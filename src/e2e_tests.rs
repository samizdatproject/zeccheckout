//! In-process end-to-end test over real HTTP: merchant API -> invoice ->
//! hosted checkout -> synthetic shielded payment in compact blocks ->
//! confirmations -> signed webhook delivered to a merchant endpoint that
//! verifies the signature.

use axum::{body::Bytes, http::HeaderMap, routing::post, Router};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

use crate::api::{router, ApiState};
use crate::db::Db;
use crate::invoice::Status;
use crate::keys::tests::{decode_ua, test_ufvk};
use crate::keys::ViewingKey;
use crate::scanner::tests::{block, orchard_action, tx};
use crate::service::{now, App, Settings};
use crate::webhook::{self, Delivery};
use zcash_protocol::consensus::Network;

const KEY: &str = "test-api-key-0123456789abcdef";
const SECRET: &str = "whsec-0123456789abcdef0123456789";

async fn spawn(app: Router) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn full_checkout_flow_over_http() {
    let vk = ViewingKey::from_ufvk(Network::TestNetwork, test_ufvk()).unwrap();
    let settings = Settings { min_confirmations: 2, merchant_name: Some("Test <Shop>".into()), ..Settings::default() };
    let app = Arc::new(App::new(Db::open(":memory:").unwrap(), vk, settings));
    let base = spawn(router(Arc::new(ApiState {
        app: app.clone(),
        api_key: KEY.into(),
        public_url: "http://pay.example".into(),
        frame_ancestors: "'self'".into(),
    })))
    .await;
    let http = reqwest::Client::new();

    // Unauthenticated / bad requests are rejected.
    let r = http.post(format!("{base}/api/v1/invoices")).json(&json!({"amount_zec": "1"})).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let r = http.post(format!("{base}/api/v1/invoices")).bearer_auth("wrong-key-wrong-key-wrong-key").json(&json!({"amount_zec": "1"})).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let r = http.post(format!("{base}/api/v1/invoices")).bearer_auth(KEY).json(&json!({"amount_zec": "1.000000001"})).send().await.unwrap();
    assert_eq!(r.status(), 400);
    let r = http.post(format!("{base}/api/v1/invoices")).bearer_auth(KEY).json(&json!({"amount_zec": "1", "amount_zat": 5})).send().await.unwrap();
    assert_eq!(r.status(), 400);

    // Create an invoice.
    let r = http
        .post(format!("{base}/api/v1/invoices"))
        .bearer_auth(KEY)
        .json(&json!({"amount_zec": "0.0025", "order_id": "ord_42", "description": "Flat <b>White</b>"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    let inv: Value = r.json().await.unwrap();
    let id = inv["id"].as_str().unwrap().to_owned();
    let address = inv["address"].as_str().unwrap().to_owned();
    assert_eq!(inv["amount_zat"], 250_000);
    assert!(address.starts_with("utest"));
    assert!(inv["payment_uri"].as_str().unwrap().starts_with(&format!("zcash:{address}?amount=0.0025")));
    assert_eq!(inv["checkout_url"], format!("http://pay.example/pay/{id}"));

    // Hosted checkout page: escaped, CSP, no external origins.
    let r = http.get(format!("{base}/pay/{id}")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let csp = r.headers()["content-security-policy"].to_str().unwrap().to_owned();
    assert!(csp.contains("default-src 'none'") && csp.contains("script-src 'self'"));
    assert_eq!(r.headers()["referrer-policy"], "no-referrer");
    let html = r.text().await.unwrap();
    assert!(html.contains("Flat &lt;b&gt;White&lt;/b&gt;"));
    assert!(html.contains("Test &lt;Shop&gt;"));
    assert!(html.contains(&address));
    assert!(html.contains("TESTNET"));
    assert!(!html.contains("http://") && !html.contains("https://"), "no absolute/external URLs in checkout page");
    let r = http.get(format!("{base}/i/{id}/qr.svg")).send().await.unwrap();
    assert_eq!(r.headers()["content-type"], "image/svg+xml");
    assert!(r.text().await.unwrap().contains("<svg"));
    for asset in ["checkout.js", "checkout.css", "widget.js"] {
        let body = http.get(format!("{base}/assets/{asset}")).send().await.unwrap().text().await.unwrap();
        assert!(!body.contains("https://") || asset == "widget.js", "{asset} references an external URL");
    }
    // Unknown / malformed ids.
    assert_eq!(http.get(format!("{base}/i/{}", "0".repeat(32))).send().await.unwrap().status(), 404);
    assert_eq!(http.get(format!("{base}/i/..%2F..%2Fetc")).send().await.unwrap().status(), 404);

    let status = |http: reqwest::Client, base: String, id: String| async move {
        http.get(format!("{base}/i/{id}")).send().await.unwrap().json::<Value>().await.unwrap()
    };
    let s = status(http.clone(), base.clone(), id.clone()).await;
    assert_eq!(s["status"], "pending");
    assert!(s.get("address").is_none(), "public status must not expose the address");

    // Payer sends 0.0025 ZEC to the invoice's Orchard receiver.
    let ua = decode_ua(&address);
    let b1 = block(1000, [0; 32], vec![]);
    app.apply_block(&b1).unwrap();
    let b2 = block(1001, b1.hash.clone().try_into().unwrap(), vec![tx(9, vec![], vec![orchard_action(ua.orchard().unwrap(), 250_000)])]);
    assert_eq!(app.apply_block(&b2).unwrap(), 1);
    app.refresh(1001, now()).unwrap();
    assert_eq!(status(http.clone(), base.clone(), id.clone()).await["status"], "detected");
    let b3 = block(1002, b2.hash.clone().try_into().unwrap(), vec![]);
    app.apply_block(&b3).unwrap();
    app.refresh(1002, now()).unwrap();
    let s = status(http.clone(), base.clone(), id.clone()).await;
    assert_eq!(s["status"], "paid");
    assert_eq!(s["confirmed_zec"], "0.0025");

    // Merchant webhook endpoint verifying signatures.
    let received: Arc<Mutex<Vec<Value>>> = Arc::default();
    let rx = received.clone();
    let hook = spawn(Router::new().route(
        "/hook",
        post(move |headers: HeaderMap, body: Bytes| {
            let rx = rx.clone();
            async move {
                let sig = headers[webhook::SIGNATURE_HEADER].to_str().unwrap();
                if webhook::verify(SECRET.as_bytes(), sig, &body, now(), 300).is_err() {
                    return axum::http::StatusCode::UNAUTHORIZED;
                }
                rx.lock().unwrap().push(serde_json::from_slice(&body).unwrap());
                axum::http::StatusCode::OK
            }
        }),
    ))
    .await;
    let delivery = Delivery::new(format!("{hook}/hook"), SECRET.as_bytes().to_vec()).unwrap();
    assert_eq!(delivery.deliver_due(&app.db, now()).await.unwrap(), 2);
    let got = received.lock().unwrap().clone();
    assert_eq!(got.iter().map(|v| v["event"].as_str().unwrap()).collect::<Vec<_>>(), ["invoice.detected", "invoice.paid"]);
    assert_eq!(got[1]["invoice"]["order_id"], "ord_42");
    assert_eq!(got[0]["seq"], 1);
    assert_eq!(got[1]["seq"], 2);
    assert_ne!(got[0]["event_id"], got[1]["event_id"]);
    assert_eq!(got[1]["invoice"]["id"], id.as_str());
    // Nothing left to deliver.
    assert_eq!(delivery.deliver_due(&app.db, now() + 10_000).await.unwrap(), 0);

    // A receiver with the wrong secret rejects; the outbox schedules a retry.
    app.db.enqueue_webhook(&id, "invoice.paid", "{}", 0).unwrap();
    let bad = Delivery::new(format!("{hook}/hook"), b"not-the-right-secret-at-all!!".to_vec()).unwrap();
    assert_eq!(bad.deliver_due(&app.db, now()).await.unwrap(), 0);
    assert!(app.db.due_webhooks(now()).unwrap().is_empty(), "retry is backed off");
    assert_eq!(app.db.due_webhooks(now() + 11).unwrap().len(), 1);

    // Merchant API read-back.
    let r: Value = http.get(format!("{base}/api/v1/invoices/{id}")).bearer_auth(KEY).send().await.unwrap().json().await.unwrap();
    assert_eq!(r["status"], Status::Paid.as_str());
    let health: Value = http.get(format!("{base}/healthz")).bearer_auth(KEY).send().await.unwrap().json().await.unwrap();
    assert_eq!(health["webhooks_delivered"], 2);
    // Without the key, /healthz reveals nothing about sales volume.
    let public: Value = http.get(format!("{base}/healthz")).send().await.unwrap().json().await.unwrap();
    assert!(public.get("webhooks_delivered").is_none() && public.get("chain_tip").is_none());

    // The checkout only pins postMessage to an allow-listed embedder.
    let html = http.get(format!("{base}/pay/{id}?embed=1&parent=https%3A%2F%2Fevil.example")).send().await.unwrap().text().await.unwrap();
    assert!(html.contains(r#"data-parent-origin="""#));
}

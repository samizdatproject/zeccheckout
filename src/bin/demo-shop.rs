//! A tiny demo merchant ("Shielded Coffee") showing how a shop integrates
//! ZecCheckout:
//!
//! 1. Server-side: create an invoice through the merchant API (bearer key).
//! 2. Browser: embed the checkout widget served by the merchant's own
//!    ZecCheckout instance (no third-party scripts).
//! 3. Server-side: receive the HMAC-signed `invoice.paid` webhook, verify it,
//!    and fulfil the order. The shop never trusts the browser for payment
//!    status.
//!
//! Env: SHOP_BIND (127.0.0.1:8081), SHOP_CHECKOUT_URL (http://127.0.0.1:8080),
//!      SHOP_API_KEY, SHOP_WEBHOOK_SECRET, SHOP_PUBLIC_URL (public origin behind a
//!      TLS proxy; defaults to RENDER_EXTERNAL_URL on Render).

use anyhow::{anyhow, Context, Result};
use axum::{
    body::Bytes,
    extract::{Form, Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use zeccheckout::api::{html_escape, render};
use zeccheckout::service::now;
use zeccheckout::webhook;

struct Product {
    id: &'static str,
    name: &'static str,
    price_zec: &'static str,
    blurb: &'static str,
}

const PRODUCTS: &[Product] = &[
    Product { id: "espresso", name: "Shielded Espresso", price_zec: "0.001", blurb: "Small, strong, unlinkable." },
    Product { id: "flatwhite", name: "Orchard Flat White", price_zec: "0.0025", blurb: "Smooth. Zero-knowledge foam." },
    Product { id: "beans", name: "Bag of Sapling Beans", price_zec: "0.01", blurb: "250 g, whole bean, roasted privately." },
];

#[derive(Clone)]
struct Order {
    product: &'static Product,
    invoice_id: String,
    status: String,
    fulfilled: bool,
    last_seq: u64,
}

struct Shop {
    orders: Mutex<HashMap<String, Order>>,
    checkout_url: String,
    /// Public origin(s) of this shop as browsers see it (behind a TLS proxy the
    /// Host header is not enough), from SHOP_PUBLIC_URL / RENDER_EXTERNAL_URL.
    public_origins: Vec<String>,
    api_key: String,
    webhook_secret: String,
    http: reqwest::Client,
}

type St = State<Arc<Shop>>;

fn env(k: &str, d: Option<&str>) -> Result<String> {
    match std::env::var(k).ok().filter(|v| !v.is_empty()) {
        Some(v) => Ok(v),
        None => d.map(str::to_owned).ok_or_else(|| anyhow!("set {k}")),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let bind = env("SHOP_BIND", Some("127.0.0.1:8081"))?;
    let shop = Arc::new(Shop {
        orders: Mutex::new(HashMap::new()),
        checkout_url: env("SHOP_CHECKOUT_URL", Some("http://127.0.0.1:8080"))?.trim_end_matches('/').to_owned(),
        public_origins: ["SHOP_PUBLIC_URL", "RENDER_EXTERNAL_URL"]
            .iter()
            .filter_map(|k| std::env::var(k).ok())
            .map(|v| v.trim().trim_end_matches('/').to_owned())
            .filter(|v| !v.is_empty())
            .collect(),
        api_key: env("SHOP_API_KEY", None)?,
        webhook_secret: env("SHOP_WEBHOOK_SECRET", None)?,
        http: reqwest::Client::new(),
    });
    let csp = format!(
        "default-src 'none'; script-src 'self' {c}; frame-src {c}; style-src 'self'; img-src 'self'; connect-src 'self'; form-action 'self'; base-uri 'none'",
        c = shop.checkout_url
    );
    let app = Router::new()
        .route("/", get(index))
        .route("/buy", post(buy))
        .route("/order/{id}", get(order_page))
        .route("/order/{id}/status", get(order_status))
        .route("/webhook", post(webhook_rx))
        .route("/shop.css", get(|| async { ([(header::CONTENT_TYPE, "text/css")], include_str!("../../static/shop.css")) }))
        .route("/shop.js", get(|| async { ([(header::CONTENT_TYPE, "text/javascript")], include_str!("../../static/shop.js")) }))
        .layer(axum::middleware::map_response(move |mut res: Response| {
            let csp = csp.clone();
            async move {
                if let Ok(v) = HeaderValue::from_str(&csp) {
                    res.headers_mut().insert(header::CONTENT_SECURITY_POLICY, v);
                }
                res.headers_mut().insert(header::REFERRER_POLICY, HeaderValue::from_static("same-origin"));
                res
            }
        }))
        .with_state(shop);
    let listener = tokio::net::TcpListener::bind(&bind).await.with_context(|| format!("binding {bind}"))?;
    tracing::info!("demo shop on http://{bind}");
    axum::serve(listener, app).await?;
    Ok(())
}

fn page(title: &str, body: String) -> Html<String> {
    Html(format!(
        "<!doctype html><html lang=en><head><meta charset=utf-8><meta name=viewport content='width=device-width,initial-scale=1'>\
         <title>{t} &middot; Shielded Coffee</title><link rel=stylesheet href=/shop.css></head><body>\
         <nav><a href=/>&#9749; Shielded Coffee</a><span>demo shop &middot; pays with ZecCheckout</span></nav><main>{body}</main></body></html>",
        t = html_escape(title)
    ))
}

async fn index(State(shop): St) -> Html<String> {
    // Wake the checkout service now (free hosts such as Render spin idle
    // services down), so it is ready by the time the visitor clicks "Buy".
    let warm = shop.http.get(format!("{}/healthz", shop.checkout_url));
    tokio::spawn(async move {
        let _ = warm.send().await;
    });
    let items: String = PRODUCTS
        .iter()
        .map(|p| {
            format!(
                "<form class=product method=post action=/buy><h2>{}</h2><p>{}</p><div class=price>{} ZEC</div>\
                 <input type=hidden name=product value='{}'><button>Buy with shielded ZEC</button></form>",
                p.name, p.blurb, p.price_zec, p.id
            )
        })
        .collect();
    page("Menu", format!("<h1>Menu</h1><div class=grid>{items}</div>"))
}

#[derive(Deserialize)]
struct BuyForm {
    product: String,
}

fn new_order_id() -> String {
    use rand::RngCore;
    let mut b = [0u8; 8];
    rand::rngs::OsRng.fill_bytes(&mut b);
    format!("ord_{}", hex::encode(b))
}

async fn buy(State(shop): St, headers: HeaderMap, Form(f): Form<BuyForm>) -> Response {
    // Minimal CSRF defence: browsers send Origin on cross-site POSTs.
    let h = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    if !csrf_ok(h("origin"), h("sec-fetch-site"), h("host").unwrap_or_default(), &shop.public_origins) {
        return (StatusCode::FORBIDDEN, "cross-origin request refused").into_response();
    }
    let Some(product) = PRODUCTS.iter().find(|p| p.id == f.product) else {
        return (StatusCode::BAD_REQUEST, "unknown product").into_response();
    };
    let order_id = new_order_id();
    // Server-to-server call; the API key never reaches the browser.
    let res = shop
        .http
        .post(format!("{}/api/v1/invoices", shop.checkout_url))
        .bearer_auth(&shop.api_key)
        .json(&json!({
            "amount_zec": product.price_zec,
            "order_id": order_id,
            "description": product.name,
            "expires_in_secs": 1800
        }))
        .send()
        .await;
    let inv: serde_json::Value = match res {
        Ok(r) if r.status().is_success() => match r.json().await {
            Ok(v) => v,
            Err(_) => return checkout_waking(),
        },
        _ => return checkout_waking(),
    };
    let invoice_id = inv["id"].as_str().unwrap_or_default().to_owned();
    shop.orders.lock().unwrap().insert(
        order_id.clone(),
        Order { product, invoice_id, status: "pending".into(), fulfilled: false, last_seq: 0 },
    );
    tracing::info!(order = %order_id, product = product.id, "order created");
    Redirect::to(&format!("/order/{order_id}")).into_response()
}

/// CSRF check for the shop's form POST.
/// - `Sec-Fetch-Site` (set by browsers, not settable by page scripts) decides
///   when present: only `same-origin` passes.
/// - Without it (older browsers, curl): an `Origin` header must be this shop;
///   `Origin: null` is refused; no Origin at all (non-browser client) passes.
fn csrf_ok(origin: Option<&str>, sec_fetch_site: Option<&str>, host: &str, public_origins: &[String]) -> bool {
    match (sec_fetch_site, origin) {
        (Some(site), _) => site.eq_ignore_ascii_case("same-origin"),
        (None, Some(o)) => same_origin(o, host, public_origins),
        (None, None) => true,
    }
}

/// The request's Origin must be this shop. Behind a TLS
/// proxy (e.g. Render) the Host header may not be the public host, so the
/// configured public origins are accepted too.
fn same_origin(origin: &str, host: &str, public_origins: &[String]) -> bool {
    public_origins.iter().any(|o| o == origin)
        || (!host.is_empty() && (origin == format!("http://{host}") || origin == format!("https://{host}")))
}

/// Shown when the checkout service is unreachable or answers with something
/// other than an invoice (e.g. a host's "service is starting" page).
fn checkout_waking() -> Response {
    let body = "<h1>Checkout is starting up</h1><p>The checkout service is waking up (free hosting sleeps when idle). \
                Please go back and try again in a minute.</p><p><a href=/>Back to the menu</a></p>";
    (StatusCode::SERVICE_UNAVAILABLE, page("Checkout starting", body.into())).into_response()
}

async fn order_page(State(shop): St, Path(id): Path<String>) -> Response {
    let Some(o) = shop.orders.lock().unwrap().get(&id).cloned() else {
        return (StatusCode::NOT_FOUND, page("Not found", "<p>Order not found.</p>".into())).into_response();
    };
    let body = render(
        "<h1>Order {{oid}}</h1><div class=order>\
         <div><h2>{{name}}</h2><p class=price>{{price}} ZEC</p>\
         <div id=fulfil class=fulfil data-order='{{oid}}'><b>Order status:</b> <span id=fulfil-text>awaiting payment</span></div>\
         <p class=small>This panel is driven only by the <em>signed webhook</em> the shop backend receives, not by the browser.</p></div>\
         <div data-zeccheckout-invoice='{{inv}}'></div></div>\
         <script src='{{checkout}}/assets/widget.js'></script><script src=/shop.js></script>",
        &[
            ("oid", html_escape(&id)),
            ("name", html_escape(o.product.name)),
            ("price", o.product.price_zec.to_owned()),
            ("inv", html_escape(&o.invoice_id)),
            ("checkout", html_escape(&shop.checkout_url)),
        ],
    );
    page("Checkout", body).into_response()
}

async fn order_status(State(shop): St, Path(id): Path<String>) -> Response {
    match shop.orders.lock().unwrap().get(&id) {
        Some(o) => Json(json!({ "status": o.status, "fulfilled": o.fulfilled })).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn webhook_rx(State(shop): St, headers: HeaderMap, body: Bytes) -> Response {
    let sig = headers
        .get(webhook::SIGNATURE_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if let Err(e) = webhook::verify(shop.webhook_secret.as_bytes(), sig, &body, now(), 300) {
        tracing::warn!("rejected webhook: {e}");
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let order_id = v["invoice"]["order_id"].as_str().unwrap_or_default();
    let invoice_id = v["invoice"]["id"].as_str().unwrap_or_default();
    let status = v["invoice"]["status"].as_str().unwrap_or_default().to_owned();
    let seq = v["seq"].as_u64().unwrap_or(0);
    let mut orders = shop.orders.lock().unwrap();
    match orders.get_mut(order_id) {
        // Cross-check the invoice id too, so a webhook can't fulfil the wrong order.
        Some(o) if o.invoice_id == invoice_id => {
            if seq <= o.last_seq {
                // Duplicate or out-of-order delivery: acknowledge, don't apply.
                return StatusCode::NO_CONTENT.into_response();
            }
            o.last_seq = seq;
            o.status = status.clone();
            if status == "paid" && !o.fulfilled {
                o.fulfilled = true;
                tracing::info!(order = order_id, "signature verified; payment confirmed; order fulfilled");
            }
            StatusCode::NO_CONTENT.into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::{csrf_ok, same_origin};

    #[test]
    fn csrf_uses_sec_fetch_site_and_handles_origin_null() {
        let public = vec!["https://zeccheckout-shop.onrender.com".to_owned()];
        let host = "10.0.0.7:10000";
        // Chrome form POST under a no-referrer policy sends Origin: null.
        assert!(csrf_ok(Some("null"), Some("same-origin"), host, &public));
        assert!(!csrf_ok(Some("null"), Some("cross-site"), host, &public));
        assert!(!csrf_ok(Some("null"), Some("same-site"), host, &public));
        assert!(!csrf_ok(Some("null"), None, host, &public));
        // Sec-Fetch-Site wins over a matching Origin.
        assert!(!csrf_ok(Some("https://zeccheckout-shop.onrender.com"), Some("cross-site"), host, &public));
        assert!(csrf_ok(Some("https://zeccheckout-shop.onrender.com"), Some("same-origin"), host, &public));
        // Older browsers: Origin only.
        assert!(csrf_ok(Some("https://zeccheckout-shop.onrender.com"), None, host, &public));
        assert!(!csrf_ok(Some("https://evil.example"), None, host, &public));
        // Non-browser clients (curl, server-to-server) send neither.
        assert!(csrf_ok(None, None, host, &public));
    }

    #[test]
    fn origin_check_accepts_public_url_behind_proxy() {
        let public = vec!["https://zeccheckout-shop.onrender.com".to_owned()];
        // Behind Render's proxy the Host header is not the public host.
        assert!(same_origin("https://zeccheckout-shop.onrender.com", "10.0.0.7:10000", &public));
        // Local use: Origin matches Host.
        assert!(same_origin("http://127.0.0.1:8081", "127.0.0.1:8081", &[]));
        // Cross-site posts are refused.
        assert!(!same_origin("https://evil.example", "10.0.0.7:10000", &public));
        assert!(!same_origin("https://zeccheckout-shop.onrender.com.evil.example", "x", &public));
        assert!(!same_origin("null", "", &[]));
    }
}

//! HTTP API and hosted checkout page.
//!
//! - Merchant API (`/api/v1/...`) requires `Authorization: Bearer <api key>`.
//! - Public endpoints (`/pay/{id}`, `/i/{id}`, `/i/{id}/qr.svg`) are keyed by
//!   the unguessable 128-bit invoice id.
//! - Every asset is served from this process. No CDNs, fonts, analytics or
//!   third-party scripts; a strict CSP enforces that in the browser.
//! - No request logging at all, so client IPs and user agents are never
//!   recorded.

use axum::{
    extract::{Path, Query, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

use crate::invoice::{self, format_zec, is_valid_invoice_id, Status};
use crate::service::{now, App, CreateInvoice};

pub struct ApiState {
    pub app: Arc<App>,
    pub api_key: String,
    pub public_url: String,
    pub frame_ancestors: String,
}

type St = State<Arc<ApiState>>;

pub fn router(state: Arc<ApiState>) -> Router {
    let csp_state = state.clone();
    Router::new()
        .route("/api/v1/invoices", post(create_invoice).get(list_invoices))
        .route("/api/v1/invoices/{id}", get(get_invoice))
        .route("/pay/{id}", get(checkout_page))
        .route("/i/{id}", get(public_status))
        .route("/i/{id}/qr.svg", get(qr_svg))
        .route("/assets/checkout.js", get(|| asset("text/javascript", include_str!("../static/checkout.js"))))
        .route("/assets/checkout.css", get(|| asset("text/css", include_str!("../static/checkout.css"))))
        .route("/assets/widget.js", get(widget_js))
        .route("/healthz", get(health))
        .layer(middleware::from_fn(move |req, next| {
            security_headers(csp_state.clone(), req, next)
        }))
        .with_state(state)
}

async fn security_headers(st: Arc<ApiState>, req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    let csp = format!(
        "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; \
         base-uri 'none'; form-action 'none'; frame-ancestors {}",
        st.frame_ancestors
    );
    if let Ok(v) = HeaderValue::from_str(&csp) {
        h.insert(header::CONTENT_SECURITY_POLICY, v);
    }
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    // The widget script is meant to be embedded by merchant sites on other
    // origins; everything else is only ever loaded by our own pages.
    if !h.contains_key("cross-origin-resource-policy") {
        h.insert("cross-origin-resource-policy", HeaderValue::from_static("same-origin"));
    }
    h.insert("permissions-policy", HeaderValue::from_static("camera=(), microphone=(), geolocation=(), interest-cohort=()"));
    if !h.contains_key(header::CACHE_CONTROL) {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    res
}

async fn asset(ct: &'static str, body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, ct),
            (header::CACHE_CONTROL, "public, max-age=300"),
        ],
        body,
    )
        .into_response()
}

async fn widget_js() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/javascript"),
            (header::CACHE_CONTROL, "public, max-age=300"),
            (header::HeaderName::from_static("cross-origin-resource-policy"), "cross-origin"),
        ],
        include_str!("../static/widget.js"),
    )
        .into_response()
}

fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({ "error": msg }))).into_response()
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn authorized(st: &ApiState, headers: &HeaderMap) -> bool {
    let Some(v) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Some(token) = v.strip_prefix("Bearer ") else {
        return false;
    };
    ct_eq(token.trim().as_bytes(), st.api_key.as_bytes())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateReq {
    /// Decimal ZEC string, e.g. "0.015". Exactly one of amount_zec / amount_zat.
    amount_zec: Option<String>,
    amount_zat: Option<u64>,
    order_id: Option<String>,
    description: Option<String>,
    expires_in_secs: Option<u64>,
}

fn invoice_json(st: &ApiState, inv: &crate::db::InvoiceRow) -> serde_json::Value {
    json!({
        "id": inv.id,
        "order_id": inv.order_id,
        "description": inv.description,
        "status": inv.status.as_str(),
        "amount_zat": inv.amount_zat,
        "amount_zec": format_zec(inv.amount_zat),
        "received_zat": inv.received_zat,
        "confirmed_zat": inv.confirmed_zat,
        "address": inv.address,
        "payment_uri": inv.uri,
        "checkout_url": format!("{}/pay/{}", st.public_url, inv.id),
        "created_at": inv.created_at,
        "expires_at": inv.expires_at,
        "paid_at": inv.paid_at,
        "min_confirmations": st.app.settings.min_confirmations,
    })
}

async fn create_invoice(State(st): St, headers: HeaderMap, body: Option<Json<CreateReq>>) -> Response {
    if !authorized(&st, &headers) {
        return err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    let Some(Json(req)) = body else {
        return err(StatusCode::BAD_REQUEST, "invalid JSON body");
    };
    let amount = match (req.amount_zec.as_deref(), req.amount_zat) {
        (Some(s), None) => match invoice::parse_zec(s) {
            Ok(v) => v,
            Err(e) => return err(StatusCode::BAD_REQUEST, &e.to_string()),
        },
        (None, Some(z)) if z > 0 && z <= invoice::MAX_ZAT => z,
        _ => return err(StatusCode::BAD_REQUEST, "provide exactly one of amount_zec or amount_zat (> 0)"),
    };
    let app = st.app.clone();
    let res = tokio::task::spawn_blocking(move || {
        app.create_invoice(
            CreateInvoice {
                amount_zat: amount,
                order_id: req.order_id,
                description: req.description,
                expires_in_secs: req.expires_in_secs,
            },
            now(),
        )
    })
    .await;
    match res {
        Ok(Ok(inv)) => (StatusCode::CREATED, Json(invoice_json(&st, &inv))).into_response(),
        Ok(Err(e)) => err(StatusCode::BAD_REQUEST, &e.to_string()),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    }
}

#[derive(Deserialize)]
struct ListQ {
    limit: Option<u32>,
}

async fn list_invoices(State(st): St, headers: HeaderMap, Query(q): Query<ListQ>) -> Response {
    if !authorized(&st, &headers) {
        return err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    match st.app.db.list_invoices(q.limit.unwrap_or(50).min(500)) {
        Ok(v) => Json(json!({ "invoices": v.iter().map(|i| invoice_json(&st, i)).collect::<Vec<_>>() })).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    }
}

async fn get_invoice(State(st): St, headers: HeaderMap, Path(id): Path<String>) -> Response {
    if !authorized(&st, &headers) {
        return err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    match load(&st, &id) {
        Ok(inv) => Json(invoice_json(&st, &inv)).into_response(),
        Err(r) => r,
    }
}

#[allow(clippy::result_large_err)]
fn load(st: &ApiState, id: &str) -> Result<crate::db::InvoiceRow, Response> {
    if !is_valid_invoice_id(id) {
        return Err(err(StatusCode::NOT_FOUND, "not found"));
    }
    match st.app.db.get_invoice(id) {
        Ok(Some(inv)) => Ok(inv),
        Ok(None) => Err(err(StatusCode::NOT_FOUND, "not found")),
        Err(_) => Err(err(StatusCode::INTERNAL_SERVER_ERROR, "internal error")),
    }
}

/// Minimal public status for the checkout page / widget. No address, txid or
/// order data.
async fn public_status(State(st): St, Path(id): Path<String>) -> Response {
    let inv = match load(&st, &id) {
        Ok(i) => i,
        Err(r) => return r,
    };
    let now = now();
    // Expiry is also re-evaluated by the scanner; reflect it immediately here.
    let status = if inv.status == Status::Pending && now >= inv.expires_at {
        Status::Expired
    } else {
        inv.status
    };
    Json(json!({
        "id": inv.id,
        "status": status.as_str(),
        "amount_zec": format_zec(inv.amount_zat),
        "received_zec": format_zec(inv.received_zat),
        "confirmed_zec": format_zec(inv.confirmed_zat),
        "seen_in_mempool_zec": format_zec(st.app.mempool_seen_for(&inv.id)),
        "min_confirmations": st.app.settings.min_confirmations,
        "expires_at": inv.expires_at,
        "now": now,
    }))
    .into_response()
}

async fn qr_svg(State(st): St, Path(id): Path<String>) -> Response {
    let inv = match load(&st, &id) {
        Ok(i) => i,
        Err(r) => return r,
    };
    match qrcode::QrCode::with_error_correction_level(inv.uri.as_bytes(), qrcode::EcLevel::M) {
        Ok(code) => {
            let svg = code
                .render::<qrcode::render::svg::Color>()
                .min_dimensions(280, 280)
                .quiet_zone(true)
                .build();
            ([(header::CONTENT_TYPE, "image/svg+xml")], svg).into_response()
        }
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "qr encoding failed"),
    }
}

pub fn html_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            _ => o.push(c),
        }
    }
    o
}

/// Single-pass `{{key}}` substitution, so substituted values can never be
/// re-interpreted as placeholders.
pub fn render(tpl: &str, vars: &[(&str, String)]) -> String {
    let mut out = String::with_capacity(tpl.len() + 512);
    let mut rest = tpl;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                let key = &after[..end];
                match vars.iter().find(|(k, _)| *k == key) {
                    Some((_, v)) => out.push_str(v),
                    None => {
                        out.push_str("{{");
                        out.push_str(key);
                        out.push_str("}}");
                    }
                }
                rest = &after[end + 2..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

#[derive(Deserialize)]
struct PayQ {
    embed: Option<u8>,
}

async fn checkout_page(State(st): St, Path(id): Path<String>, Query(q): Query<PayQ>) -> Response {
    let inv = match load(&st, &id) {
        Ok(i) => i,
        Err(_) => return (StatusCode::NOT_FOUND, Html("<!doctype html><title>Not found</title><p>Invoice not found.</p>".to_string())).into_response(),
    };
    let merchant = st.app.settings.merchant_name.clone().unwrap_or_else(|| "ZecCheckout".into());
    let desc = inv.description.clone().unwrap_or_default();
    let network = st.app.vk.network_name();
    let embed = if q.embed == Some(1) { "embed" } else { "" };
    let banner = if network == "testnet" {
        "<p class=\"testnet\">TESTNET &mdash; pay with testnet ZEC (TAZ) only</p>"
    } else {
        ""
    };
    let html = render(
        include_str!("../static/checkout.html"),
        &[
            ("embed_class", embed.to_string()),
            ("merchant", html_escape(&merchant)),
            ("description", html_escape(&desc)),
            ("amount_zec", format_zec(inv.amount_zat)),
            ("address", html_escape(&inv.address)),
            ("uri", html_escape(&inv.uri)),
            ("id", html_escape(&inv.id)),
            ("network", network.to_string()),
            ("testnet_banner", banner.to_string()),
            ("min_conf", st.app.settings.min_confirmations.to_string()),
        ],
    );
    Html(html).into_response()
}

async fn health(State(st): St) -> Response {
    let scanned = st.app.db.get_meta_u64("scan_height").ok().flatten();
    let tip = st.app.tip();
    let (delivered, pending) = st.app.db.webhook_counts().unwrap_or((0, 0));
    Json(json!({
        "ok": true,
        "network": st.app.vk.network_name(),
        "chain_tip": tip,
        "scanned_height": scanned,
        "synced": matches!((tip, scanned), (Some(t), Some(s)) if s >= t),
        "webhooks_delivered": delivered,
        "webhooks_pending": pending,
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping() {
        assert_eq!(html_escape("<a href=\"x\">'&'</a>"), "&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;");
    }

    #[test]
    fn render_is_single_pass() {
        let out = render("a {{x}} b {{y}} {{z}}", &[("x", "{{y}}".into()), ("y", "Y".into())]);
        assert_eq!(out, "a {{y}} b Y {{z}}");
    }

    #[test]
    fn constant_time_eq() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"abcd"));
    }
}

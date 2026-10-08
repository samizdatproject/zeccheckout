//! HMAC-SHA256 signed webhooks with a persistent outbox and exponential
//! backoff.
//!
//! Header format (similar in spirit to common payment processors):
//!
//! ```text
//! ZecCheckout-Signature: t=<unix seconds>,v1=<hex(HMAC_SHA256(secret, "<t>.<raw body>"))>
//! ```
//!
//! Receivers must verify the signature over the *raw* body bytes and reject
//! timestamps outside a tolerance window to prevent replay.

use anyhow::{anyhow, Result};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::time::Duration;

use crate::db::Db;

pub const SIGNATURE_HEADER: &str = "ZecCheckout-Signature";
const MAX_ATTEMPTS: u32 = 12;

type HmacSha256 = Hmac<Sha256>;

pub fn sign(secret: &[u8], timestamp: u64, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

pub fn signature_header(secret: &[u8], timestamp: u64, body: &[u8]) -> String {
    format!("t={timestamp},v1={}", sign(secret, timestamp, body))
}

/// Verifies a `ZecCheckout-Signature` header value. Constant-time comparison
/// of the MAC; rejects timestamps more than `tolerance_secs` away from `now`.
pub fn verify(secret: &[u8], header: &str, body: &[u8], now: u64, tolerance_secs: u64) -> Result<()> {
    let mut ts = None;
    let mut sigs = Vec::new();
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", v)) => ts = v.parse::<u64>().ok(),
            Some(("v1", v)) => sigs.push(v.to_owned()),
            _ => {}
        }
    }
    let ts = ts.ok_or_else(|| anyhow!("missing timestamp"))?;
    if ts.abs_diff(now) > tolerance_secs {
        return Err(anyhow!("timestamp outside tolerance"));
    }
    for s in sigs {
        let Ok(raw) = hex::decode(&s) else { continue };
        let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
        mac.update(ts.to_string().as_bytes());
        mac.update(b".");
        mac.update(body);
        if mac.verify_slice(&raw).is_ok() {
            return Ok(());
        }
    }
    Err(anyhow!("signature mismatch"))
}

/// Backoff schedule: 10s, 20s, 40s ... capped at one hour.
pub fn backoff_secs(attempts: u32) -> u64 {
    (10u64 << attempts.min(12)).min(3600)
}

pub struct Delivery {
    pub url: String,
    pub secret: Vec<u8>,
    pub client: reqwest::Client,
}

impl Delivery {
    pub fn new(url: String, secret: Vec<u8>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("zeccheckout-webhook")
            .build()?;
        Ok(Self { url, secret, client })
    }

    /// Attempts every due webhook once. Returns the number delivered.
    pub async fn deliver_due(&self, db: &Db, now: u64) -> Result<usize> {
        let mut delivered = 0;
        for row in db.due_webhooks(now)? {
            let body = row.body.into_bytes();
            let header = signature_header(&self.secret, now, &body);
            let res = self
                .client
                .post(&self.url)
                .header("content-type", "application/json")
                .header(SIGNATURE_HEADER, header)
                .body(body)
                .send()
                .await;
            match res {
                Ok(r) if r.status().is_success() => {
                    db.mark_webhook_delivered(row.id, now)?;
                    delivered += 1;
                }
                other => {
                    let attempts = row.attempts + 1;
                    let give_up = attempts >= MAX_ATTEMPTS;
                    let why = match other {
                        Ok(r) => format!("HTTP {}", r.status().as_u16()),
                        Err(e) if e.is_timeout() => "timeout".into(),
                        Err(_) => "connection error".into(),
                    };
                    tracing::warn!(outbox_id = row.id, attempts, give_up, "webhook delivery failed: {why}");
                    db.mark_webhook_retry(row.id, now + backoff_secs(row.attempts), give_up)?;
                }
            }
        }
        Ok(delivered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_is_deterministic_and_known_answer() {
        // Known-answer test computed independently:
        // python3 -c "import hmac,hashlib;print(hmac.new(b'whsec_test',b'1700000000.{\"a\":1}',hashlib.sha256).hexdigest())"
        let sig = sign(b"whsec_test", 1_700_000_000, br#"{"a":1}"#);
        assert_eq!(sig, KAT);
        assert_eq!(sig.len(), 64);
    }

    const KAT: &str = include_str!("../tests/data/hmac_kat.txt").trim_ascii();

    #[test]
    fn verify_accepts_valid_and_rejects_tampering() {
        let body = br#"{"event":"invoice.paid"}"#;
        let h = signature_header(b"s3cret", 1000, body);
        assert!(verify(b"s3cret", &h, body, 1000, 300).is_ok());
        assert!(verify(b"s3cret", &h, body, 1299, 300).is_ok());
        // replay outside tolerance
        assert!(verify(b"s3cret", &h, body, 1301, 300).is_err());
        // wrong secret
        assert!(verify(b"other", &h, body, 1000, 300).is_err());
        // tampered body
        assert!(verify(b"s3cret", &h, br#"{"event":"invoice.paid "}"#, 1000, 300).is_err());
        // tampered timestamp
        let forged = h.replace("t=1000", "t=1001");
        assert!(verify(b"s3cret", &forged, body, 1000, 300).is_err());
        // garbage
        assert!(verify(b"s3cret", "nonsense", body, 1000, 300).is_err());
    }

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff_secs(0), 10);
        assert_eq!(backoff_secs(1), 20);
        assert_eq!(backoff_secs(3), 80);
        assert_eq!(backoff_secs(20), 3600);
    }
}

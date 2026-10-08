//! Invoice model, amount parsing, ZIP-321 URI construction and the payment
//! state machine (pure functions, no I/O).

use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use std::str::FromStr;
use zcash_address::ZcashAddress;
use zcash_protocol::memo::{Memo, MemoBytes};
use zcash_protocol::value::Zatoshis;
use zip321::{Payment, TransactionRequest};

pub const ZAT_PER_ZEC: u64 = 100_000_000;
/// Total ZEC supply cap, used as an upper bound for invoice amounts.
pub const MAX_ZAT: u64 = 21_000_000 * ZAT_PER_ZEC;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Waiting for payment; nothing seen on chain yet.
    Pending,
    /// Some funds arrived but less than the invoice amount.
    Underpaid,
    /// The full amount has been mined but has fewer than N confirmations.
    Detected,
    /// The full amount has at least N confirmations. Final.
    Paid,
    /// Expired with nothing received.
    Expired,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Underpaid => "underpaid",
            Status::Detected => "detected",
            Status::Paid => "paid",
            Status::Expired => "expired",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "pending" => Status::Pending,
            "underpaid" => Status::Underpaid,
            "detected" => Status::Detected,
            "paid" => Status::Paid,
            "expired" => Status::Expired,
            other => bail!("unknown status {other}"),
        })
    }
}

/// Parses a decimal ZEC amount such as "0.015" into zatoshis without using
/// floating point. At most 8 decimal places; must be > 0 and <= MAX_ZAT.
pub fn parse_zec(s: &str) -> Result<u64> {
    let s = s.trim();
    if s.is_empty() || s.starts_with('-') || s.starts_with('+') {
        bail!("invalid amount");
    }
    let (int, frac) = match s.split_once('.') {
        Some((i, f)) => (i, f),
        None => (s, ""),
    };
    if (int.is_empty() && frac.is_empty())
        || !int.chars().all(|c| c.is_ascii_digit())
        || !frac.chars().all(|c| c.is_ascii_digit())
    {
        bail!("invalid amount");
    }
    if frac.len() > 8 {
        bail!("too many decimal places (max 8)");
    }
    if int.len() > 9 {
        bail!("amount too large");
    }
    let int: u64 = if int.is_empty() { 0 } else { int.parse()? };
    let frac_val: u64 = if frac.is_empty() {
        0
    } else {
        format!("{frac:0<8}").parse()?
    };
    let zat = int
        .checked_mul(ZAT_PER_ZEC)
        .and_then(|v| v.checked_add(frac_val))
        .ok_or_else(|| anyhow!("amount overflow"))?;
    if zat == 0 {
        bail!("amount must be greater than zero");
    }
    if zat > MAX_ZAT {
        bail!("amount exceeds total supply");
    }
    Ok(zat)
}

/// Formats zatoshis as a decimal ZEC string with trailing zeros trimmed.
pub fn format_zec(zat: u64) -> String {
    let int = zat / ZAT_PER_ZEC;
    let frac = zat % ZAT_PER_ZEC;
    if frac == 0 {
        return int.to_string();
    }
    let f = format!("{frac:08}");
    format!("{int}.{}", f.trim_end_matches('0'))
}

/// Builds a ZIP-321 payment request URI for a single shielded payment.
pub fn zip321_uri(
    address: &str,
    amount_zat: u64,
    memo: Option<&str>,
    label: Option<&str>,
) -> Result<String> {
    let addr = ZcashAddress::try_from_encoded(address)
        .map_err(|e| anyhow!("invalid address: {e}"))?;
    if !addr.can_receive_memo() {
        // Defence in depth: we only ever hand out shielded addresses.
        bail!("refusing to build a payment request for a transparent-only address");
    }
    let amount = Zatoshis::from_u64(amount_zat).map_err(|_| anyhow!("invalid amount"))?;
    let memo = match memo {
        Some(m) => Some(MemoBytes::from(
            Memo::from_str(m).map_err(|e| anyhow!("memo: {e:?}"))?,
        )),
        None => None,
    };
    let payment = Payment::new(
        addr,
        Some(amount),
        memo,
        label.map(str::to_owned),
        None,
        vec![],
    )
    .map_err(|e| anyhow!("payment: {e:?}"))?;
    let req = TransactionRequest::new(vec![payment]).map_err(|e| anyhow!("zip321: {e:?}"))?;
    Ok(req.to_uri())
}

/// Number of confirmations of a transaction mined at `height` given chain tip `tip`.
/// A transaction in the tip block has 1 confirmation.
pub fn confirmations(tip: u64, height: u64) -> u64 {
    if height > tip {
        0
    } else {
        tip - height + 1
    }
}

/// A received shielded output credited to an invoice.
#[derive(Clone, Copy, Debug)]
pub struct Received {
    pub value_zat: u64,
    pub height: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Evaluation {
    pub status: Status,
    /// All mined value credited to the invoice.
    pub received_zat: u64,
    /// Value with at least `min_conf` confirmations.
    pub confirmed_zat: u64,
    /// Height at which the confirmed sum first reached the amount (if paid).
    pub paid_height: Option<u64>,
}

/// Pure state machine. `current` is the stored status, so that `Paid` is
/// sticky (final) even if a later reorg removes outputs.
pub fn evaluate(
    current: Status,
    amount_zat: u64,
    expires_at: u64,
    now: u64,
    received: &[Received],
    tip: u64,
    min_conf: u64,
) -> Evaluation {
    let min_conf = min_conf.max(1);
    let received_zat: u64 = received.iter().map(|r| r.value_zat).sum();
    let mut confirmed: Vec<Received> = received
        .iter()
        .copied()
        .filter(|r| confirmations(tip, r.height) >= min_conf)
        .collect();
    let confirmed_zat: u64 = confirmed.iter().map(|r| r.value_zat).sum();

    // Height at which the cumulative confirmed value first covers the amount.
    confirmed.sort_by_key(|r| r.height);
    let mut acc = 0u64;
    let mut paid_height = None;
    for r in &confirmed {
        acc += r.value_zat;
        if acc >= amount_zat {
            paid_height = Some(r.height);
            break;
        }
    }

    let status = if current == Status::Paid || confirmed_zat >= amount_zat {
        Status::Paid
    } else if received_zat >= amount_zat {
        Status::Detected
    } else if received_zat > 0 {
        Status::Underpaid
    } else if now >= expires_at {
        Status::Expired
    } else {
        Status::Pending
    };
    Evaluation {
        status,
        received_zat,
        confirmed_zat,
        paid_height,
    }
}

/// Random 128-bit invoice identifier, hex encoded. Invoice ids double as the
/// capability for the public status endpoint, so they must be unguessable.
pub fn new_invoice_id() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut b);
    hex::encode(b)
}

pub fn is_valid_invoice_id(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{tests::test_ufvk, ViewingKey};
    use zcash_protocol::consensus::Network;

    #[test]
    fn parse_and_format_amounts() {
        assert_eq!(parse_zec("1").unwrap(), 100_000_000);
        assert_eq!(parse_zec("0.015").unwrap(), 1_500_000);
        assert_eq!(parse_zec(".5").unwrap(), 50_000_000);
        assert_eq!(parse_zec("0.00000001").unwrap(), 1);
        assert_eq!(parse_zec("21000000").unwrap(), MAX_ZAT);
        for bad in ["", "0", "0.0", "-1", "1.000000001", "abc", "1.2.3", "21000000.00000001", "1e5", " . "] {
            assert!(parse_zec(bad).is_err(), "{bad} should fail");
        }
        assert_eq!(format_zec(1_500_000), "0.015");
        assert_eq!(format_zec(100_000_000), "1");
        assert_eq!(format_zec(1), "0.00000001");
        assert_eq!(parse_zec(&format_zec(123_456_789)).unwrap(), 123_456_789);
    }

    #[test]
    fn zip321_roundtrip() {
        let vk = ViewingKey::from_ufvk(Network::TestNetwork, test_ufvk()).unwrap();
        let (addr, _) = vk.invoice_address(42).unwrap();
        let uri = zip321_uri(&addr, 1_500_000, Some("zeccheckout:abc"), Some("Order 7")).unwrap();
        assert!(uri.starts_with(&format!("zcash:{addr}?")));
        assert!(uri.contains("amount=0.015"));
        let parsed = TransactionRequest::from_uri(&uri).unwrap();
        let p = parsed.payments().get(&0).unwrap();
        assert_eq!(p.amount().unwrap().into_u64(), 1_500_000);
        assert_eq!(p.recipient_address().encode(), addr);
        assert_eq!(p.label().unwrap(), "Order 7");
        let memo = Memo::try_from(p.memo().unwrap().clone()).unwrap();
        assert_eq!(memo, Memo::from_str("zeccheckout:abc").unwrap());
    }

    #[test]
    fn zip321_without_memo() {
        let vk = ViewingKey::from_ufvk(Network::TestNetwork, test_ufvk()).unwrap();
        let (addr, _) = vk.invoice_address(1).unwrap();
        let uri = zip321_uri(&addr, 100_000_000, None, None).unwrap();
        assert_eq!(uri, format!("zcash:{addr}?amount=1"));
    }

    #[test]
    fn confirmation_math() {
        assert_eq!(confirmations(100, 100), 1);
        assert_eq!(confirmations(100, 91), 10);
        assert_eq!(confirmations(100, 101), 0);
    }

    fn r(v: u64, h: u64) -> Received {
        Received { value_zat: v, height: h }
    }

    #[test]
    fn state_machine() {
        let amt = 1000;
        // nothing received, not expired
        let e = evaluate(Status::Pending, amt, 500, 100, &[], 10, 3);
        assert_eq!(e.status, Status::Pending);
        // nothing received, expired
        assert_eq!(evaluate(Status::Pending, amt, 500, 500, &[], 10, 3).status, Status::Expired);
        // partial
        let e = evaluate(Status::Pending, amt, 500, 100, &[r(400, 10)], 20, 3);
        assert_eq!(e.status, Status::Underpaid);
        assert_eq!(e.received_zat, 400);
        // full but unconfirmed (tip 11, mined 10 -> 2 confs < 3)
        let e = evaluate(Status::Pending, amt, 500, 100, &[r(1000, 10)], 11, 3);
        assert_eq!(e.status, Status::Detected);
        assert_eq!(e.confirmed_zat, 0);
        // exactly 3 confirmations
        let e = evaluate(Status::Detected, amt, 500, 100, &[r(1000, 10)], 12, 3);
        assert_eq!(e.status, Status::Paid);
        assert_eq!(e.paid_height, Some(10));
        // two partial payments, second confirms later
        let rs = [r(600, 10), r(600, 15)];
        assert_eq!(evaluate(Status::Underpaid, amt, 500, 100, &rs, 16, 3).status, Status::Detected);
        let e = evaluate(Status::Detected, amt, 500, 100, &rs, 17, 3);
        assert_eq!(e.status, Status::Paid);
        assert_eq!(e.paid_height, Some(15));
        // payment arriving after expiry still counts
        assert_eq!(evaluate(Status::Expired, amt, 500, 900, &[r(1000, 10)], 20, 3).status, Status::Paid);
        // paid is final even if a reorg removed the outputs
        assert_eq!(evaluate(Status::Paid, amt, 500, 900, &[], 20, 3).status, Status::Paid);
        // min_conf 0 is treated as 1
        assert_eq!(evaluate(Status::Pending, amt, 500, 100, &[r(1000, 10)], 10, 0).status, Status::Paid);
    }

    #[test]
    fn invoice_ids() {
        let a = new_invoice_id();
        let b = new_invoice_id();
        assert_ne!(a, b);
        assert!(is_valid_invoice_id(&a));
        assert!(!is_valid_invoice_id("../etc/passwd"));
        assert!(!is_valid_invoice_id(&a.to_uppercase()));
    }
}

//! Application core: invoice creation, applying scanned blocks, status
//! transitions and webhook enqueueing. The network loops at the bottom drive
//! it from lightwalletd; tests drive it with synthetic blocks.

use anyhow::{anyhow, bail, Result};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use zcash_client_backend::proto::compact_formats::CompactBlock;

use crate::db::{Db, InvoiceRow, NewInvoice, NoteRow};
use crate::invoice::{self, Status};
use crate::keys::ViewingKey;
use crate::scanner::{self, Lightwalletd};
use crate::webhook::Delivery;

/// How many blocks to roll back when a reorg is detected.
pub const REORG_ROLLBACK: u64 = 20;

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub min_confirmations: u64,
    pub default_expiry_secs: u64,
    pub diversifier_start: u64,
    pub include_memo: bool,
    pub merchant_name: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            min_confirmations: 3,
            default_expiry_secs: 3600,
            diversifier_start: 1_000_000,
            include_memo: true,
            merchant_name: None,
        }
    }
}

pub struct App {
    pub db: Db,
    pub vk: ViewingKey,
    pub settings: Settings,
    /// Invoice id -> zatoshis seen in the mempool (in memory only, advisory).
    pub mempool_seen: Mutex<HashMap<String, u64>>,
    /// Latest chain tip reported by lightwalletd.
    pub tip: Mutex<Option<u64>>,
    /// Serialises diversifier-index allocation.
    alloc: Mutex<()>,
}

pub struct CreateInvoice {
    pub amount_zat: u64,
    pub order_id: Option<String>,
    pub description: Option<String>,
    pub expires_in_secs: Option<u64>,
}

fn clean_text(s: Option<String>, max: usize) -> Result<Option<String>> {
    match s {
        None => Ok(None),
        Some(s) => {
            let s = s.trim().to_owned();
            if s.is_empty() {
                return Ok(None);
            }
            if s.chars().count() > max || s.chars().any(|c| c.is_control()) {
                bail!("text field too long or contains control characters");
            }
            Ok(Some(s))
        }
    }
}

impl App {
    pub fn new(db: Db, vk: ViewingKey, settings: Settings) -> Self {
        Self {
            db,
            vk,
            settings,
            mempool_seen: Mutex::new(HashMap::new()),
            tip: Mutex::new(None),
            alloc: Mutex::new(()),
        }
    }

    pub fn tip(&self) -> Option<u64> {
        *self.tip.lock().unwrap()
    }

    pub fn create_invoice(&self, req: CreateInvoice, now: u64) -> Result<InvoiceRow> {
        if req.amount_zat == 0 || req.amount_zat > invoice::MAX_ZAT {
            bail!("invalid amount");
        }
        let order_id = clean_text(req.order_id, 128)?;
        let description = clean_text(req.description, 200)?;
        let ttl = req
            .expires_in_secs
            .unwrap_or(self.settings.default_expiry_secs)
            .clamp(60, 30 * 24 * 3600);

        let _guard = self.alloc.lock().unwrap();
        let start = match self.db.max_div_index()? {
            Some(j) => j + 1,
            None => self.settings.diversifier_start,
        };
        let (address, j) = self.vk.invoice_address(start)?;
        let id = invoice::new_invoice_id();
        let memo = self
            .settings
            .include_memo
            .then(|| format!("zeccheckout invoice {}", &id[..12]));
        let label = self.settings.merchant_name.clone();
        let uri = invoice::zip321_uri(&address, req.amount_zat, memo.as_deref(), label.as_deref())?;
        self.db.insert_invoice(&NewInvoice {
            id: &id,
            order_id: order_id.as_deref(),
            description: description.as_deref(),
            amount_zat: req.amount_zat,
            div_index: j,
            address: &address,
            uri: &uri,
            created_at: now,
            expires_at: now + ttl,
        })?;
        tracing::info!(invoice = %id, amount_zat = req.amount_zat, "invoice created");
        self.db
            .get_invoice(&id)?
            .ok_or_else(|| anyhow!("invoice vanished after insert"))
    }

    /// Applies one compact block. Returns `Err` with `ReorgDetected` if the
    /// block does not connect to the stored chain.
    pub fn apply_block(&self, block: &CompactBlock) -> Result<usize, ApplyError> {
        let h = block.height;
        if h > 0 {
            if let Some(prev) = self.db.block_hash(h - 1).map_err(ApplyError::Other)? {
                if prev != block.prev_hash {
                    return Err(ApplyError::Reorg { height: h });
                }
            }
        }
        let mut notes = Vec::new();
        for d in scanner::scan_block(&self.vk, block) {
            if let Some(invoice_id) = self
                .db
                .invoice_id_by_div_index(d.div_index)
                .map_err(ApplyError::Other)?
            {
                tracing::info!(invoice = %invoice_id, pool = d.pool.as_str(), height = h, "payment output detected");
                notes.push(NoteRow {
                    invoice_id,
                    pool: d.pool.as_str(),
                    txid: d.txid,
                    output_index: d.output_index,
                    value_zat: d.value_zat,
                    height: h,
                });
            }
            // Outputs to other diversifier indices (e.g. the merchant's own
            // wallet addresses) are ignored and not recorded.
        }
        self.db
            .commit_block(h, &block.hash, &notes)
            .map_err(ApplyError::Other)?;
        Ok(notes.len())
    }

    /// Recomputes every open invoice's status against `tip` and enqueues a
    /// webhook for each transition. Returns the transitions made.
    pub fn refresh(&self, tip: u64, now: u64) -> Result<Vec<(String, Status)>> {
        let mut changes = Vec::new();
        for inv in self.db.open_invoices()? {
            let received = self.db.received_for(&inv.id)?;
            let ev = invoice::evaluate(
                inv.status,
                inv.amount_zat,
                inv.expires_at,
                now,
                &received,
                tip,
                self.settings.min_confirmations,
            );
            let changed = ev.status != inv.status
                || ev.received_zat != inv.received_zat
                || ev.confirmed_zat != inv.confirmed_zat;
            if !changed {
                continue;
            }
            let paid_at = (ev.status == Status::Paid).then_some(now);
            self.db.update_invoice_state(
                &inv.id,
                ev.status,
                ev.received_zat,
                ev.confirmed_zat,
                paid_at,
                ev.paid_height,
            )?;
            if ev.status != inv.status {
                tracing::info!(invoice = %inv.id, from = inv.status.as_str(), to = ev.status.as_str(), "status change");
                let updated = self.db.get_invoice(&inv.id)?.expect("exists");
                let event = format!("invoice.{}", ev.status.as_str());
                let body = webhook_body(&event, &updated, now);
                self.db.enqueue_webhook(&inv.id, &event, &body, now)?;
                changes.push((inv.id.clone(), ev.status));
            }
        }
        Ok(changes)
    }

    pub fn handle_reorg(&self, height: u64) -> Result<()> {
        let to = height.saturating_sub(REORG_ROLLBACK);
        tracing::warn!(at = height, rollback_to = to, "chain reorganisation detected; rolling back");
        self.db.rollback_to(to)
    }

    pub fn mempool_seen_for(&self, id: &str) -> u64 {
        self.mempool_seen.lock().unwrap().get(id).copied().unwrap_or(0)
    }
}

#[derive(Debug)]
pub enum ApplyError {
    Reorg { height: u64 },
    Other(anyhow::Error),
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApplyError::Reorg { height } => write!(f, "reorg detected at {height}"),
            ApplyError::Other(e) => write!(f, "{e}"),
        }
    }
}

/// Webhook payload. Deliberately excludes addresses, txids and memos: the
/// merchant backend needs to know *that* an order was paid, not how.
pub fn webhook_body(event: &str, inv: &InvoiceRow, now: u64) -> String {
    json!({
        "event": event,
        "created": now,
        "invoice": {
            "id": inv.id,
            "order_id": inv.order_id,
            "status": inv.status.as_str(),
            "amount_zat": inv.amount_zat,
            "amount_zec": invoice::format_zec(inv.amount_zat),
            "received_zat": inv.received_zat,
            "confirmed_zat": inv.confirmed_zat,
            "paid_at": inv.paid_at,
        }
    })
    .to_string()
}

/// Scanner loop: catch up to the tip in batches, then poll.
pub async fn run_scanner(app: Arc<App>, lwd_url: String, poll: Duration, birthday: Option<u64>) {
    let mut lwd: Option<Lightwalletd> = None;
    loop {
        if let Err(e) = scan_step(&app, &mut lwd, &lwd_url, birthday).await {
            tracing::warn!("scanner: {e:#}");
            lwd = None; // reconnect next time
        }
        tokio::time::sleep(poll).await;
    }
}

const BATCH: u64 = 500;

pub async fn scan_step(
    app: &App,
    lwd: &mut Option<Lightwalletd>,
    url: &str,
    birthday: Option<u64>,
) -> Result<()> {
    if lwd.is_none() {
        *lwd = Some(Lightwalletd::connect(url).await?);
    }
    let client = lwd.as_mut().expect("connected");
    let tip = client.tip().await?;
    *app.tip.lock().unwrap() = Some(tip);

    let mut scanned = match app.db.get_meta_u64("scan_height")? {
        Some(h) => h,
        None => {
            // First run: start just below the birthday (default: current tip).
            let start = birthday.unwrap_or(tip).saturating_sub(1);
            app.db.set_meta("scan_height", &start.to_string())?;
            tracing::info!(start, "initialised scan height");
            start
        }
    };

    while scanned < tip {
        let from = scanned + 1;
        let to = (from + BATCH - 1).min(tip);
        let blocks = client.blocks(from, to).await?;
        for b in &blocks {
            match app.apply_block(b) {
                Ok(_) => scanned = b.height,
                Err(ApplyError::Reorg { height }) => {
                    app.handle_reorg(height)?;
                    scanned = app.db.get_meta_u64("scan_height")?.unwrap_or(0);
                    break;
                }
                Err(ApplyError::Other(e)) => return Err(e),
            }
        }
        app.refresh(tip, now())?;
    }
    app.refresh(tip, now())?;

    // Mempool: advisory "payment seen" for a snappier checkout UX. Never
    // affects invoice status, which only depends on mined, confirmed outputs.
    match client.mempool().await {
        Ok(txs) => {
            let mut seen: HashMap<String, u64> = HashMap::new();
            for tx in &txs {
                for d in scanner::scan_tx(&app.vk, tx) {
                    if let Some(id) = app.db.invoice_id_by_div_index(d.div_index)? {
                        *seen.entry(id).or_default() += d.value_zat;
                    }
                }
            }
            *app.mempool_seen.lock().unwrap() = seen;
        }
        Err(e) => tracing::debug!("mempool fetch failed: {e:#}"),
    }
    Ok(())
}

pub async fn run_webhooks(app: Arc<App>, delivery: Delivery, poll: Duration) {
    loop {
        if let Err(e) = delivery.deliver_due(&app.db, now()).await {
            tracing::warn!("webhook worker: {e:#}");
        }
        tokio::time::sleep(poll).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::tests::{decode_ua, test_ufvk};
    use crate::scanner::tests::{block, orchard_action, sapling_output, tx};
    use zcash_protocol::consensus::Network;

    fn app() -> App {
        let vk = ViewingKey::from_ufvk(Network::TestNetwork, test_ufvk()).unwrap();
        App::new(Db::open(":memory:").unwrap(), vk, Settings::default())
    }

    fn hash_of(b: &CompactBlock) -> [u8; 32] {
        b.hash.clone().try_into().unwrap()
    }

    #[test]
    fn invoices_get_unique_monotonic_addresses() {
        let app = app();
        let a = app.create_invoice(CreateInvoice { amount_zat: 10, order_id: Some("o1".into()), description: None, expires_in_secs: None }, 0).unwrap();
        let b = app.create_invoice(CreateInvoice { amount_zat: 10, order_id: None, description: None, expires_in_secs: None }, 0).unwrap();
        assert_ne!(a.address, b.address);
        assert!(b.div_index > a.div_index);
        assert!(a.div_index >= Settings::default().diversifier_start);
        assert!(a.uri.contains("memo="));
        assert_eq!(a.status, Status::Pending);
        assert_eq!(a.expires_at, 3600);
        assert!(app.create_invoice(CreateInvoice { amount_zat: 0, order_id: None, description: None, expires_in_secs: None }, 0).is_err());
        assert!(app.create_invoice(CreateInvoice { amount_zat: 1, order_id: Some("bad\nline".into()), description: None, expires_in_secs: None }, 0).is_err());
    }

    #[test]
    fn end_to_end_payment_confirmation_and_webhook_enqueue() {
        let app = app();
        let inv = app.create_invoice(CreateInvoice { amount_zat: 150_000, order_id: Some("A-1".into()), description: None, expires_in_secs: Some(600) }, 1000).unwrap();
        let ua = decode_ua(&inv.address);

        let b1 = block(100, [0; 32], vec![]);
        app.apply_block(&b1).unwrap();
        // Partial payment via Sapling.
        let b2 = block(101, hash_of(&b1), vec![tx(1, vec![sapling_output(ua.sapling().unwrap(), 50_000)], vec![])]);
        assert_eq!(app.apply_block(&b2).unwrap(), 1);
        let ch = app.refresh(101, 1001).unwrap();
        assert_eq!(ch, vec![(inv.id.clone(), Status::Underpaid)]);
        // Remainder via Orchard.
        let b3 = block(102, hash_of(&b2), vec![tx(2, vec![], vec![orchard_action(ua.orchard().unwrap(), 100_000)])]);
        app.apply_block(&b3).unwrap();
        assert_eq!(app.refresh(102, 1002).unwrap(), vec![(inv.id.clone(), Status::Detected)]);
        // Two more blocks -> 3 confirmations for the block-102 output.
        let b4 = block(103, hash_of(&b3), vec![]);
        app.apply_block(&b4).unwrap();
        assert!(app.refresh(103, 1003).unwrap().is_empty());
        let b5 = block(104, hash_of(&b4), vec![]);
        app.apply_block(&b5).unwrap();
        assert_eq!(app.refresh(104, 1004).unwrap(), vec![(inv.id.clone(), Status::Paid)]);

        let row = app.db.get_invoice(&inv.id).unwrap().unwrap();
        assert_eq!(row.status, Status::Paid);
        assert_eq!(row.received_zat, 150_000);
        assert_eq!(row.paid_height, Some(102));
        assert_eq!(row.paid_at, Some(1004));
        // Three webhooks queued: underpaid, detected, paid.
        let due = app.db.due_webhooks(2000).unwrap();
        assert_eq!(due.len(), 3);
        let last: serde_json::Value = serde_json::from_str(&due[2].body).unwrap();
        assert_eq!(last["event"], "invoice.paid");
        assert_eq!(last["invoice"]["order_id"], "A-1");
        // Privacy: no address / txid / memo in webhook payloads.
        assert!(!due[2].body.contains(&inv.address));
        assert!(!due[2].body.contains("memo"));
        // Re-applying the same block is idempotent.
        assert!(app.refresh(104, 1005).unwrap().is_empty());
    }

    #[test]
    fn reorg_rolls_back_unconfirmed_payment() {
        let app = app();
        let inv = app.create_invoice(CreateInvoice { amount_zat: 1000, order_id: None, description: None, expires_in_secs: None }, 0).unwrap();
        let ua = decode_ua(&inv.address);
        let b1 = block(200, [0; 32], vec![]);
        app.apply_block(&b1).unwrap();
        let b2 = block(201, hash_of(&b1), vec![tx(1, vec![], vec![orchard_action(ua.orchard().unwrap(), 1000)])]);
        app.apply_block(&b2).unwrap();
        app.refresh(201, 1).unwrap();
        assert_eq!(app.db.get_invoice(&inv.id).unwrap().unwrap().status, Status::Detected);

        // A competing block 202 that does not build on our 201.
        let fork = block(202, [0xAA; 32], vec![]);
        match app.apply_block(&fork) {
            Err(ApplyError::Reorg { height }) => app.handle_reorg(height).unwrap(),
            other => panic!("expected reorg, got {other:?}"),
        }
        assert!(app.db.received_for(&inv.id).unwrap().is_empty());
        app.refresh(202, 2).unwrap();
        assert_eq!(app.db.get_invoice(&inv.id).unwrap().unwrap().status, Status::Pending);
    }

    #[test]
    fn expiry_and_unrelated_outputs() {
        let app = app();
        let inv = app.create_invoice(CreateInvoice { amount_zat: 1000, order_id: None, description: None, expires_in_secs: Some(60) }, 0).unwrap();
        // Payment to the merchant's own default address (index 0) is ignored.
        let (own, _) = app.vk.invoice_address(0).unwrap();
        let own = decode_ua(&own);
        let b = block(300, [0; 32], vec![tx(1, vec![], vec![orchard_action(own.orchard().unwrap(), 1000)])]);
        assert_eq!(app.apply_block(&b).unwrap(), 0);
        assert!(app.refresh(300, 59).unwrap().is_empty());
        assert_eq!(app.refresh(300, 60).unwrap(), vec![(inv.id.clone(), Status::Expired)]);
    }
}

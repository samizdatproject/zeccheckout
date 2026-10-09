//! Live integration test against a public Zcash TESTNET lightwalletd.
//! Read-only: downloads compact blocks and trial-decrypts them with a
//! throwaway viewing key. Run with:
//!
//!   cargo test --test testnet_live -- --ignored --nocapture
//!
//! Override the server with ZC_LIGHTWALLETD (default https://testnet.zec.rocks:443).

use zcash_keys::keys::UnifiedSpendingKey;
use zcash_protocol::consensus::Network;
use zeccheckout::db::Db;
use zeccheckout::keys::ViewingKey;
use zeccheckout::scanner::Lightwalletd;
use zeccheckout::service::{App, CreateInvoice, Settings};

fn url() -> String {
    std::env::var("ZC_LIGHTWALLETD").unwrap_or_else(|_| "https://testnet.zec.rocks:443".into())
}

#[tokio::test]
#[ignore = "needs network access to a testnet lightwalletd"]
async fn scans_real_testnet_blocks_with_hash_chain_continuity() {
    let ufvk = UnifiedSpendingKey::from_seed(&Network::TestNetwork, &[42u8; 32], zip32::AccountId::ZERO)
        .unwrap()
        .to_unified_full_viewing_key();
    let vk = ViewingKey::from_ufvk(Network::TestNetwork, ufvk).unwrap();
    let app = App::new(Db::open(":memory:").unwrap(), vk, Settings::default());
    let inv = app
        .create_invoice(CreateInvoice { amount_zat: 100_000, order_id: None, description: None, expires_in_secs: None }, 0)
        .unwrap();

    let mut lwd = Lightwalletd::connect(&url()).await.expect("connect");
    let tip = lwd.tip().await.unwrap();
    assert!(tip > 3_000_000, "testnet tip looks wrong: {tip}");
    let from = tip - 1500;
    let blocks = lwd.blocks(from, tip).await.unwrap();
    assert_eq!(blocks.len() as u64, tip - from + 1);
    let mut outputs = 0usize;
    for b in &blocks {
        outputs += b.vtx.iter().map(|t| t.outputs.len() + t.actions.len() + t.ironwood_actions.len()).sum::<usize>();
        // apply_block checks prev_hash against the stored previous hash, so
        // this verifies real chain continuity across all 1500 blocks.
        app.apply_block(b).unwrap_or_else(|e| panic!("block {}: {e}", b.height));
    }
    assert_eq!(app.db.get_meta_u64("scan_height").unwrap(), Some(tip));
    app.refresh(tip, 1).unwrap();
    let row = app.db.get_invoice(&inv.id).unwrap().unwrap();
    assert_eq!(row.received_zat, 0, "fresh key must not receive anything");
    let mempool = lwd.mempool().await.unwrap();
    println!(
        "testnet tip {tip}: scanned {} blocks, trial-decrypted {outputs} shielded outputs, mempool {} txs ({} Ironwood actions); invoice address {}",
        blocks.len(),
        mempool.len(),
        mempool.iter().map(|t| t.ironwood_actions.len()).sum::<usize>(),
        row.address
    );
}

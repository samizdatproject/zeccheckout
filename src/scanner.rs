//! Compact-block scanning: trial-decrypt Sapling outputs and Orchard actions
//! with the viewing key's external IVKs, and fetch blocks from lightwalletd.
//!
//! Only *compact* blocks are downloaded, and always the full block range, so
//! the lightwalletd operator learns nothing about which transactions are
//! ours. We deliberately never call `GetTransaction` (which would reveal the
//! txid we care about) and therefore never see memos: invoices are matched by
//! their unique diversified address instead.

use anyhow::{anyhow, Context, Result};
use std::time::Duration;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};
use zcash_client_backend::proto::compact_formats::{CompactBlock, CompactTx};
use zcash_client_backend::proto::service::{
    compact_tx_streamer_client::CompactTxStreamerClient, BlockId, BlockRange, ChainSpec,
    GetMempoolTxRequest, PoolType,
};
use zcash_note_encryption::{try_compact_note_decryption, COMPACT_NOTE_SIZE};

use crate::keys::ViewingKey;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pool {
    Sapling,
    Orchard,
}

impl Pool {
    pub fn as_str(self) -> &'static str {
        match self {
            Pool::Sapling => "sapling",
            Pool::Orchard => "orchard",
        }
    }
}

/// An incoming shielded output addressed to one of our diversified addresses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detected {
    pub div_index: u64,
    pub pool: Pool,
    pub txid: [u8; 32],
    pub output_index: u32,
    pub value_zat: u64,
}

/// Trial-decrypts every shielded output of a compact transaction.
pub fn scan_tx(vk: &ViewingKey, tx: &CompactTx) -> Vec<Detected> {
    let mut out = Vec::new();
    let Ok(txid) = <[u8; 32]>::try_from(tx.txid.as_slice()) else {
        tracing::warn!("compact transaction without a valid txid; skipping");
        return out;
    };

    if let Some(ivk) = vk.sapling_ivk() {
        for (i, o) in tx.outputs.iter().enumerate() {
            // Upstream conversions panic on wrong lengths; a hostile or buggy
            // lightwalletd must not be able to crash the scanner.
            if o.cmu.len() != 32 || o.ephemeral_key.len() != 32 || o.ciphertext.len() != COMPACT_NOTE_SIZE {
                continue;
            }
            let Ok(desc) = sapling_crypto::note_encryption::CompactOutputDescription::try_from(o)
            else {
                continue;
            };
            // All heights we scan are post-Canopy (ZIP 212 fully enforced).
            if let Some((note, addr)) = sapling_crypto::note_encryption::try_sapling_compact_note_decryption(
                ivk,
                &desc,
                sapling_crypto::note_encryption::Zip212Enforcement::On,
            ) {
                if let Some(j) = vk.sapling_index(&addr) {
                    out.push(Detected {
                        div_index: j,
                        pool: Pool::Sapling,
                        txid,
                        output_index: i as u32,
                        value_zat: note.value().inner(),
                    });
                }
            }
        }
    }

    if let Some(ivk) = vk.orchard_ivk() {
        for (i, a) in tx.actions.iter().enumerate() {
            if a.nullifier.len() != 32
                || a.cmx.len() != 32
                || a.ephemeral_key.len() != 32
                || a.ciphertext.len() != COMPACT_NOTE_SIZE
            {
                continue;
            }
            let Ok(action) = orchard::note_encryption::CompactAction::try_from(a) else {
                continue;
            };
            let domain = orchard::note_encryption::OrchardDomain::for_compact_action(&action);
            if let Some((note, addr)) = try_compact_note_decryption(&domain, ivk, &action) {
                if let Some(j) = vk.orchard_index(&addr) {
                    out.push(Detected {
                        div_index: j,
                        pool: Pool::Orchard,
                        txid,
                        output_index: i as u32,
                        value_zat: note.value().inner(),
                    });
                }
            }
        }
    }
    out
}

pub fn scan_block(vk: &ViewingKey, block: &CompactBlock) -> Vec<Detected> {
    block.vtx.iter().flat_map(|tx| scan_tx(vk, tx)).collect()
}

/// Thin lightwalletd gRPC client.
#[derive(Clone)]
pub struct Lightwalletd {
    client: CompactTxStreamerClient<Channel>,
}

impl Lightwalletd {
    pub async fn connect(url: &str) -> Result<Self> {
        let mut ep = Endpoint::from_shared(url.to_owned())
            .context("invalid lightwalletd URL")?
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(120))
            // Do not advertise a product-specific user agent to the operator.
            .user_agent("grpc")?;
        if url.starts_with("https://") {
            ep = ep.tls_config(ClientTlsConfig::new().with_webpki_roots())?;
        }
        let channel = ep.connect().await.context("connecting to lightwalletd")?;
        Ok(Self {
            client: CompactTxStreamerClient::new(channel)
                .max_decoding_message_size(32 * 1024 * 1024),
        })
    }

    pub async fn tip(&mut self) -> Result<u64> {
        Ok(self.client.get_latest_block(ChainSpec {}).await?.into_inner().height)
    }

    /// Streams compact blocks `[start, end]` (inclusive), shielded data only.
    pub async fn blocks(&mut self, start: u64, end: u64) -> Result<Vec<CompactBlock>> {
        let req = BlockRange {
            start: Some(BlockId { height: start, hash: vec![] }),
            end: Some(BlockId { height: end, hash: vec![] }),
            pool_types: vec![PoolType::Sapling as i32, PoolType::Orchard as i32],
        };
        let mut stream = self.client.get_block_range(req).await?.into_inner();
        let mut out = Vec::with_capacity((end + 1).saturating_sub(start) as usize);
        while let Some(b) = stream.message().await? {
            out.push(b);
        }
        if out.len() as u64 != end - start + 1 {
            return Err(anyhow!(
                "lightwalletd returned {} blocks for range {start}..={end}",
                out.len()
            ));
        }
        Ok(out)
    }

    /// The whole current mempool in compact form (we never ask for a subset,
    /// so the operator learns nothing about our interest).
    pub async fn mempool(&mut self) -> Result<Vec<CompactTx>> {
        let req = GetMempoolTxRequest {
            exclude_txid_suffixes: vec![],
            pool_types: vec![PoolType::Sapling as i32, PoolType::Orchard as i32],
        };
        let mut stream = self.client.get_mempool_tx(req).await?.into_inner();
        let mut out = Vec::new();
        while let Some(tx) = stream.message().await? {
            out.push(tx);
        }
        Ok(out)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    //! Builds synthetic compact blocks containing notes encrypted to our own
    //! diversified addresses, exactly as a sending wallet would, and checks the
    //! scanner finds them (and only them).
    use super::*;
    use crate::keys::tests::{decode_ua, test_ufvk};
    use rand::{rngs::OsRng, RngCore};
    use zcash_client_backend::proto::compact_formats::{CompactOrchardAction, CompactSaplingOutput};
    use zcash_note_encryption::{Domain, NoteEncryption};
    use zcash_protocol::consensus::Network;

    pub fn sapling_output(addr: &sapling_crypto::PaymentAddress, value: u64) -> CompactSaplingOutput {
        let mut rseed = [0u8; 32];
        OsRng.fill_bytes(&mut rseed);
        let note = sapling_crypto::Note::from_parts(
            *addr,
            sapling_crypto::value::NoteValue::from_raw(value),
            sapling_crypto::Rseed::AfterZip212(rseed),
        );
        let enc = sapling_crypto::note_encryption::sapling_note_encryption(None, note.clone(), [0u8; 512], &mut OsRng);
        let epk = sapling_crypto::note_encryption::SaplingDomain::epk_bytes(enc.epk());
        let ct = enc.encrypt_note_plaintext();
        CompactSaplingOutput {
            cmu: note.cmu().to_bytes().to_vec(),
            ephemeral_key: epk.0.to_vec(),
            ciphertext: ct[..COMPACT_NOTE_SIZE].to_vec(),
        }
    }

    pub fn orchard_action(addr: &orchard::Address, value: u64) -> CompactOrchardAction {
        // rho must be a valid Pallas base element; the nullifier field of the
        // compact action carries it.
        let (rho_bytes, rho, note) = loop {
            let mut rb = [0u8; 32];
            OsRng.fill_bytes(&mut rb);
            rb[31] &= 0x3f;
            let Some(rho) = Option::<orchard::note::Rho>::from(orchard::note::Rho::from_bytes(&rb)) else {
                continue;
            };
            let mut seed = [0u8; 32];
            OsRng.fill_bytes(&mut seed);
            let Some(rseed) = Option::from(orchard::note::RandomSeed::from_bytes(seed, &rho)) else {
                continue;
            };
            let Some(note) = Option::from(orchard::Note::from_parts(
                *addr,
                orchard::value::NoteValue::from_raw(value),
                rho,
                rseed,
                orchard::note::NoteVersion::V2,
            )) else {
                continue;
            };
            break (rb, rho, note);
        };
        let _ = rho;
        let enc = NoteEncryption::<orchard::note_encryption::OrchardDomain>::new(None, note, [0u8; 512]);
        let epk = orchard::note_encryption::OrchardDomain::epk_bytes(enc.epk());
        let ct = enc.encrypt_note_plaintext();
        let cmx = orchard::note::ExtractedNoteCommitment::from(note.commitment());
        CompactOrchardAction {
            nullifier: rho_bytes.to_vec(),
            cmx: cmx.to_bytes().to_vec(),
            ephemeral_key: epk.0.to_vec(),
            ciphertext: ct[..COMPACT_NOTE_SIZE].to_vec(),
        }
    }

    pub fn tx(id: u8, outputs: Vec<CompactSaplingOutput>, actions: Vec<CompactOrchardAction>) -> CompactTx {
        CompactTx {
            index: 0,
            txid: vec![id; 32],
            fee: 0,
            spends: vec![],
            outputs,
            actions,
            ironwood_actions: vec![],
            vin: vec![],
            vout: vec![],
        }
    }

    pub fn block(height: u64, prev: [u8; 32], vtx: Vec<CompactTx>) -> CompactBlock {
        let mut hash = [0u8; 32];
        hash[..8].copy_from_slice(&height.to_le_bytes());
        hash[8..16].copy_from_slice(&prev[..8]);
        CompactBlock {
            height,
            hash: hash.to_vec(),
            prev_hash: prev.to_vec(),
            time: 0,
            header: vec![],
            vtx,
            chain_metadata: None,
        }
    }

    #[test]
    fn detects_own_orchard_and_sapling_outputs_only() {
        let vk = ViewingKey::from_ufvk(Network::TestNetwork, test_ufvk()).unwrap();
        let (a1, j1) = vk.invoice_address(100).unwrap();
        let (a2, j2) = vk.invoice_address(j1 + 1).unwrap();
        let ua1 = decode_ua(&a1);
        let ua2 = decode_ua(&a2);

        // A stranger's address (different seed).
        let other = zcash_keys::keys::UnifiedSpendingKey::from_seed(&Network::TestNetwork, &[9u8; 32], zip32::AccountId::ZERO)
            .unwrap()
            .to_unified_full_viewing_key();
        let other_vk = ViewingKey::from_ufvk(Network::TestNetwork, other).unwrap();
        let (oa, _) = other_vk.invoice_address(0).unwrap();
        let oua = decode_ua(&oa);

        let b = block(
            10,
            [0u8; 32],
            vec![
                tx(1, vec![sapling_output(ua1.sapling().unwrap(), 5000)], vec![orchard_action(oua.orchard().unwrap(), 1)]),
                tx(2, vec![sapling_output(oua.sapling().unwrap(), 7)], vec![orchard_action(ua2.orchard().unwrap(), 12345)]),
            ],
        );
        let found = scan_block(&vk, &b);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0], Detected { div_index: j1, pool: Pool::Sapling, txid: [1; 32], output_index: 0, value_zat: 5000 });
        assert_eq!(found[1], Detected { div_index: j2, pool: Pool::Orchard, txid: [2; 32], output_index: 0, value_zat: 12345 });
        // The stranger sees exactly their own two outputs.
        assert_eq!(scan_block(&other_vk, &b).len(), 2);
    }

    #[test]
    fn malformed_outputs_are_skipped() {
        let vk = ViewingKey::from_ufvk(Network::TestNetwork, test_ufvk()).unwrap();
        let bad = CompactSaplingOutput { cmu: vec![1, 2, 3], ephemeral_key: vec![], ciphertext: vec![] };
        let bad_a = CompactOrchardAction { nullifier: vec![0; 3], cmx: vec![], ephemeral_key: vec![], ciphertext: vec![] };
        assert!(scan_block(&vk, &block(1, [0; 32], vec![tx(1, vec![bad], vec![bad_a])])).is_empty());
    }
}

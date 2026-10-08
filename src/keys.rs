//! Viewing-key handling: parse a UFVK, derive per-invoice diversified addresses,
//! and map a decrypted note's recipient back to the diversifier index it was
//! derived from.
//!
//! The server only ever sees a Unified Full Viewing Key. No spending key is
//! parsed, stored, or required anywhere in this crate (see `bin/zc-keygen.rs`
//! for the separate, testnet-only helper that creates a throwaway wallet).

use anyhow::{anyhow, bail, Context, Result};
use zcash_keys::keys::{ReceiverRequirement, UnifiedAddressRequest, UnifiedFullViewingKey};
use zcash_protocol::consensus::{Network, Parameters};
use zip32::{DiversifierIndex, Scope};

/// A parsed viewing key plus the precomputed incoming viewing keys used for
/// trial decryption.
pub struct ViewingKey {
    network: Network,
    ufvk: UnifiedFullViewingKey,
    request: UnifiedAddressRequest,
    sapling_dfvk: Option<sapling_crypto::zip32::DiversifiableFullViewingKey>,
    sapling_ivk: Option<sapling_crypto::keys::PreparedIncomingViewingKey>,
    orchard_ivk: Option<orchard::keys::IncomingViewingKey>,
    orchard_pivk: Option<orchard::keys::PreparedIncomingViewingKey>,
}

impl ViewingKey {
    /// Parses a UFVK string for the given network. Rejects keys that carry no
    /// shielded component, because this service never uses transparent
    /// addresses.
    pub fn parse(network: Network, encoded: &str) -> Result<Self> {
        let ufvk = UnifiedFullViewingKey::decode(&network, encoded.trim())
            .map_err(|e| anyhow!("invalid UFVK for {:?}: {e}", network))?;
        Self::from_ufvk(network, ufvk)
    }

    pub fn from_ufvk(network: Network, ufvk: UnifiedFullViewingKey) -> Result<Self> {
        let sapling_dfvk = ufvk.sapling().cloned();
        let orchard_fvk = ufvk.orchard().cloned();
        if sapling_dfvk.is_none() && orchard_fvk.is_none() {
            bail!("UFVK has neither an Orchard nor a Sapling component; transparent-only keys are not supported");
        }
        let req = |present: bool| {
            if present {
                ReceiverRequirement::Require
            } else {
                ReceiverRequirement::Omit
            }
        };
        // Never include a transparent receiver: invoices are shielded-only.
        let request = UnifiedAddressRequest::custom(
            req(orchard_fvk.is_some()),
            req(sapling_dfvk.is_some()),
            ReceiverRequirement::Omit,
        )
        .map_err(|e| anyhow!("cannot build address request: {e:?}"))?;

        let sapling_ivk = sapling_dfvk
            .as_ref()
            .map(|d| sapling_crypto::keys::PreparedIncomingViewingKey::new(&d.to_ivk(Scope::External)));
        let orchard_ivk = orchard_fvk.as_ref().map(|f| f.to_ivk(Scope::External));
        let orchard_pivk = orchard_ivk
            .as_ref()
            .map(orchard::keys::PreparedIncomingViewingKey::new);

        Ok(Self {
            network,
            ufvk,
            request,
            sapling_dfvk,
            sapling_ivk,
            orchard_ivk,
            orchard_pivk,
        })
    }

    pub fn network(&self) -> Network {
        self.network
    }

    /// Derives the first valid shielded unified address at or after diversifier
    /// index `start`. Returns the encoded address and the index actually used
    /// (Sapling diversifiers are only valid for about half of all indices).
    pub fn invoice_address(&self, start: u64) -> Result<(String, u64)> {
        let (ua, j) = self
            .ufvk
            .find_address(DiversifierIndex::from(start), self.request)
            .map_err(|e| anyhow!("address derivation failed: {e:?}"))?;
        let j = u64::try_from(j).map_err(|_| anyhow!("diversifier index overflow"))?;
        debug_assert!(!ua.has_transparent());
        Ok((ua.encode(&self.network), j))
    }

    pub fn sapling_ivk(&self) -> Option<&sapling_crypto::keys::PreparedIncomingViewingKey> {
        self.sapling_ivk.as_ref()
    }

    pub fn orchard_ivk(&self) -> Option<&orchard::keys::PreparedIncomingViewingKey> {
        self.orchard_pivk.as_ref()
    }

    /// Returns the external diversifier index of a Sapling recipient address
    /// derived from this key, or `None` if it is not ours (or is internal/change).
    pub fn sapling_index(&self, addr: &sapling_crypto::PaymentAddress) -> Option<u64> {
        let dfvk = self.sapling_dfvk.as_ref()?;
        match dfvk.decrypt_diversifier(addr)? {
            (j, Scope::External) => u64::try_from(j).ok(),
            _ => None,
        }
    }

    /// Returns the diversifier index of an Orchard recipient address derived
    /// from this key's external IVK.
    pub fn orchard_index(&self, addr: &orchard::Address) -> Option<u64> {
        let ivk = self.orchard_ivk.as_ref()?;
        ivk.diversifier_index(addr).and_then(|j| u64::try_from(j).ok())
    }

    pub fn network_name(&self) -> &'static str {
        match self.network.network_type() {
            zcash_protocol::consensus::NetworkType::Main => "mainnet",
            zcash_protocol::consensus::NetworkType::Test => "testnet",
            zcash_protocol::consensus::NetworkType::Regtest => "regtest",
        }
    }
}

pub fn parse_network(s: &str) -> Result<Network> {
    match s.trim().to_ascii_lowercase().as_str() {
        "main" | "mainnet" => Ok(Network::MainNetwork),
        "test" | "testnet" => Ok(Network::TestNetwork),
        other => Err(anyhow!("unknown network '{other}' (use mainnet or testnet)")),
    }
    .context("parsing network")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use zcash_keys::keys::UnifiedSpendingKey;

    /// Deterministic throwaway key for tests only.
    pub fn test_ufvk() -> UnifiedFullViewingKey {
        let seed = [7u8; 32];
        UnifiedSpendingKey::from_seed(&Network::TestNetwork, &seed, zip32::AccountId::ZERO)
            .unwrap()
            .to_unified_full_viewing_key()
    }

    pub fn decode_ua(s: &str) -> zcash_keys::address::UnifiedAddress {
        match zcash_keys::address::Address::decode(&Network::TestNetwork, s) {
            Some(zcash_keys::address::Address::Unified(ua)) => ua,
            _ => panic!("not a unified address"),
        }
    }

    #[test]
    fn ufvk_roundtrip_and_shielded_only_addresses() {
        let ufvk = test_ufvk();
        let enc = ufvk.encode(&Network::TestNetwork);
        assert!(enc.starts_with("uviewtest"));
        let vk = ViewingKey::parse(Network::TestNetwork, &enc).unwrap();
        let (addr, j) = vk.invoice_address(1000).unwrap();
        assert!(j >= 1000);
        assert!(addr.starts_with("utest"));
        let ua = decode_ua(&addr);
        assert!(ua.has_orchard());
        assert!(ua.has_sapling());
        assert!(!ua.has_transparent());
    }

    #[test]
    fn rejects_wrong_network() {
        let enc = test_ufvk().encode(&Network::TestNetwork);
        assert!(ViewingKey::parse(Network::MainNetwork, &enc).is_err());
        assert!(ViewingKey::parse(Network::TestNetwork, "garbage").is_err());
    }

    #[test]
    fn distinct_indices_give_distinct_addresses_and_map_back() {
        let vk = ViewingKey::from_ufvk(Network::TestNetwork, test_ufvk()).unwrap();
        let (a1, j1) = vk.invoice_address(5000).unwrap();
        let (a2, j2) = vk.invoice_address(j1 + 1).unwrap();
        assert_ne!(a1, a2);
        assert!(j2 > j1);
        let ua = decode_ua(&a2);
        assert_eq!(vk.orchard_index(ua.orchard().unwrap()), Some(j2));
        assert_eq!(vk.sapling_index(ua.sapling().unwrap()), Some(j2));
    }
}

//! Demo helper: creates a throwaway wallet seed and prints its UFVK.
//! Testnet by default; `--mainnet` exists only so you can record a demo with a
//! tiny real payment and later restore the 24 words in any ZIP-32 wallet.
//!
//! This is a development convenience and is deliberately a separate binary:
//! the `zeccheckout` server never loads, parses or needs a seed or spending
//! key. For production, export the UFVK from your own wallet instead
//! (e.g. `zcash-devtool`, Zallet, or any wallet that exports a UFVK).
//!
//! Usage: zc-keygen [--mainnet] <output-seed-file>
//! The 24-word mnemonic is written to <output-seed-file> (mode 0600); only the
//! UFVK and a receiving address are printed.

use anyhow::{bail, Result};
use rand::RngCore;
use std::io::Write;
use zcash_keys::address::Address;
use zcash_keys::keys::{ReceiverRequirement, UnifiedAddressRequest, UnifiedSpendingKey};
use zcash_protocol::consensus::Network;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mainnet = args.iter().any(|a| a == "--mainnet");
    let path = match args.iter().find(|a| !a.starts_with("--")) {
        Some(p) => p.clone(),
        None => bail!("usage: zc-keygen [--mainnet] <output-seed-file>"),
    };
    if std::path::Path::new(&path).exists() {
        bail!("{path} already exists; refusing to overwrite a seed");
    }
    let network = if mainnet { Network::MainNetwork } else { Network::TestNetwork };
    let mut entropy = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut entropy);
    let mnemonic = bip39::Mnemonic::from_entropy(&entropy)?;
    let seed = mnemonic.to_seed("");
    let usk = UnifiedSpendingKey::from_seed(&network, &seed, zip32::AccountId::ZERO)
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let ufvk = usk.to_unified_full_viewing_key();
    let (ua, _) = ufvk
        .default_address(
            UnifiedAddressRequest::custom(
                ReceiverRequirement::Require,
                ReceiverRequirement::Require,
                ReceiverRequirement::Omit,
            )
            .map_err(|e| anyhow::anyhow!("{e:?}"))?,
        )
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&path)?;
    writeln!(
        f,
        "# ZecCheckout throwaway {} demo wallet. Keep tiny balances only.",
        if mainnet { "MAINNET" } else { "TESTNET" }
    )?;
    writeln!(f, "{mnemonic}")?;

    println!("network:          {}", if mainnet { "mainnet" } else { "testnet" });
    println!("seed written to:  {path} (keep private; NOT needed by the server)");
    println!("UFVK:             {}", ufvk.encode(&network));
    println!("unified address:  {}", ua.encode(&network));
    if let Some(s) = ua.sapling() {
        println!("sapling address:  {}", Address::Sapling(*s).encode(&network));
    }
    Ok(())
}

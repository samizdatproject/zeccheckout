//! `zeccheckout` server binary.
//!
//! Subcommands:
//!   serve                 run the HTTP API, scanner and webhook worker (default)
//!   scan <from> <to>      one-off: scan a block range with the configured UFVK
//!                         and print every incoming output found (diagnostics)
//!   tip                   print the lightwalletd chain tip
//!
//! Configuration is via environment variables (see README / .env.example).

use anyhow::{anyhow, bail, Context, Result};
use std::sync::Arc;
use std::time::Duration;

use zeccheckout::api::{self, ApiState};
use zeccheckout::db::Db;
use zeccheckout::keys::{parse_network, ViewingKey};
use zeccheckout::scanner::{self, Lightwalletd};
use zeccheckout::service::{self, App, Settings};
use zeccheckout::webhook::Delivery;

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> Result<T> {
    match env(key) {
        Some(v) => v.trim().parse().map_err(|_| anyhow!("invalid value for {key}")),
        None => Ok(default),
    }
}

/// First diversifier index for invoice addresses. `boot` is for hosts without
/// persistent storage (the database is lost on restart): each boot starts in a
/// fresh range above every earlier boot, so a late payment to an invoice from
/// a previous boot can never be credited to a new invoice.
fn diversifier_start(value: Option<&str>, now: u64) -> Result<u64> {
    match value.map(str::trim) {
        None => Ok(1_000_000),
        Some("boot") => Ok(1_000_000 + now * 1000),
        Some(v) => v.parse().map_err(|_| anyhow!("invalid value for ZC_DIVERSIFIER_START")),
    }
}

fn load_vk() -> Result<ViewingKey> {
    let network = parse_network(&env("ZC_NETWORK").unwrap_or_else(|| "testnet".into()))?;
    let ufvk = match (env("ZC_UFVK"), env("ZC_UFVK_FILE")) {
        (Some(v), _) => v,
        (None, Some(path)) => std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?,
        (None, None) => bail!("set ZC_UFVK or ZC_UFVK_FILE to your Unified Full Viewing Key"),
    };
    let lower = ufvk.trim().to_ascii_lowercase();
    if lower.starts_with("secret") || lower.contains("extended-spending") || lower.split_whitespace().count() >= 12 {
        bail!("that looks like a spending key or seed phrase. ZecCheckout only needs (and must only be given) a viewing key");
    }
    ViewingKey::parse(network, &ufvk)
}

fn lightwalletd_url(vk: &ViewingKey) -> String {
    env("ZC_LIGHTWALLETD").unwrap_or_else(|| match vk.network_name() {
        "mainnet" => "https://zec.rocks:443".into(),
        _ => "https://testnet.zec.rocks:443".into(),
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(match env("ZC_LOG").as_deref() {
            Some("debug") => tracing::Level::DEBUG,
            Some("warn") => tracing::Level::WARN,
            _ => tracing::Level::INFO,
        })
        .with_target(false)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("serve") => serve().await,
        Some("scan") => {
            let from: u64 = args.get(1).ok_or_else(|| anyhow!("usage: scan <from> <to>"))?.parse()?;
            let to: u64 = args.get(2).ok_or_else(|| anyhow!("usage: scan <from> <to>"))?.parse()?;
            scan_range(from, to).await
        }
        Some("tip") => {
            let vk = load_vk()?;
            let mut lwd = Lightwalletd::connect(&lightwalletd_url(&vk)).await?;
            println!("{}", lwd.tip().await?);
            Ok(())
        }
        Some(other) => bail!("unknown command '{other}' (serve | scan <from> <to> | tip)"),
    }
}

async fn scan_range(from: u64, to: u64) -> Result<()> {
    if to < from {
        bail!("to < from");
    }
    let vk = load_vk()?;
    let mut lwd = Lightwalletd::connect(&lightwalletd_url(&vk)).await?;
    let mut h = from;
    let (mut blocks, mut outputs, mut found) = (0u64, 0u64, 0u64);
    while h <= to {
        let end = (h + 499).min(to);
        for b in lwd.blocks(h, end).await? {
            blocks += 1;
            outputs += b.vtx.iter().map(|t| (t.outputs.len() + t.actions.len() + t.ironwood_actions.len()) as u64).sum::<u64>();
            for d in scanner::scan_block(&vk, &b) {
                found += 1;
                println!(
                    "height={} pool={} diversifier_index={} value_zat={}",
                    b.height,
                    d.pool.as_str(),
                    d.div_index,
                    d.value_zat
                );
            }
        }
        h = end + 1;
    }
    println!("scanned {blocks} blocks, trial-decrypted {outputs} shielded outputs, {found} belong to this UFVK");
    Ok(())
}

async fn serve() -> Result<()> {
    let vk = load_vk()?;
    let api_key = env("ZC_API_KEY").ok_or_else(|| anyhow!("set ZC_API_KEY (merchant API bearer token, >= 24 chars)"))?;
    if api_key.len() < 24 {
        bail!("ZC_API_KEY must be at least 24 characters");
    }
    let settings = Settings {
        min_confirmations: env_parse("ZC_MIN_CONFIRMATIONS", 3u64)?.max(1),
        default_expiry_secs: env_parse("ZC_INVOICE_TTL_SECS", 3600u64)?,
        diversifier_start: diversifier_start(env("ZC_DIVERSIFIER_START").as_deref(), zeccheckout::service::now())?,
        include_memo: env_parse("ZC_INCLUDE_MEMO", false)?,
        merchant_name: env("ZC_MERCHANT_NAME"),
    };
    let bind = env("ZC_BIND").unwrap_or_else(|| "127.0.0.1:8080".into());
    // RENDER_EXTERNAL_URL: the public https URL Render.com gives a web service.
    let public_url = env("ZC_PUBLIC_URL")
        .or_else(|| env("RENDER_EXTERNAL_URL"))
        .unwrap_or_else(|| format!("http://{bind}"))
        .trim_end_matches('/')
        .to_string();
    let frame_ancestors = env("ZC_FRAME_ANCESTORS").unwrap_or_else(|| "'self'".into());
    if frame_ancestors.chars().any(|c| c == ';' || c == ',' || c.is_control()) {
        bail!("ZC_FRAME_ANCESTORS must be a space-separated list of origins");
    }
    let lwd_url = lightwalletd_url(&vk);
    let birthday = env("ZC_BIRTHDAY").map(|v| v.parse::<u64>()).transpose().context("ZC_BIRTHDAY")?;
    let poll = Duration::from_secs(env_parse("ZC_POLL_SECS", 15u64)?.max(2));
    let db = Db::open(&env("ZC_DB").unwrap_or_else(|| "zeccheckout.db".into()))?;

    tracing::info!(network = vk.network_name(), lightwalletd = %lwd_url, min_conf = settings.min_confirmations, "starting ZecCheckout (viewing key only)");
    let app = Arc::new(App::new(db, vk, settings));

    tokio::spawn(service::run_scanner(app.clone(), lwd_url, poll, birthday));

    match (env("ZC_WEBHOOK_URL"), env("ZC_WEBHOOK_SECRET")) {
        (Some(url), Some(secret)) => {
            if secret.len() < 24 {
                bail!("ZC_WEBHOOK_SECRET must be at least 24 characters");
            }
            let delivery = Delivery::new(url, secret.into_bytes())?;
            tokio::spawn(service::run_webhooks(app.clone(), delivery, Duration::from_secs(3)));
        }
        (Some(_), None) => bail!("ZC_WEBHOOK_URL requires ZC_WEBHOOK_SECRET"),
        _ => tracing::warn!("no ZC_WEBHOOK_URL configured; webhooks will be queued but not delivered"),
    }

    let state = Arc::new(ApiState {
        app,
        api_key,
        public_url,
        frame_ancestors,
    });
    let listener = tokio::net::TcpListener::bind(&bind).await.with_context(|| format!("binding {bind}"))?;
    tracing::info!("listening on http://{bind}");
    axum::serve(listener, api::router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::diversifier_start;

    #[test]
    fn diversifier_start_modes() {
        assert_eq!(diversifier_start(None, 5).unwrap(), 1_000_000);
        assert_eq!(diversifier_start(Some("42"), 5).unwrap(), 42);
        assert!(diversifier_start(Some("x"), 5).is_err());
        // Later boots start above anything an earlier boot could have used
        // (up to 1000 invoices per second of uptime).
        let a = diversifier_start(Some("boot"), 1_791_700_000).unwrap();
        let b = diversifier_start(Some(" boot "), 1_791_700_001).unwrap();
        assert!(b >= a + 1000);
    }
}

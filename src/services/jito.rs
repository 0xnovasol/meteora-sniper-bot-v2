//! Jito bundle submission and tip management via the Jito REST API.

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use once_cell::sync::Lazy;
use rand::seq::SliceRandom;
use serde::Deserialize;
use serde_json::json;
use solana_sdk::{pubkey::Pubkey, transaction::VersionedTransaction};
use std::{env, future::Future, str::FromStr, time::Duration};
use tokio::time::{sleep, Instant};

// ─── Configuration ────────────────────────────────────────────────────────────

pub static BLOCK_ENGINE_URL: Lazy<String> = Lazy::new(|| {
    env::var("JITO_BLOCK_ENGINE_URL")
        .unwrap_or_else(|_| "https://mainnet.block-engine.jito.wtf".to_string())
});

static KNOWN_TIP_ACCOUNTS: &[&str] = &[
    "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5",
    "HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe",
    "Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY",
    "ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt13qaRDsR",
    "DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh",
    "ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt",
    "DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL",
    "3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT",
];

// ─── Tip account pool ─────────────────────────────────────────────────────────

static TIP_ACCOUNTS: Lazy<tokio::sync::RwLock<Vec<Pubkey>>> =
    Lazy::new(|| tokio::sync::RwLock::new(Vec::new()));

pub async fn init_tip_accounts() -> Result<()> {
    let accounts: Vec<Pubkey> = KNOWN_TIP_ACCOUNTS
        .iter()
        .filter_map(|s| Pubkey::from_str(s).ok())
        .collect();
    if accounts.is_empty() {
        return Err(anyhow!("failed to parse Jito tip accounts"));
    }
    *TIP_ACCOUNTS.write().await = accounts;
    Ok(())
}

pub async fn get_tip_account() -> Result<Pubkey> {
    TIP_ACCOUNTS
        .read()
        .await
        .choose(&mut rand::thread_rng())
        .copied()
        .ok_or_else(|| anyhow!("tip accounts not initialised – call init_tip_accounts() first"))
}

pub async fn get_tip_value() -> Result<f64> {
    Ok(env::var("JITO_TIP_VALUE")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.004))
}

// ─── Bundle submission ────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct SendBundleResponse {
    result: Option<String>,
    error: Option<serde_json::Value>,
}

/// Serialize transactions to base-64 and submit them as a Jito bundle.
/// Returns the bundle ID.
pub async fn send_bundle(transactions: &[VersionedTransaction]) -> Result<String> {
    let encoded: Vec<String> = transactions
        .iter()
        .map(|tx| {
            let bytes = bincode::serialize(tx)
                .map_err(|e| anyhow!("failed to serialize tx: {}", e))?;
            Ok(B64.encode(bytes))
        })
        .collect::<Result<Vec<_>>>()?;

    let payload = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "sendBundle",
        "params": [encoded]
    });

    let url = format!("{}/api/v1/bundles", *BLOCK_ENGINE_URL);
    let resp: SendBundleResponse = reqwest::Client::new()
        .post(&url)
        .json(&payload)
        .send()
        .await?
        .json()
        .await?;

    if let Some(err) = resp.error {
        return Err(anyhow!("Jito sendBundle error: {}", err));
    }
    resp.result
        .ok_or_else(|| anyhow!("Jito sendBundle returned no bundle ID"))
}

// ─── Bundle status polling ────────────────────────────────────────────────────

#[derive(Deserialize)]
struct BundleStatusesResponse {
    result: Option<BundleStatusesResult>,
    error: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct BundleStatusesResult {
    value: Vec<Option<BundleStatusEntry>>,
}

#[derive(Deserialize)]
struct BundleStatusEntry {
    transactions: Vec<String>,
    #[serde(rename = "confirmation_status")]
    confirmation_status: Option<String>,
}

/// Public wrapper so `tx.rs` can poll without duplicating the HTTP logic.
pub async fn fetch_bundle_statuses_pub(bundle_id: &str) -> Result<Vec<String>> {
    fetch_bundle_statuses(bundle_id).await
}

async fn fetch_bundle_statuses(bundle_id: &str) -> Result<Vec<String>> {
    let payload = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getBundleStatuses",
        "params": [[bundle_id]]
    });

    let url = format!("{}/api/v1/bundles", *BLOCK_ENGINE_URL);
    let resp: BundleStatusesResponse = reqwest::Client::new()
        .post(&url)
        .json(&payload)
        .send()
        .await?
        .json()
        .await?;

    if let Some(err) = resp.error {
        return Err(anyhow!("Jito getBundleStatuses error: {}", err));
    }

    let sigs: Vec<String> = resp
        .result
        .map(|r| r.value)
        .unwrap_or_default()
        .into_iter()
        .flatten()
        .filter(|entry| {
            entry
                .confirmation_status
                .as_deref()
                .map(|s| s == "confirmed" || s == "finalized")
                .unwrap_or(false)
        })
        .flat_map(|entry| entry.transactions)
        .collect();

    Ok(sigs)
}

/// Poll for bundle confirmation, returning confirmed transaction signatures.
pub async fn wait_for_bundle_confirmation<F, Fut>(
    check_fn: F,
    bundle_id: String,
    poll_interval: Duration,
    timeout: Duration,
) -> Result<Vec<String>>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<Vec<String>>>,
{
    let start = Instant::now();
    loop {
        if start.elapsed() >= timeout {
            return Err(anyhow!("bundle {} timed out after {:?}", bundle_id, timeout));
        }
        match check_fn(bundle_id.clone()).await {
            Ok(sigs) if !sigs.is_empty() => return Ok(sigs),
            _ => {}
        }
        sleep(poll_interval).await;
    }
}

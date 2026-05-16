//! WebSocket monitor – listens for new Meteora DLMM pool creation and
//! immediately executes a buy swap.

use anyhow::{anyhow, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use solana_client::rpc_config::RpcTransactionConfig;
use solana_sdk::{
    commitment_config::CommitmentConfig, pubkey::Pubkey, signature::Signature,
};
use solana_transaction_status::UiTransactionEncoding;
use std::str::FromStr;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::{
    common::{
        logger::Logger,
        utils::{import_env_var_or, AppState, SwapConfig},
    },
    dex::meteora::{LbPairState, Meteora, METEORA_DLMM_PROGRAM_ID},
    engine::swap::{SwapDirection, SwapInType},
};

// ─── Entry point ─────────────────────────────────────────────────────────────

/// Connect to `rpc_wss`, subscribe to Meteora DLMM logs, and react to every
/// new `InitializeLbPair` event by buying into the pool.
pub async fn meteora_monitor(
    rpc_wss: &str,
    state: AppState,
    slippage: u64,
    use_jito: bool,
) {
    let logger = Logger::new("[MONITOR] => ".to_string());
    loop {
        logger.log(format!("connecting to {}", rpc_wss));
        match run_monitor(rpc_wss, state.clone(), slippage, use_jito, &logger).await {
            Ok(_) => logger.log("stream ended – reconnecting".to_string()),
            Err(e) => logger.error(format!("error: {} – reconnecting", e)),
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    }
}

// ─── Inner loop ──────────────────────────────────────────────────────────────

async fn run_monitor(
    rpc_wss: &str,
    state: AppState,
    slippage: u64,
    use_jito: bool,
    logger: &Logger,
) -> Result<()> {
    let (ws_stream, _) = connect_async(rpc_wss)
        .await
        .map_err(|e| anyhow!("WebSocket connect failed: {}", e))?;

    let (mut write, mut read) = ws_stream.split();

    let sub = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "logsSubscribe",
        "params": [
            { "mentions": [METEORA_DLMM_PROGRAM_ID] },
            { "commitment": "processed" }
        ]
    });
    write.send(Message::Text(sub.to_string())).await?;
    logger.log("subscribed to Meteora DLMM logs".to_string());

    while let Some(msg) = read.next().await {
        let msg = msg?;
        let text = match msg {
            Message::Text(t) => t,
            Message::Ping(p) => {
                write.send(Message::Pong(p)).await.ok();
                continue;
            }
            Message::Close(_) => return Ok(()),
            _ => continue,
        };

        let notification: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Skip subscription confirmations
        if notification.get("result").is_some() && notification.get("method").is_none() {
            continue;
        }

        let value = match notification["params"]["result"]["value"].as_object() {
            Some(v) => v,
            None => continue,
        };

        // Skip failed transactions
        if !value.get("err").map(|e| e.is_null()).unwrap_or(false) {
            continue;
        }

        let logs = match value.get("logs").and_then(|l| l.as_array()) {
            Some(l) => l,
            None => continue,
        };

        let is_new_pool = logs.iter().any(|l| {
            l.as_str()
                .map(|s| s.contains("Instruction: InitializeLbPair"))
                .unwrap_or(false)
        });

        if !is_new_pool {
            continue;
        }

        let signature = match value.get("signature").and_then(|s| s.as_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };

        logger.log(format!("new pool detected  tx={}", signature));

        let state_clone = state.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_new_pool(signature, state_clone, slippage, use_jito).await {
                eprintln!("[SNIPE] error: {}", e);
            }
        });
    }

    Ok(())
}

// ─── Pool handler ─────────────────────────────────────────────────────────────

async fn handle_new_pool(
    signature: String,
    state: AppState,
    slippage: u64,
    use_jito: bool,
) -> Result<()> {
    let logger = Logger::new("[SNIPE] => ".to_string());

    let lb_pair = resolve_lb_pair(&signature, &state).await?;
    logger.log(format!("lb_pair={}", lb_pair));

    let lb_pair_account = state
        .rpc_nonblocking_client
        .get_account(&lb_pair)
        .await
        .map_err(|e| anyhow!("failed to fetch LbPair: {}", e))?
        .ok_or_else(|| anyhow!("LbPair account {} not found", lb_pair))?;
    let lb_pair_data = lb_pair_account.data;

    let pool_state = LbPairState::from_account_data(&lb_pair_data)?;

    if !pool_state.is_sol_pair() {
        logger.log(format!(
            "skipping – no SOL in pool (x={} y={})",
            pool_state.token_x_mint, pool_state.token_y_mint
        ));
        return Ok(());
    }

    let buy_sol: f64 = import_env_var_or("BUY_AMOUNT", "0.1")
        .parse()
        .unwrap_or(0.1);
    let amount_in = spl_token::ui_amount_to_amount(buy_sol, spl_token::native_mint::DECIMALS);

    let swap_config = SwapConfig {
        swap_direction: SwapDirection::Buy,
        in_type: SwapInType::Qty,
        amount_in,
        slippage,
        use_jito,
    };

    let meteora = Meteora::new(
        state.rpc_nonblocking_client.clone(),
        state.rpc_client.clone(),
        state.wallet.clone(),
    );

    match meteora.swap(&lb_pair.to_string(), swap_config).await {
        Ok(sigs) => logger.log(format!("swap confirmed  sigs={:?}", sigs)),
        Err(e) => logger.error(format!("swap failed: {}", e)),
    }

    Ok(())
}

// ─── Transaction parser ───────────────────────────────────────────────────────

/// Fetch the confirmed transaction and return the lb_pair account address.
///
/// For `InitializeLbPair`, the Meteora DLMM IDL places the lb_pair as the
/// first non-signer writable account (index 0 in the instruction accounts).
async fn resolve_lb_pair(signature: &str, state: &AppState) -> Result<Pubkey> {
    let sig = Signature::from_str(signature)
        .map_err(|e| anyhow!("invalid signature {}: {}", signature, e))?;

    let tx = state
        .rpc_nonblocking_client
        .get_transaction_with_config(
            &sig,
            RpcTransactionConfig {
                encoding: Some(UiTransactionEncoding::JsonParsed),
                commitment: Some(CommitmentConfig::confirmed()),
                max_supported_transaction_version: Some(0),
            },
        )
        .await
        .map_err(|e| anyhow!("getTransaction failed for {}: {}", signature, e))?;

    // Extract all account keys and find the first writable non-signer that is
    // not a well-known system/token program account.
    let skip_list = [
        METEORA_DLMM_PROGRAM_ID,
        "11111111111111111111111111111111",   // system
        "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",  // SPL Token
        "SysvarRent111111111111111111111111111111111",
        "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL", // ATA
    ];

    use solana_transaction_status::{EncodedTransaction, UiMessage};

    let addr = match &tx.transaction.transaction {
        EncodedTransaction::Json(ui_tx) => match &ui_tx.message {
            UiMessage::Parsed(parsed) => parsed
                .account_keys
                .iter()
                .find(|ak| {
                    ak.writable && !ak.signer && !skip_list.contains(&ak.pubkey.as_str())
                })
                .map(|ak| ak.pubkey.clone()),
            UiMessage::Raw(raw) => raw
                .account_keys
                .iter()
                .find(|pk| !skip_list.contains(&pk.as_str()))
                .cloned(),
        },
        _ => None,
    };

    let addr = addr.ok_or_else(|| anyhow!("could not locate lb_pair in tx {}", signature))?;
    Pubkey::from_str(&addr).map_err(|e| anyhow!("invalid lb_pair pubkey {}: {}", addr, e))
}

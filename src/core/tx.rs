use std::{env, time::Duration};

use anyhow::Result;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    instruction::Instruction,
    signature::Keypair,
    signer::Signer,
    system_transaction,
    transaction::{Transaction, VersionedTransaction},
};
use spl_token::ui_amount_to_amount;
use std::str::FromStr;
use tokio::time::Instant;

use crate::{
    common::{logger::Logger, rpc},
    services::jito::{self, get_tip_account, get_tip_value, send_bundle, wait_for_bundle_confirmation},
};

fn get_unit_price() -> u64 {
    env::var("UNIT_PRICE")
        .ok()
        .and_then(|v| u64::from_str(&v).ok())
        .unwrap_or(1)
}

fn get_unit_limit() -> u32 {
    env::var("UNIT_LIMIT")
        .ok()
        .and_then(|v| u32::from_str(&v).ok())
        .unwrap_or(300_000)
}

pub async fn new_signed_and_send(
    client: &RpcClient,
    keypair: &Keypair,
    mut instructions: Vec<Instruction>,
    use_jito: bool,
    logger: &Logger,
) -> Result<Vec<String>> {
    let unit_price = get_unit_price();
    let unit_limit = get_unit_limit();

    if !use_jito {
        instructions.insert(
            0,
            solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_price(
                unit_price,
            ),
        );
        instructions.insert(
            1,
            solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(
                unit_limit,
            ),
        );
    }

    let recent_blockhash = client.get_latest_blockhash()?;
    let txn = Transaction::new_signed_with_payer(
        &instructions,
        Some(&keypair.pubkey()),
        &vec![keypair],
        recent_blockhash,
    );

    let start_time = Instant::now();
    let mut txs = vec![];

    if use_jito {
        let tip_account = get_tip_account().await?;

        let mut tip = get_tip_value().await?;
        tip = tip.min(0.1);
        let tip_lamports = ui_amount_to_amount(tip, spl_token::native_mint::DECIMALS);

        logger.log(format!(
            "tip account: {}, tip(sol): {}, lamports: {}",
            tip_account, tip, tip_lamports
        ));

        let bundle: Vec<VersionedTransaction> = vec![
            VersionedTransaction::from(txn),
            VersionedTransaction::from(system_transaction::transfer(
                keypair,
                &tip_account,
                tip_lamports,
                recent_blockhash,
            )),
        ];

        let bundle_id = send_bundle(&bundle).await?;
        logger.log(format!("bundle_id: {}", bundle_id));

        txs = wait_for_bundle_confirmation(
            move |id: String| async move { jito::fetch_bundle_statuses_pub(&id).await },
            bundle_id,
            Duration::from_millis(1000),
            Duration::from_secs(10),
        )
        .await?;
    } else {
        let sig = rpc::send_txn(client, &txn, true)?;
        logger.log(format!("signature: {:#?}", sig));
        txs.push(sig.to_string());
    }

    logger.log(format!("tx elapsed: {:?}", start_time.elapsed()));
    Ok(txs)
}

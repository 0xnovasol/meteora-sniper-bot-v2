use anyhow::Result;
use solana_client::{
    rpc_client::RpcClient,
    rpc_config::RpcProgramAccountsConfig,
    rpc_filter::RpcFilterType,
};
use solana_sdk::{
    pubkey::Pubkey,
    signature::Signature,
    transaction::Transaction,
};

/// Send a signed transaction, optionally skipping preflight checks.
pub fn send_txn(client: &RpcClient, txn: &Transaction, skip_preflight: bool) -> Result<Signature> {
    let config = solana_client::rpc_config::RpcSendTransactionConfig {
        skip_preflight,
        ..Default::default()
    };
    let sig = client.send_transaction_with_config(txn, config)?;
    Ok(sig)
}

/// Fetch the raw account data for a single pubkey, returning `None` when not found.
pub fn get_account(client: &RpcClient, pubkey: &Pubkey) -> Result<Option<Vec<u8>>> {
    match client.get_account_data(pubkey) {
        Ok(data) => Ok(Some(data)),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("AccountNotFound") || msg.contains("could not find account") {
                Ok(None)
            } else {
                Err(e.into())
            }
        }
    }
}

/// Fetch all program accounts that match an optional set of filters.
pub fn get_program_accounts_with_filters(
    client: &RpcClient,
    program_id: Pubkey,
    filters: Option<Vec<RpcFilterType>>,
) -> Result<Vec<(Pubkey, solana_sdk::account::Account)>> {
    let config = RpcProgramAccountsConfig {
        filters,
        account_config: solana_client::rpc_config::RpcAccountInfoConfig {
            encoding: Some(solana_account_decoder::UiAccountEncoding::Base64),
            ..Default::default()
        },
        ..Default::default()
    };
    let accounts = client.get_program_accounts_with_config(&program_id, config)?;
    Ok(accounts)
}

use dotenv::dotenv;
use meteora_sniper_bot::{
    common::{
        logger::Logger,
        utils::{
            create_nonblocking_rpc_client, create_rpc_client, import_env_var, import_env_var_or,
            import_wallet, AppState,
        },
    },
    engine::monitor::meteora_monitor,
    services::jito,
};
use solana_sdk::signer::Signer;

#[tokio::main]
async fn main() {
    let logger = Logger::new("[INIT] => ".to_string());

    dotenv().ok();

    let rpc_wss = import_env_var("RPC_WSS");
    let rpc_client = create_rpc_client().expect("failed to create RPC client");
    let rpc_nonblocking_client = create_nonblocking_rpc_client()
        .await
        .expect("failed to create async RPC client");
    let wallet = import_wallet().expect("failed to load wallet");
    let wallet_pubkey = wallet.pubkey();

    let state = AppState {
        rpc_client,
        rpc_nonblocking_client,
        wallet,
    };

    let slippage = import_env_var_or("SLIPPAGE", "10")
        .parse::<u64>()
        .unwrap_or(10);
    let use_jito: bool = import_env_var_or("USE_JITO", "true")
        .parse()
        .unwrap_or(true);

    if use_jito {
        jito::init_tip_accounts()
            .await
            .expect("failed to initialise Jito tip accounts");
    }

    logger.log(format!(
        "Meteora Sniper Bot started\n\
         \t\t\t\t [WS RPC]:   {}\n\
         \t\t\t\t [Wallet]:   {}\n\
         \t\t\t\t [Slippage]: {}%\n\
         \t\t\t\t [Jito]:     {}",
        rpc_wss, wallet_pubkey, slippage, use_jito
    ));

    meteora_monitor(&rpc_wss, state, slippage, use_jito).await;
}

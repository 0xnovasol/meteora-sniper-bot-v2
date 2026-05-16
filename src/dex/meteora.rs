//! Meteora DLMM (Dynamic Liquidity Market Maker) swap integration.
//!
//! Program: LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo
//! Docs:    https://docs.meteora.ag/

use anyhow::{anyhow, Result};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
};
use spl_associated_token_account::instruction::create_associated_token_account;
use std::{str::FromStr, sync::Arc};

use crate::{
    common::{logger::Logger, utils::SwapConfig},
    core::{token, tx},
    engine::swap::SwapDirection,
};

// ─── Constants ────────────────────────────────────────────────────────────────

pub const METEORA_DLMM_PROGRAM_ID: &str = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo";

/// Number of bins packed into a single BinArray account.
const BIN_ARRAY_SIZE: i64 = 70;

/// Anchor instruction discriminator for the `swap` instruction.
/// Derived from: sha256("global:swap")[0..8]
const SWAP_DISCRIMINATOR: [u8; 8] = [248, 198, 158, 145, 225, 117, 135, 200];

// ─── LbPair state ─────────────────────────────────────────────────────────────

/// Key fields extracted from a Meteora DLMM LbPair account.
///
/// Byte offsets (including 8-byte Anchor discriminator):
///   active_id   : 126
///   bin_step    : 130
///   token_x_mint: 138
///   token_y_mint: 170
///   reserve_x   : 202
///   reserve_y   : 234
///   oracle      : 314
#[derive(Debug, Clone)]
pub struct LbPairState {
    pub active_id: i32,
    pub bin_step: u16,
    pub token_x_mint: Pubkey,
    pub token_y_mint: Pubkey,
    pub reserve_x: Pubkey,
    pub reserve_y: Pubkey,
    pub oracle: Pubkey,
}

impl LbPairState {
    pub fn from_account_data(data: &[u8]) -> Result<Self> {
        if data.len() < 346 {
            return Err(anyhow!(
                "LbPair account data too short: {} bytes",
                data.len()
            ));
        }

        let active_id = i32::from_le_bytes(data[126..130].try_into()?);
        let bin_step = u16::from_le_bytes(data[130..132].try_into()?);
        let token_x_mint = Pubkey::try_from(&data[138..170])?;
        let token_y_mint = Pubkey::try_from(&data[170..202])?;
        let reserve_x = Pubkey::try_from(&data[202..234])?;
        let reserve_y = Pubkey::try_from(&data[234..266])?;
        let oracle = Pubkey::try_from(&data[314..346])?;

        Ok(Self {
            active_id,
            bin_step,
            token_x_mint,
            token_y_mint,
            reserve_x,
            reserve_y,
            oracle,
        })
    }

    /// True when one side of the pair is native SOL.
    pub fn is_sol_pair(&self) -> bool {
        let sol = spl_token::native_mint::ID;
        self.token_x_mint == sol || self.token_y_mint == sol
    }

    /// Return (sol_reserve, token_mint, token_reserve, swap_for_y).
    /// `swap_for_y = true` means we spend X (SOL) to receive Y (token).
    pub fn sol_token_layout(&self) -> Option<(Pubkey, Pubkey, Pubkey, bool)> {
        let sol = spl_token::native_mint::ID;
        if self.token_x_mint == sol {
            Some((self.reserve_x, self.token_y_mint, self.reserve_y, true))
        } else if self.token_y_mint == sol {
            Some((self.reserve_y, self.token_x_mint, self.reserve_x, false))
        } else {
            None
        }
    }
}

// ─── BinArray PDAs ────────────────────────────────────────────────────────────

/// Derive the PDA for a BinArray at the given index.
pub fn bin_array_pda(lb_pair: &Pubkey, index: i64, program_id: &Pubkey) -> Pubkey {
    let (pda, _) = Pubkey::find_program_address(
        &[b"bin_array", lb_pair.as_ref(), &index.to_le_bytes()],
        program_id,
    );
    pda
}

/// Return the BinArray index that contains `bin_id`.
pub fn bin_array_index_for_bin(bin_id: i32) -> i64 {
    (bin_id as i64).div_euclid(BIN_ARRAY_SIZE)
}

/// Compute the 2–3 BinArray PDAs needed for a swap starting at `active_id`.
/// Pass `swap_for_y = true` when spending token X (e.g. SOL) for token Y.
pub fn bin_arrays_for_swap(
    lb_pair: &Pubkey,
    active_id: i32,
    swap_for_y: bool,
    program_id: &Pubkey,
) -> Vec<Pubkey> {
    let base = bin_array_index_for_bin(active_id);
    let offsets: &[i64] = if swap_for_y {
        &[0, 1, 2]
    } else {
        &[0, -1, -2]
    };
    offsets
        .iter()
        .map(|&o| bin_array_pda(lb_pair, base + o, program_id))
        .collect()
}

// ─── Instruction builder ──────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn build_swap_instruction(
    program_id: &Pubkey,
    lb_pair: &Pubkey,
    state: &LbPairState,
    user_token_in: &Pubkey,
    user_token_out: &Pubkey,
    user: &Pubkey,
    bin_arrays: &[Pubkey],
    amount_in: u64,
    min_amount_out: u64,
    swap_for_y: bool,
) -> Instruction {
    let (event_authority, _) =
        Pubkey::find_program_address(&[b"__event_authority"], program_id);

    let mut accounts = vec![
        AccountMeta::new(*lb_pair, false),
        AccountMeta::new(state.reserve_x, false),
        AccountMeta::new(state.reserve_y, false),
        AccountMeta::new(*user_token_in, false),
        AccountMeta::new(*user_token_out, false),
        AccountMeta::new_readonly(state.token_x_mint, false),
        AccountMeta::new_readonly(state.token_y_mint, false),
        AccountMeta::new(state.oracle, false),
        AccountMeta::new_readonly(*user, true),
        AccountMeta::new_readonly(spl_token::ID, false),
        AccountMeta::new_readonly(spl_token::ID, false),
        AccountMeta::new_readonly(event_authority, false),
        AccountMeta::new_readonly(*program_id, false),
    ];

    for ba in bin_arrays {
        accounts.push(AccountMeta::new(*ba, false));
    }

    let mut data = SWAP_DISCRIMINATOR.to_vec();
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&min_amount_out.to_le_bytes());
    data.push(u8::from(swap_for_y));

    Instruction {
        program_id: *program_id,
        accounts,
        data,
    }
}

// ─── Meteora swap executor ───────────────────────────────────────────────────

pub struct Meteora {
    pub rpc_nonblocking_client: Arc<solana_client::nonblocking::rpc_client::RpcClient>,
    pub rpc_client: Arc<solana_client::rpc_client::RpcClient>,
    pub keypair: Arc<Keypair>,
}

impl Meteora {
    pub fn new(
        rpc_nonblocking_client: Arc<solana_client::nonblocking::rpc_client::RpcClient>,
        rpc_client: Arc<solana_client::rpc_client::RpcClient>,
        keypair: Arc<Keypair>,
    ) -> Self {
        Self {
            rpc_nonblocking_client,
            rpc_client,
            keypair,
        }
    }

    /// Execute a swap against a Meteora DLMM pool.
    ///
    /// * `lb_pair_address` – base-58 address of the LbPair account
    /// * `swap_config`     – amount, direction, slippage, Jito flag
    pub async fn swap(
        &self,
        lb_pair_address: &str,
        swap_config: SwapConfig,
    ) -> Result<Vec<String>> {
        let logger = Logger::new("[METEORA SWAP] => ".to_string());

        let program_id = Pubkey::from_str(METEORA_DLMM_PROGRAM_ID)?;
        let lb_pair = Pubkey::from_str(lb_pair_address)?;
        let owner = self.keypair.pubkey();
        let native_mint = spl_token::native_mint::ID;

        // ── Fetch and parse the LbPair account ───────────────────────────────
        let lb_pair_account = self
            .rpc_nonblocking_client
            .get_account(&lb_pair)
            .await
            .map_err(|e| anyhow!("failed to fetch LbPair {}: {}", lb_pair, e))?
            .ok_or_else(|| anyhow!("LbPair account not found: {}", lb_pair))?;
        let lb_pair_data = lb_pair_account.data;

        let state = LbPairState::from_account_data(&lb_pair_data)?;
        logger.log(format!(
            "pool  active_id={} bin_step={} x={} y={}",
            state.active_id, state.bin_step, state.token_x_mint, state.token_y_mint
        ));

        // ── Determine swap direction ─────────────────────────────────────────
        let (token_in, token_out, swap_for_y) = match swap_config.swap_direction {
            SwapDirection::Buy => {
                // Spend SOL to acquire the new token
                let (_, token_mint, _, sfy) = state
                    .sol_token_layout()
                    .ok_or_else(|| anyhow!("pool does not contain native SOL"))?;
                (native_mint, token_mint, sfy)
            }
            SwapDirection::Sell => {
                let (_, token_mint, _, sfy) = state
                    .sol_token_layout()
                    .ok_or_else(|| anyhow!("pool does not contain native SOL"))?;
                (token_mint, native_mint, !sfy)
            }
        };

        // ── Compute slippage ─────────────────────────────────────────────────
        let slippage_bps = swap_config.slippage * 100;
        let min_amount_out = swap_config
            .amount_in
            .saturating_mul(10_000u64.saturating_sub(slippage_bps))
            / 10_000;

        // ── Resolve / create ATAs ────────────────────────────────────────────
        let user_token_in = token::get_associated_token_address(
            self.rpc_nonblocking_client.clone(),
            self.keypair.clone(),
            &token_in,
            &owner,
        );
        let user_token_out = token::get_associated_token_address(
            self.rpc_nonblocking_client.clone(),
            self.keypair.clone(),
            &token_out,
            &owner,
        );

        let mut instructions: Vec<solana_sdk::instruction::Instruction> = vec![];

        // Create output ATA if it does not yet exist
        if token::get_account_info(
            self.rpc_nonblocking_client.clone(),
            self.keypair.clone(),
            &token_out,
            &user_token_out,
        )
        .await
        .is_err()
        {
            instructions.push(create_associated_token_account(
                &owner,
                &owner,
                &token_out,
                &spl_token::ID,
            ));
        }

        // ── BinArray accounts ────────────────────────────────────────────────
        let bin_arrays =
            bin_arrays_for_swap(&lb_pair, state.active_id, swap_for_y, &program_id);

        logger.log(format!(
            "swap  in={} out={} amount={} min_out={} swap_for_y={}",
            token_in, token_out, swap_config.amount_in, min_amount_out, swap_for_y
        ));

        // ── Build and send swap instruction ──────────────────────────────────
        let swap_ix = build_swap_instruction(
            &program_id,
            &lb_pair,
            &state,
            &user_token_in,
            &user_token_out,
            &owner,
            &bin_arrays,
            swap_config.amount_in,
            min_amount_out,
            swap_for_y,
        );
        instructions.push(swap_ix);

        tx::new_signed_and_send(
            &self.rpc_client,
            &self.keypair,
            instructions,
            swap_config.use_jito,
            &logger,
        )
        .await
    }
}

// ─── Pool info (Meteora DLMM REST API) ────────────────────────────────────────

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct PoolApiResponse {
    pub address: String,
    pub name: String,
    pub mint_x: String,
    pub mint_y: String,
    pub reserve_x_amount: u64,
    pub reserve_y_amount: u64,
    pub bin_step: u16,
    pub base_fee_percentage: String,
    pub current_price: f64,
}

/// Fetch pool details from the Meteora DLMM REST API.
pub async fn get_pool_info(lb_pair_address: &str) -> Result<PoolApiResponse> {
    let url = format!("https://dlmm-api.meteora.ag/pair/{}", lb_pair_address);
    let resp = reqwest::get(&url)
        .await?
        .json::<PoolApiResponse>()
        .await
        .map_err(|e| anyhow!("failed to parse Meteora pool API response: {}", e))?;
    Ok(resp)
}

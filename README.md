# Meteora DLMM Sniper Bot

A Rust bot that monitors the Solana blockchain for newly created [Meteora DLMM](https://docs.meteora.ag/) liquidity pools and immediately executes a buy swap into them.

## How it works

1. **Monitor** – subscribes to the Meteora DLMM program (`LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo`) via a Solana WebSocket `logsSubscribe` connection.
2. **Detect** – filters for `InitializeLbPair` log messages, which are emitted when a new pool is created.
3. **Validate** – fetches the `LbPair` account and checks that one of the mints is native SOL (only SOL-paired pools are sniped).
4. **Swap** – constructs a Meteora DLMM `swap` instruction and submits it via **Jito bundles** (default) or standard RPC.

## Setup

### Prerequisites

- Rust 1.75+ (`rustup update stable`)
- A Solana wallet with SOL for trades and transaction fees
- A premium WebSocket RPC endpoint (Helius, QuickNode, etc.) for low-latency detection

### Install

```bash
git clone <repo>
cd meteora-sniper-bot
cp .env.example .env
# fill in your .env values
cargo build --release
```

### Run

```bash
cargo run --release
```

## Configuration

| Variable | Description | Default |
|---|---|---|
| `RPC_HTTPS` | HTTP RPC endpoint | – |
| `RPC_WSS` | WebSocket RPC endpoint | – |
| `PRIVATE_KEY` | Base-58 encoded wallet private key | – |
| `BUY_AMOUNT` | SOL to spend per new pool | `0.1` |
| `SLIPPAGE` | Slippage tolerance (%) | `10` |
| `USE_JITO` | Use Jito bundle submission | `true` |
| `JITO_BLOCK_ENGINE_URL` | Jito block engine base URL | `https://mainnet.block-engine.jito.wtf` |
| `JITO_TIP_VALUE` | Fixed Jito tip in SOL | `0.004` |
| `UNIT_PRICE` | Compute unit price (non-Jito) | `100000` |
| `UNIT_LIMIT` | Compute unit limit (non-Jito) | `300000` |

## Architecture

```
src/
├── main.rs                      # Entry point
├── common/
│   ├── logger.rs                # Timestamped logger
│   ├── rpc.rs                   # RPC helpers
│   └── utils.rs                 # AppState, SwapConfig
├── core/
│   ├── token.rs                 # SPL token helpers
│   └── tx.rs                    # Transaction signing & sending
├── dex/
│   └── meteora.rs               # Meteora DLMM swap + LbPair parsing
├── engine/
│   ├── swap.rs                  # SwapDirection, SwapInType types
│   └── monitor/
│       └── meteora.rs           # WebSocket pool detector
└── services/
    ├── jito.rs                  # Jito bundle service
    └── nextblock.rs             # NextBlock stub
```

## Notes

- **Bin array offsets** – the `LbPairState::from_account_data` parser uses hard-coded byte offsets derived from the Meteora DLMM IDL. If the program is upgraded these may need updating.
- **DLMM swap discriminator** – `[248, 198, 158, 145, 225, 117, 135, 200]` (Anchor `sha256("global:swap")[0..8]`). Verify against the live IDL before deploying.
- This software is provided for educational purposes. Use at your own risk.

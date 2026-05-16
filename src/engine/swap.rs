#[derive(Debug, Clone, PartialEq)]
pub enum SwapDirection {
    /// Buy: spend SOL to acquire the new token
    Buy,
    /// Sell: spend the token to reclaim SOL
    Sell,
}

/// How `amount_in` is interpreted
#[derive(Debug, Clone, PartialEq)]
pub enum SwapInType {
    /// Exact token/lamport quantity
    Qty,
    /// Percentage of current wallet balance (0–100)
    Pct,
}

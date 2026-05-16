/// NextBlock transaction submission (placeholder – not yet implemented).
///
/// To enable NextBlock support, set `NEXTBLOCK_AUTH_TOKEN` in your `.env`
/// and implement the HTTP submission logic here.
pub async fn send_transaction(_tx_base64: &str) -> anyhow::Result<String> {
    anyhow::bail!("NextBlock support is not yet implemented")
}

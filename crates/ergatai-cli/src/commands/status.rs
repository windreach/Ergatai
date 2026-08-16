use crate::client::http::ErgataiClient;
use crate::output::formatter;
use anyhow::{bail, Result};

pub async fn handle(watch: bool, api_url: &str, token: Option<&str>) -> Result<()> {
    if watch {
        bail!("WebSocket watch mode is not yet implemented; use regular status command for now");
    }

    let client = ErgataiClient::new(api_url, token);
    let status = client.get_status().await?;
    formatter::format_status(&status);

    Ok(())
}

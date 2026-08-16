use anyhow::Result;
use crate::client::http::ErgataiClient;
use crate::output::formatter;

pub async fn handle(watch: bool, api_url: &str, token: Option<&str>) -> Result<()> {
    if watch {
        println!("WebSocket watch mode not yet implemented");
        println!("Use regular status command for now");
        return Ok(());
    }

    let client = ErgataiClient::new(api_url, token);
    let status = client.get_status().await?;
    formatter::format_status(&status);

    Ok(())
}

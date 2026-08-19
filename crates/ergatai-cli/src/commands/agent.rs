use crate::client::http::ErgataiClient;
use crate::output::formatter;
use crate::AgentAction;
use anyhow::Result;

pub async fn handle(action: AgentAction, api_url: &str, token: Option<&str>) -> Result<()> {
    let client = ErgataiClient::new(api_url, token);

    match action {
        AgentAction::List => {
            let agents = client.list_agents().await?;
            formatter::format_agents_table(&agents);
        }
        AgentAction::Spawn {
            workspace,
            command,
            instruction,
        } => {
            let response = client
                .spawn_agent(&workspace, &command, None, instruction.as_deref())
                .await?;
            println!("Spawned agent: {}", response.agent_id);
        }
        AgentAction::Kill { id } => {
            client.kill_agent(&id).await?;
            println!("Stopped agent: {}", id);
        }
        AgentAction::Message { id, message } => {
            client.send_message(&id, &message).await?;
            println!("Message sent to agent: {}", id);
        }
    }

    Ok(())
}

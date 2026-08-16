use anyhow::Result;
use crate::client::http::ErgataiClient;
use crate::output::formatter;
use crate::WorkspaceAction;

pub async fn handle(action: WorkspaceAction, api_url: &str, token: Option<&str>) -> Result<()> {
    let client = ErgataiClient::new(api_url, token);

    match action {
        WorkspaceAction::List => {
            let workspaces = client.list_workspaces().await?;
            formatter::format_workspaces_table(&workspaces);
        }
        WorkspaceAction::Create { id, work_dir } => {
            let workspace = client.create_workspace(&id, work_dir.as_deref()).await?;
            println!("Created workspace: {}", workspace.id);
        }
        WorkspaceAction::Delete { id } => {
            client.delete_workspace(&id).await?;
            println!("Deleted workspace: {}", id);
        }
    }

    Ok(())
}

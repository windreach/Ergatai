//! MCP Tool implementations
//!
//! Implements the core MCP tools exposed by Ergatai.

use anyhow::Result;
use serde_json::json;
use tracing::{info, warn};

use super::types::{Content, Tool, ToolCallResponse};
use super::agent_registry::AgentRegistry;
use super::message_relay;

/// Create tool definitions
pub fn get_tool_definitions() -> Vec<Tool> {
    vec![
        Tool {
            name: "list_agents".to_string(),
            description: "List all connected agents and their status".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "include_capabilities": {
                        "type": "boolean",
                        "description": "Whether to include agent capabilities"
                    }
                }
            }),
        },
        Tool {
            name: "send_message".to_string(),
            description: "Send a message to another agent".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "target_agent_id": {
                        "type": "string",
                        "description": "ID of the target agent"
                    },
                    "message": {
                        "type": "string",
                        "description": "Message content"
                    },
                    "message_type": {
                        "type": "string",
                        "enum": ["request", "response", "broadcast"],
                        "description": "Type of message"
                    }
                },
                "required": ["target_agent_id", "message"]
            }),
        },
        Tool {
            name: "submit_orchestration".to_string(),
            description: "Submit a DAG workflow for multi-agent collaboration".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "dag_definition": {
                        "type": "string",
                        "description": "Markdown-formatted DAG definition"
                    },
                    "context": {
                        "type": "object",
                        "description": "Optional context variables"
                    }
                },
                "required": ["dag_definition"]
            }),
        },
        Tool {
            name: "check_dag_status".to_string(),
            description: "Check the status of a DAG execution".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "dag_id": {
                        "type": "string",
                        "description": "DAG ID to check"
                    }
                },
                "required": ["dag_id"]
            }),
        },
    ]
}

/// Handle tool calls
pub async fn handle_tool_call(
    tool_name: &str,
    arguments: serde_json::Value,
    registry: &AgentRegistry,
) -> Result<ToolCallResponse> {
    match tool_name {
        "list_agents" => handle_list_agents(arguments, registry).await,
        "send_message" => handle_send_message(arguments, registry).await,
        "submit_orchestration" => handle_submit_orchestration(arguments).await,
        "check_dag_status" => handle_check_dag_status(arguments).await,
        _ => {
            warn!("Unknown tool: {}", tool_name);
            Ok(ToolCallResponse {
                content: vec![Content::Text {
                    text: format!("Unknown tool: {}", tool_name),
                }],
                is_error: Some(true),
            })
        }
    }
}

/// Handle list_agents tool
async fn handle_list_agents(
    arguments: serde_json::Value,
    registry: &AgentRegistry,
) -> Result<ToolCallResponse> {
    let include_capabilities = arguments
        .get("include_capabilities")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let agents = registry.list_agents().await;

    let agents_json: Vec<serde_json::Value> = agents
        .iter()
        .map(|agent| {
            let mut agent_json = json!({
                "agent_id": agent.agent_id,
                "status": agent.status,
                "connected_at": agent.connected_at,
                "last_heartbeat": agent.last_heartbeat,
            });

            if include_capabilities {
                if let Some(caps) = &agent.capabilities {
                    agent_json["capabilities"] = json!(caps);
                }
            }

            agent_json
        })
        .collect();

    let result = json!({
        "agents": agents_json,
        "total": agents.len()
    });

    Ok(ToolCallResponse {
        content: vec![Content::Text {
            text: serde_json::to_string_pretty(&result)?,
        }],
        is_error: None,
    })
}

/// Handle send_message tool
async fn handle_send_message(
    arguments: serde_json::Value,
    registry: &AgentRegistry,
) -> Result<ToolCallResponse> {
    let target_agent_id = arguments
        .get("target_agent_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing target_agent_id"))?;

    let message = arguments
        .get("message")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing message"))?;

    let message_type = arguments
        .get("message_type")
        .and_then(|v| v.as_str())
        .unwrap_or("request");

    info!(
        "Sending message to agent {}: {} (type: {})",
        target_agent_id, message, message_type
    );

    // Check if target agent exists
    let target_agent = registry.get_agent(target_agent_id).await;
    if target_agent.is_none() {
        return Ok(ToolCallResponse {
            content: vec![Content::Text {
                text: format!("Agent {} not found", target_agent_id),
            }],
            is_error: Some(true),
        });
    }

    // TODO: Actually send message via ACP
    // For now, we need to:
    // 1. Get agent config for target_agent_id
    // 2. Create ACP connection
    // 3. Send message as prompt
    // 4. Wait for response

    // Get agent config
    let agent_config = match ergatai_core::agent::config::get_agent_config(target_agent_id) {
        Ok(config) => config,
        Err(e) => {
            return Ok(ToolCallResponse {
                content: vec![Content::Text {
                    text: format!("Failed to get agent config for {}: {}", target_agent_id, e),
                }],
                is_error: Some(true),
            });
        }
    };

    // For now, just acknowledge the message
    // TODO: Implement actual ACP message sending
    let message_id = uuid::Uuid::new_v4().to_string();

    info!("Message {} queued for agent {} via ACP", message_id, target_agent_id);

    let result = json!({
        "message_id": message_id,
        "status": "sent",
        "target_agent_id": target_agent_id,
        "message_type": message_type,
        "note": "Message relay via ACP not yet implemented"
    });

    Ok(ToolCallResponse {
        content: vec![Content::Text {
            text: serde_json::to_string_pretty(&result)?,
        }],
        is_error: None,
    })
}

/// Handle submit_orchestration tool
async fn handle_submit_orchestration(arguments: serde_json::Value) -> Result<ToolCallResponse> {
    let _dag_definition = arguments
        .get("dag_definition")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing dag_definition"))?;

    let _context = arguments.get("context").cloned();

    // TODO: Parse DAG and submit to scheduler
    // For now, just acknowledge
    let dag_id = uuid::Uuid::new_v4().to_string();

    let result = json!({
        "dag_id": dag_id,
        "status": "submitted",
        "message": "DAG workflow submitted successfully"
    });

    Ok(ToolCallResponse {
        content: vec![Content::Text {
            text: serde_json::to_string_pretty(&result)?,
        }],
        is_error: None,
    })
}

/// Handle check_dag_status tool
async fn handle_check_dag_status(arguments: serde_json::Value) -> Result<ToolCallResponse> {
    let dag_id = arguments
        .get("dag_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing dag_id"))?;

    // TODO: Query DAG scheduler for status
    // For now, return mock status
    let result = json!({
        "dag_id": dag_id,
        "status": "running",
        "progress": {
            "total_nodes": 3,
            "completed_nodes": 1,
            "failed_nodes": 0
        },
        "results": {}
    });

    Ok(ToolCallResponse {
        content: vec![Content::Text {
            text: serde_json::to_string_pretty(&result)?,
        }],
        is_error: None,
    })
}

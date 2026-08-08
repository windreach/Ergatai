// SDK-based ACP Session — uses the official agent-client-protocol SDK
// for direct JSON-RPC communication with agents.
//
// This is a rewrite of the session lifecycle using:
// - Client::builder().connect_with() for connection management
// - SDK's SessionNotification handler for event forwarding
// - SDK's RequestPermissionRequest handler for permission bridging
// - Custom idle timeout + usage tracking wrappers

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;

use agent_client_protocol::schema::v1::{
    CloseSessionRequest, ContentBlock, InitializeRequest, NewSessionRequest, PromptRequest,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionId, SetSessionConfigOptionRequest, SessionConfigId,
    SessionConfigValueId, TextContent,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Agent, Client, ConnectionTo};

use super::manager::{
    event_tx, manager, NapiPermissionOption, NapiPermissionRequest, SessionCommand, SessionEvent,
    SessionHandle, SessionKind,
};
use crate::agent::config::{
    codex_network_env, default_agent_env, normalize_agent_args, normalize_agent_command_identity,
    AgentConfig,
};
use crate::error::{ErgataiError, ErgataiResult};

const SESSION_TIMEOUT: Duration = Duration::from_secs(30);
const _IDLE_TIMEOUT: Duration = Duration::from_secs(900); // 15 minutes (reserved for future idle-close)
const MAX_TURN_DURATION: Duration = Duration::from_secs(7200); // 2 hours

/// Permission request ID counter
static PERM_REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Pending permission requests (request_id → responder channel)
type PendingPermissions = Arc<Mutex<std::collections::HashMap<String, oneshot::Sender<Option<String>>>>>;

/// Spawn a session task using the ACP SDK.
pub fn spawn_session_task(
    config: AgentConfig,
    cwd: String,
    session_id_tx: oneshot::Sender<ErgataiResult<String>>,
) {
    spawn_session_task_with_kind(config, cwd, SessionKind::Chat, session_id_tx);
}

/// Spawn a session task with an explicit session kind.
/// DAG sessions auto-approve permission requests (YOLO mode).
pub fn spawn_session_task_with_kind(
    config: AgentConfig,
    cwd: String,
    kind: SessionKind,
    session_id_tx: oneshot::Sender<ErgataiResult<String>>,
) {
    let agent_name = config.name.clone();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<SessionCommand>();
    let evt_tx = event_tx().clone();
    let pending_perms: PendingPermissions = Arc::new(Mutex::new(std::collections::HashMap::new()));

    let session_id_tx = Mutex::new(Some(session_id_tx));

    tokio::spawn(async move {
        let result = run_sdk_session(config, cwd, kind, cmd_rx, evt_tx.clone(), pending_perms.clone(), cmd_tx.clone()).await;

        match result {
            Ok(session_id) => {
                tracing::info!(agent = %agent_name, session_id = %session_id, "SDK session created successfully");
                if let Ok(mut guard) = session_id_tx.lock() {
                    if let Some(tx) = guard.take() {
                        let _ = tx.send(Ok(session_id));
                    }
                }
            }
            Err(e) => {
                tracing::error!(agent = %agent_name, error = %e, "SDK session failed");
                if let Ok(mut guard) = session_id_tx.lock() {
                    if let Some(tx) = guard.take() {
                        let _ = tx.send(Err(e));
                    }
                }
            }
        }
    });
}

/// Run an ACP SDK-based session.
async fn run_sdk_session(
    config: AgentConfig,
    cwd: String,
    kind: SessionKind,
    mut cmd_rx: mpsc::UnboundedReceiver<SessionCommand>,
    evt_tx: mpsc::UnboundedSender<SessionEvent>,
    pending_perms: PendingPermissions,
    cmd_tx: mpsc::UnboundedSender<SessionCommand>,
) -> ErgataiResult<String> {
    // 1. Normalize agent config
    let command = normalize_agent_command_identity(&config.command);
    let args = normalize_agent_args(&command, config.args.clone());

    tracing::info!(
        agent = %config.name,
        command = %command,
        args = ?args,
        "Starting SDK session"
    );

    // 2. Build AcpAgent config
    let mut agent_config = AcpAgentConfig::new(&command).args(args.clone());
    for (k, v) in &config.env {
        agent_config = agent_config.env(k, v);
    }
    // Apply default env vars for known agents
    for &(key, value) in default_agent_env(&command) {
        if !config.env.contains_key(key) {
            agent_config = agent_config.env(key, value);
        }
    }
    // Apply Codex network config if needed (only when BUZZ_RELAY_URL is set)
    if let Ok(relay_url) = std::env::var("BUZZ_RELAY_URL") {
        if let Some((k, v)) = codex_network_env(&command, &relay_url) {
            agent_config = agent_config.env(k, v);
        }
    }

    let agent = AcpAgent::new(agent_config);

    // 3. Create SDK connection with handlers
    let session_id_holder: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let session_id_holder_clone = session_id_holder.clone();
    let evt_tx_clone = evt_tx.clone();
    let pending_perms_clone = pending_perms.clone();

    let result = Client
        .builder()
        // Notification handler: forward session/update events to frontend
        .on_receive_notification(
            {
                let evt_tx = evt_tx_clone.clone();
                async move |notification: agent_client_protocol::schema::v1::SessionNotification,
                            _connection: ConnectionTo<Agent>| -> std::result::Result<(), agent_client_protocol::Error> {
                    let session_id = notification.session_id.to_string();
                    let event_type = match &notification.update {
                        agent_client_protocol::schema::v1::SessionUpdate::AgentMessageChunk(_) => "agent_message_chunk",
                        agent_client_protocol::schema::v1::SessionUpdate::UserMessageChunk(_) => "user_message_chunk",
                        agent_client_protocol::schema::v1::SessionUpdate::AgentThoughtChunk(_) => "agent_thought_chunk",
                        agent_client_protocol::schema::v1::SessionUpdate::ToolCall(_) => "tool_call",
                        agent_client_protocol::schema::v1::SessionUpdate::ToolCallUpdate(_) => "tool_call_update",
                        agent_client_protocol::schema::v1::SessionUpdate::Plan(_) => "plan",
                        agent_client_protocol::schema::v1::SessionUpdate::AvailableCommandsUpdate(_) => "available_commands_update",
                        agent_client_protocol::schema::v1::SessionUpdate::CurrentModeUpdate(_) => "current_mode_update",
                        agent_client_protocol::schema::v1::SessionUpdate::ConfigOptionUpdate(_) => "config_option_update",
                        agent_client_protocol::schema::v1::SessionUpdate::SessionInfoUpdate(_) => "session_info_update",
                        agent_client_protocol::schema::v1::SessionUpdate::UsageUpdate(_) => "usage_update",
                        _ => "other",
                };
                let data = serde_json::to_value(&notification.update)
                    .unwrap_or(serde_json::Value::Null);
                let _ = evt_tx.send(SessionEvent {
                    session_id,
                    event_type: event_type.to_string(),
                    data,
                });
                Ok(())
            }
        }, agent_client_protocol::on_receive_notification!())
        // Permission request handler: bridge to frontend (or auto-approve for DAG sessions)
        .on_receive_request({
            let evt_tx = evt_tx_clone.clone();
            let pending_perms = pending_perms_clone.clone();
            async move |request: RequestPermissionRequest,
                        responder,
                        _connection: ConnectionTo<Agent>| -> std::result::Result<(), agent_client_protocol::Error> {
                let session_id = request.session_id.to_string();
                let request_id = format!("perm-{}", PERM_REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed));

                // DAG sessions auto-approve all permissions (YOLO mode — unattended execution)
                if kind == SessionKind::Dag {
                    let first_option = request.options.first().map(|opt| opt.option_id.to_string());
                    let outcome = match first_option {
                        Some(option_id) => RequestPermissionOutcome::Selected(
                            SelectedPermissionOutcome::new(option_id),
                        ),
                        None => RequestPermissionOutcome::Cancelled,
                    };
                    tracing::debug!(
                        session_id = %session_id,
                        request_id = %request_id,
                        "DAG session auto-approving permission"
                    );
                    let _ = responder.respond(RequestPermissionResponse::new(outcome));
                    return Ok(());
                }

                // Extract options
                let options: Vec<NapiPermissionOption> = request.options.iter().map(|opt| {
                    NapiPermissionOption {
                        option_id: opt.option_id.to_string(),
                        label: opt.option_id.to_string(),
                    }
                }).collect();

                // Send permission request to frontend
                let _ = evt_tx.send(SessionEvent {
                    session_id: session_id.clone(),
                    event_type: "permission_request".to_string(),
                    data: serde_json::to_value(&NapiPermissionRequest {
                        session_id,
                        request_id: request_id.clone(),
                        tool_name: None,
                        options,
                    }).unwrap_or(serde_json::Value::Null),
                });

                // Wait for frontend response with timeout
                let (response_tx, response_rx) = oneshot::channel();
                {
                    if let Ok(mut map) = pending_perms.lock() {
                        map.insert(request_id.clone(), response_tx);
                    }
                }

                let outcome = match tokio::time::timeout(Duration::from_secs(300), response_rx).await {
                    Ok(Ok(Some(option_id))) => {
                        // Remove from pending
                        if let Ok(mut map) = pending_perms.lock() {
                            map.remove(&request_id);
                        }
                        RequestPermissionOutcome::Selected(
                            SelectedPermissionOutcome::new(option_id),
                        )
                    }
                    _ => {
                        // Timeout or error - cancel
                        if let Ok(mut map) = pending_perms.lock() {
                            map.remove(&request_id);
                        }
                        RequestPermissionOutcome::Cancelled
                    }
                };

                let _ = responder.respond(RequestPermissionResponse::new(outcome));
                Ok(())
            }
        }, agent_client_protocol::on_receive_request!())
        // Main connection closure
        .connect_with(agent, {
            let cwd_clone = cwd.clone();
            let session_id_holder = session_id_holder_clone.clone();
            let evt_tx = evt_tx_clone.clone();
            move |connection: ConnectionTo<Agent>| async move {
                // Initialize
                let init_result = timeout(SESSION_TIMEOUT, connection
                    .send_request(InitializeRequest::new(ProtocolVersion::V1))
                    .block_task())
                    .await
                    .map_err(|_| agent_client_protocol::Error::internal_error().data("Initialize timeout"))?
                    .map_err(|e| agent_client_protocol::Error::internal_error().data(format!("Initialize failed: {}", e)))?;

                tracing::info!("Agent initialized: {:?}", init_result);

                // Create session
                let new_session_request = NewSessionRequest::new(&cwd_clone);
                let session_response = timeout(SESSION_TIMEOUT, connection
                    .send_request(new_session_request)
                    .block_task())
                    .await
                    .map_err(|_| agent_client_protocol::Error::internal_error().data("Session creation timeout"))?
                    .map_err(|e| agent_client_protocol::Error::internal_error().data(format!("Session creation failed: {}", e)))?;

                let session_id = session_response.session_id.to_string();
                tracing::info!(session_id = %session_id, "Session created");

                // Store session_id for later use
                {
                    if let Ok(mut holder) = session_id_holder.lock() {
                        *holder = Some(session_id.clone());
                    }
                }

                // Register with manager
                manager().register(SessionHandle {
                    session_id: session_id.clone(),
                    agent_name: config.name.clone(),
                    cwd: cwd_clone.clone(),
                    cmd_tx,
                    kind,
                }).await;

                // Command loop
                let session_id_arc = SessionId::new(session_id.clone());
                loop {
                    match cmd_rx.recv().await {
                        Some(SessionCommand::SendPrompt { text, reply_tx }) => {
                            let result = timeout(MAX_TURN_DURATION, connection
                                .send_request(PromptRequest::new(
                                    session_id_arc.clone(),
                                    vec![ContentBlock::Text(TextContent::new(text))],
                                ))
                                .block_task())
                                .await
                                .map_err(|_| anyhow::anyhow!("Prompt timeout"))
                                .and_then(|r| r.map_err(|e| anyhow::anyhow!("Prompt failed: {}", e)));

                            // Send usage event if available (from UsageUpdate notifications)
                            // Note: SDK's UsageUpdate is handled via notification handler above

                            let _ = reply_tx.send(result.map(|_| ()));
                        }
                        Some(SessionCommand::SetMode { mode_id: _, reply_tx }) => {
                            // SetMode is handled via SetSessionConfigOptionRequest
                            let _ = reply_tx.send(Ok(()));
                        }
                        Some(SessionCommand::SetConfigOption { config_id, value_id, reply_tx }) => {
                            let result = timeout(SESSION_TIMEOUT, connection
                                .send_request(SetSessionConfigOptionRequest::new(
                                    session_id_arc.clone(),
                                    SessionConfigId::new(config_id),
                                    SessionConfigValueId::new(value_id),
                                ))
                                .block_task())
                                .await
                                .map_err(|_| anyhow::anyhow!("SetConfigOption timeout"))
                                .and_then(|r| r.map_err(|e| anyhow::anyhow!("SetConfigOption failed: {}", e)));
                            let _ = reply_tx.send(result.map(|_| ()));
                        }
                        Some(SessionCommand::PermissionResponse { request_id, option_id }) => {
                            // Forward to pending permission waiter
                            if let Ok(mut map) = pending_perms.lock() {
                                if let Some(tx) = map.remove(&request_id) {
                                    let _ = tx.send(option_id);
                                }
                            }
                        }
                        Some(SessionCommand::Steer { text: _, reply_tx }) => {
                            // Steering not natively supported by SDK — would require custom extension
                            let _ = reply_tx.send(Err(anyhow::anyhow!(
                                "Steering not yet supported in SDK mode — use AgentPool path"
                            )));
                        }
                        Some(SessionCommand::Close) => {
                            tracing::info!("Closing SDK session");
                            let _ = timeout(Duration::from_secs(5), connection
                                .send_request(CloseSessionRequest::new(session_id_arc.clone()))
                                .block_task())
                                .await;
                            let _ = evt_tx.send(SessionEvent {
                                session_id: session_id.clone(),
                                event_type: "closed".to_string(),
                                data: serde_json::Value::Null,
                            });
                            manager().unregister(&session_id).await;
                            break;
                        }
                        None => {
                            tracing::info!("Command channel closed, shutting down");
                            let _ = timeout(Duration::from_secs(5), connection
                                .send_request(CloseSessionRequest::new(session_id_arc.clone()))
                                .block_task())
                                .await;
                            manager().unregister(&session_id).await;
                            break;
                        }
                    }
                }

                Ok::<_, agent_client_protocol::Error>(())
            }
        })
        .await;

    // Handle connection result
    if let Err(e) = result {
        return Err(ErgataiError::internal(format!("SDK connection failed: {}", e)));
    }

    // Return session_id
    let session_id = session_id_holder
        .lock()
        .ok()
        .and_then(|h| h.clone())
        .ok_or_else(|| ErgataiError::internal("Session ID not captured"))?;

    Ok(session_id)
}

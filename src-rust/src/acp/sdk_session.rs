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
use agent_client_protocol::{AcpAgent, Agent, Client, ConnectionTo};

use super::manager::{
    event_tx, manager, NapiPermissionOption, NapiPermissionRequest, SessionCommand, SessionEvent,
    SessionHandle, SessionKind,
};
use crate::agent::config::{
    build_acp_agent_config, normalize_agent_args, normalize_agent_command_identity, AgentConfig,
};
use crate::error::{ErgataiError, ErgataiResult};
use crate::file_access::FileMode;

const SESSION_TIMEOUT: Duration = Duration::from_secs(30);
const _IDLE_TIMEOUT: Duration = Duration::from_secs(900); // 15 minutes (reserved for future idle-close)
const MAX_TURN_DURATION: Duration = Duration::from_secs(7200); // 2 hours

/// Permission request ID counter
static PERM_REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Pending permission requests (request_id → responder channel)
type PendingPermissions = Arc<Mutex<std::collections::HashMap<String, oneshot::Sender<Option<String>>>>>;

/// Approval response from TypeScript
#[derive(Debug, Clone)]
pub struct ApprovalResponse {
    pub approved: bool,
    pub approved_by: String,
    pub reason: Option<String>,
}

/// Pending approval requests (request_id → responder channel)
/// Used for multi-agent approval flow
static APPROVAL_WAITERS: std::sync::OnceLock<Arc<Mutex<std::collections::HashMap<String, oneshot::Sender<ApprovalResponse>>>>> = std::sync::OnceLock::new();

/// Get the global approval waiters map
pub fn approval_waiters() -> &'static Arc<Mutex<std::collections::HashMap<String, oneshot::Sender<ApprovalResponse>>>> {
    APPROVAL_WAITERS.get_or_init(|| Arc::new(Mutex::new(std::collections::HashMap::new())))
}

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

    // Build AcpAgent config BEFORE spawning to avoid blocking I/O in async context
    let agent_config = build_acp_agent_config(&config);

    tokio::spawn(async move {
        // Create a channel to signal when session is ready (created + registered)
        let (session_ready_tx, session_ready_rx) = oneshot::channel::<ErgataiResult<String>>();

        // Run session in background - it will signal when ready via session_ready_tx
        tokio::spawn(async move {
            let _ = run_sdk_session(
                config,
                agent_config,
                cwd,
                kind,
                cmd_rx,
                evt_tx.clone(),
                pending_perms.clone(),
                cmd_tx.clone(),
                Some(session_ready_tx),
            )
            .await;
        });

        // Wait for session to be ready and forward to caller
        match session_ready_rx.await {
            Ok(Ok(session_id)) => {
                tracing::info!(agent = %agent_name, session_id = %session_id, "SDK session created successfully");
                let _ = session_id_tx.send(Ok(session_id));
            }
            Ok(Err(e)) => {
                tracing::error!(agent = %agent_name, error = %e, "SDK session failed");
                let _ = session_id_tx.send(Err(e));
            }
            Err(_) => {
                tracing::error!(agent = %agent_name, "Session ready channel dropped");
                let _ = session_id_tx.send(Err(crate::error::ErgataiError::ChannelError(
                    "Session ready channel dropped".into(),
                )));
            }
        }
    });
}

/// Acquire file locks for a permission request.
///
/// This function is called when a RequestPermissionRequest contains file locations.
/// It looks up the FileToken for the session and acquires locks for all specified files.
/// In multi-agent mode, it sends an approval request to TypeScript and waits for response.
async fn acquire_file_locks_for_permission(
    project_id: &str,
    session_id: &str,
    agent_id: &str,
    file_paths: &[String],
    evt_tx: &mpsc::UnboundedSender<SessionEvent>,
) -> ErgataiResult<()> {
    use crate::file_access::get_lock_manager;

    let lock_manager = get_lock_manager(project_id).await?;

    // Find the active FileToken for this session
    let file_token = lock_manager.find_active_file_token_by_session(session_id)?;

    // Check if this permission level requires human approval
    // Only ADMIN mode requires approval; READ and WRITE are auto-approved
    let needs_approval = matches!(file_token.mode, FileMode::Admin);

    let approved_by = if !needs_approval {
        // READ or WRITE permission: auto-approve
        tracing::info!(
            session_id = %session_id,
            mode = ?file_token.mode,
            "Auto-approving file access (READ/WRITE mode)"
        );
        "auto".to_string()
    } else {
        // ADMIN permission: requires human approval
        let request_id = format!("approval-{}", PERM_REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed));

        // Create oneshot channel for response
        let (tx, rx) = oneshot::channel();
        {
            let mut waiters = approval_waiters().lock().map_err(|_| {
                ErgataiError::internal("Failed to acquire approval waiters lock")
            })?;
            waiters.insert(request_id.clone(), tx);
        }

        // 🔴-11 fix: Drop guard ensures cleanup runs even on timeout/error
        struct WaiterCleanup(String);
        impl Drop for WaiterCleanup {
            fn drop(&mut self) {
                if let Ok(mut w) = approval_waiters().lock() {
                    w.remove(&self.0);
                }
            }
        }
        let _cleanup = WaiterCleanup(request_id.clone());

        // Send approval request event to TypeScript
        let _ = evt_tx.send(SessionEvent {
            session_id: session_id.to_string(),
            event_type: "file_access_approval_request".to_string(),
            data: serde_json::to_value(&serde_json::json!({
                "request_id": request_id,
                "agent_id": agent_id,
                "session_id": session_id,
                "file_paths": file_paths,
                "scope": file_token.scope,
                "mode": "ADMIN",
                "reason": format!("Agent {} requests ADMIN access to {} file(s)", agent_id, file_paths.len()),
            })).unwrap_or(serde_json::Value::Null),
        });

        tracing::info!(
            session_id = %session_id,
            request_id = %request_id,
            "ADMIN mode, sending approval request to human"
        );

        // Wait for response with timeout (30 seconds)
        let response = timeout(Duration::from_secs(30), rx)
            .await
            .map_err(|_| ErgataiError::AgentTimeout {
                message: "Approval request timed out".to_string(),
                source: None,
            })?
            .map_err(|_| ErgataiError::ChannelError("Approval response channel dropped".into()))?;

        // Cleanup handled by WaiterCleanup Drop guard (_cleanup)

        if !response.approved {
            return Err(ErgataiError::PermissionDenied(format!(
                "ADMIN access denied by {}: {}",
                response.approved_by,
                response.reason.unwrap_or_else(|| "No reason provided".to_string())
            )));
        }

        tracing::info!(
            session_id = %session_id,
            request_id = %request_id,
            approved_by = %response.approved_by,
            "ADMIN access approved"
        );

        response.approved_by
    };

    // Acquire locks for each file
    for file_path in file_paths {
        // Check scope
        if !file_token.matches_path(file_path) {
            return Err(ErgataiError::PermissionDenied(format!(
                "File {} is outside scope {}",
                file_path, file_token.scope
            )));
        }

        // Acquire the lock (log_audit is called internally by acquire_lock)
        lock_manager.acquire_lock(&file_token, file_path)?;

        // Log approval in audit trail
        lock_manager.log_audit(
            agent_id,
            session_id,
            "file_access_approved",
            Some(file_path),
            Some(&format!("{:?}", file_token.mode)),
            Some(&format!("Approved by: {}", approved_by)),
        )?;

        tracing::info!(
            agent_id = %agent_id,
            session_id = %session_id,
            file_path = %file_path,
            approved_by = %approved_by,
            "File lock acquired for permission request"
        );
    }

    Ok(())
}

/// Run an ACP SDK-based session.
async fn run_sdk_session(
    config: AgentConfig,
    agent_config: agent_client_protocol::AcpAgentConfig,
    cwd: String,
    kind: SessionKind,
    mut cmd_rx: mpsc::UnboundedReceiver<SessionCommand>,
    evt_tx: mpsc::UnboundedSender<SessionEvent>,
    pending_perms: PendingPermissions,
    cmd_tx: mpsc::UnboundedSender<SessionCommand>,
    session_ready_tx: Option<oneshot::Sender<ErgataiResult<String>>>,
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

    // 2. Create AcpAgent from pre-built config (built before spawn to avoid blocking I/O)
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
                let data = match serde_json::to_value(&notification.update) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(error = %e, update_type = %event_type, "Failed to serialize session update");
                        serde_json::Value::Null
                    }
                };
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
            let cwd_for_locks = cwd.clone();
            let config_name = config.name.clone();
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

                // Check if this is a file operation by looking at locations
                if let Some(locations) = &request.tool_call.fields.locations {
                    if !locations.is_empty() {
                        // This is a file operation, acquire file locks
                        let file_paths: Vec<String> = locations
                            .iter()
                            .map(|loc| loc.path.to_string_lossy().to_string())
                            .collect();

                        match acquire_file_locks_for_permission(
                            &cwd_for_locks,  // Use cwd as project_id
                            &session_id,
                            &config_name,
                            &file_paths,
                            &evt_tx,
                        ).await {
                            Ok(()) => {
                                // Locks acquired successfully, approve the request
                                tracing::info!(
                                    session_id = %session_id,
                                    file_paths = ?file_paths,
                                    "File locks acquired, approving permission"
                                );
                                let first_option = request.options.first().map(|opt| opt.option_id.to_string());
                                let outcome = match first_option {
                                    Some(option_id) => RequestPermissionOutcome::Selected(
                                        SelectedPermissionOutcome::new(option_id),
                                    ),
                                    None => RequestPermissionOutcome::Cancelled,
                                };
                                let _ = responder.respond(RequestPermissionResponse::new(outcome));
                                return Ok(());
                            }
                            Err(e) => {
                                // Failed to acquire locks, deny the request
                                tracing::warn!(
                                    session_id = %session_id,
                                    file_paths = ?file_paths,
                                    error = %e,
                                    "Failed to acquire file locks, denying permission"
                                );
                                let _ = responder.respond(RequestPermissionResponse::new(
                                    RequestPermissionOutcome::Cancelled
                                ));
                                return Ok(());
                            }
                        }
                    }
                }

                // Non-file operations or operations without locations: send to frontend
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
                        tool_name: request.tool_call.fields.title.clone(),
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
                    match session_id_holder.lock() {
                        Ok(mut holder) => *holder = Some(session_id.clone()),
                        Err(e) => tracing::error!("session_id_holder mutex poisoned: {}", e),
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

                // Signal that session is ready (created + registered)
                if let Some(tx) = session_ready_tx {
                    tracing::info!(session_id = %session_id, "Signaling session ready to caller");
                    let _ = tx.send(Ok(session_id.clone()));
                }

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

                            // Signal completion: send closed event after prompt completes
                            if result.is_ok() {
                                let _ = evt_tx.send(SessionEvent {
                                    session_id: session_id.clone(),
                                    event_type: "closed".to_string(),
                                    data: serde_json::Value::Null,
                                });
                            }

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
                            match timeout(Duration::from_secs(5), connection
                                .send_request(CloseSessionRequest::new(session_id_arc.clone()))
                                .block_task())
                                .await
                            {
                                Ok(Ok(_)) => {}
                                Ok(Err(e)) => tracing::warn!(error = %e, "CloseSession request failed"),
                                Err(_) => tracing::warn!("CloseSession request timed out"),
                            }
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
                            match timeout(Duration::from_secs(5), connection
                                .send_request(CloseSessionRequest::new(session_id_arc.clone()))
                                .block_task())
                                .await
                            {
                                Ok(Ok(_)) => {}
                                Ok(Err(e)) => tracing::warn!(error = %e, "CloseSession request failed"),
                                Err(_) => tracing::warn!("CloseSession request timed out"),
                            }
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

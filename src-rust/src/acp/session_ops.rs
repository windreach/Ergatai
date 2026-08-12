use std::path::PathBuf;
use std::time::Duration;
use tokio::time::timeout;

use agent_client_protocol::schema::v1::{
    CloseSessionRequest, ContentBlock, DeleteSessionRequest, InitializeRequest,
    ListSessionsRequest, LoadSessionRequest, ResumeSessionRequest, SessionConfigId,
    SessionConfigValueId, SessionId, SessionInfo, SessionModeId, SetSessionConfigOptionRequest,
    SetSessionModeRequest, TextContent,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{AcpAgent, Agent, Client, ConnectionTo};

use super::manager::{event_tx, manager, SessionCommand, SessionEvent, SessionHandle, SessionKind};
use crate::agent::config::{build_acp_agent_config, AgentConfig};
use crate::error::{ErgataiError, ErgataiResult};

const SESSION_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_TURN_DURATION: Duration = Duration::from_secs(7200); // 2 hours (matches sdk_session.rs)

/// 通过临时连接执行一次性 ACP 操作。
/// 连接 → 初始化 → 执行闭包 → 断开。
async fn with_agent_connection<F, T, Fut>(config: &AgentConfig, f: F) -> ErgataiResult<T>
where
    F: FnOnce(ConnectionTo<Agent>) -> Fut,
    Fut: std::future::Future<Output = ErgataiResult<T>>,
{
    let agent_config = build_acp_agent_config(config);
    let agent = AcpAgent::new(agent_config);

    let result = Client
        .builder()
        .connect_with(agent, |connection: ConnectionTo<Agent>| async move {
            // 初始化
            let init_result = timeout(
                SESSION_TIMEOUT,
                connection
                    .send_request(InitializeRequest::new(ProtocolVersion::V1))
                    .block_task(),
            )
            .await;

            match init_result {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    return Err(agent_client_protocol::Error::internal_error()
                        .data(format!("Initialize failed: {}", e)))
                }
                Err(_) => {
                    return Err(
                        agent_client_protocol::Error::internal_error().data("Initialize timeout")
                    )
                }
            }

            // 执行操作
            match f(connection).await {
                Ok(result) => Ok(result),
                Err(e) => Err(agent_client_protocol::Error::internal_error().data(e.to_string())),
            }
        })
        .await?;

    Ok(result)
}

/// 从 agent 查询会话列表
pub async fn list_sessions_from_agent(
    config: &AgentConfig,
    cwd: Option<String>,
) -> ErgataiResult<Vec<SessionInfo>> {
    let cwd_path = cwd.map(PathBuf::from);

    with_agent_connection(config, |connection| async move {
        let mut request = ListSessionsRequest::new();
        if let Some(cwd) = &cwd_path {
            request = request.cwd(cwd.clone());
        }

        let response = timeout(
            SESSION_TIMEOUT,
            connection.send_request(request).block_task(),
        )
        .await
        .map_err(|_| ErgataiError::agent_timeout("ListSessions timeout"))?
        .map_err(|e| ErgataiError::network_with_source("ListSessions failed", e))?;

        Ok(response.sessions)
    })
    .await
}

/// 从 agent 删除会话
pub async fn delete_session_from_agent(
    config: &AgentConfig,
    session_id: &str,
) -> ErgataiResult<()> {
    let sid = SessionId::new(session_id.to_string());

    with_agent_connection(config, |connection| async move {
        timeout(
            SESSION_TIMEOUT,
            connection
                .send_request(DeleteSessionRequest::new(sid))
                .block_task(),
        )
        .await
        .map_err(|_| ErgataiError::agent_timeout("DeleteSession timeout"))?
        .map_err(|e| ErgataiError::network_with_source("DeleteSession failed", e))?;

        Ok(())
    })
    .await
}

/// 加载已有会话（与 create_session 类似，但发送 LoadSessionRequest）
pub fn load_session_task(
    config: AgentConfig,
    session_id: String,
    cwd: String,
    session_id_tx: tokio::sync::oneshot::Sender<ErgataiResult<String>>,
) {
    let agent_name = config.name.clone();
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<SessionCommand>();
    let evt_tx = event_tx().clone();

    let session_id_tx = std::sync::Arc::new(std::sync::Mutex::new(Some(session_id_tx)));

    let agent_config = build_acp_agent_config(&config);
    let agent = AcpAgent::new(agent_config);

    tokio::spawn({
        let session_id_tx = session_id_tx.clone();
        async move {
            let result = Client.builder()
                .on_receive_notification({
                    let evt_tx = evt_tx.clone();
                    async move |notification: agent_client_protocol::schema::v1::SessionNotification,
                                _connection: ConnectionTo<Agent>| {
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
                },
                agent_client_protocol::on_receive_notification!(),
                )
                .on_receive_request(
                    async move |request: agent_client_protocol::schema::v1::RequestPermissionRequest,
                                responder,
                                _connection: ConnectionTo<Agent>| {
                        // YOLO: 自动批准
                        let option_id = request.options.first().map(|opt| opt.option_id.clone());
                        if let Some(id) = option_id {
                            responder.respond(agent_client_protocol::schema::v1::RequestPermissionResponse::new(
                                agent_client_protocol::schema::v1::RequestPermissionOutcome::Selected(
                                    agent_client_protocol::schema::v1::SelectedPermissionOutcome::new(id),
                                ),
                            ))
                        } else {
                            responder.respond(agent_client_protocol::schema::v1::RequestPermissionResponse::new(
                                agent_client_protocol::schema::v1::RequestPermissionOutcome::Cancelled,
                            ))
                        }
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .connect_with(agent, {
                    let tx_for_closure = session_id_tx.clone();
                    let session_id_clone = session_id.clone();
                    let cwd_clone = cwd.clone();
                    move |connection: ConnectionTo<Agent>| async move {
                        let tx = match tx_for_closure.lock() {
                            Ok(mut g) => g.take(),
                            Err(e) => {
                                tracing::error!("session_id_tx mutex poisoned: {}", e);
                                None
                            }
                        };

                        // 1. 初始化
                        let init_result = timeout(SESSION_TIMEOUT, connection
                            .send_request(InitializeRequest::new(ProtocolVersion::V1))
                            .block_task())
                            .await;
                        match init_result {
                            Ok(Ok(_)) => {},
                            Ok(Err(e)) => return Err(agent_client_protocol::Error::internal_error().data(format!("Initialize failed: {}", e))),
                            Err(_) => return Err(agent_client_protocol::Error::internal_error().data("Initialize timeout")),
                        }

                        // 2. 加载会话
                        let sid = SessionId::new(session_id_clone.clone());
                        let load_result = timeout(SESSION_TIMEOUT, connection
                            .send_request(LoadSessionRequest::new(sid, PathBuf::from(&cwd_clone)))
                            .block_task())
                            .await;
                        match load_result {
                            Ok(Ok(_)) => {},
                            Ok(Err(e)) => return Err(agent_client_protocol::Error::internal_error().data(format!("LoadSession failed: {}", e))),
                            Err(_) => return Err(agent_client_protocol::Error::internal_error().data("LoadSession timeout")),
                        }

                        // 3. 注册到全局管理器
                        manager().register(SessionHandle {
                            session_id: session_id_clone.clone(),
                            agent_name: agent_name.clone(),
                            cwd: cwd_clone.clone(),
                            cmd_tx,
                            kind: SessionKind::Chat,
                        }).await;

                        // Register with file access control for single-agent mode detection
                        if let Ok(lock_manager) = crate::file_access::get_lock_manager(&cwd_clone).await {
                            lock_manager.register_session();
                        }

                        // 4. 通知 NAPI
                        if let Some(tx) = tx {
                            let _ = tx.send(Ok(session_id_clone.clone()));
                        }

                        // 5. 命令循环（与 session.rs 相同）
                        let mut cmd_rx = cmd_rx;
                        let session_id_arc = SessionId::new(session_id_clone.clone());
                        loop {
                            match cmd_rx.recv().await {
                                Some(SessionCommand::SendPrompt { text, reply_tx }) => {
                                    let result = timeout(MAX_TURN_DURATION, connection
                                        .send_request(agent_client_protocol::schema::v1::PromptRequest::new(
                                            session_id_arc.clone(),
                                            vec![ContentBlock::Text(TextContent::new(text))],
                                        ))
                                        .block_task())
                                        .await
                                        .map_err(|_| anyhow::anyhow!("Prompt timeout"))
                                        .and_then(|r| r.map_err(|e| anyhow::anyhow!("Prompt failed: {}", e)));
                                    let _ = reply_tx.send(result.map(|_| ()));
                                }
                                Some(SessionCommand::SetMode { mode_id, reply_tx }) => {
                                    let result = timeout(SESSION_TIMEOUT, connection
                                        .send_request(SetSessionModeRequest::new(
                                            session_id_arc.clone(),
                                            SessionModeId::new(mode_id),
                                        ))
                                        .block_task())
                                        .await
                                        .map_err(|_| anyhow::anyhow!("SetMode timeout"))
                                        .and_then(|r| r.map_err(|e| anyhow::anyhow!("SetMode failed: {}", e)));
                                    let _ = reply_tx.send(result.map(|_| ()));
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
                                Some(SessionCommand::PermissionResponse { .. }) => {
                                    // Load session doesn't handle permissions directly
                                }
                                Some(SessionCommand::Steer { reply_tx, .. }) => {
                                    // Steering not supported via legacy agent-client-protocol path
                                    let _ = reply_tx.send(Err(anyhow::anyhow!(
                                        "Steering not supported for sessions loaded via legacy protocol"
                                    )));
                                }
                                Some(SessionCommand::Close) => {
                                    // 通过 ACP 协议发送 CloseSessionRequest
                                    match timeout(SESSION_TIMEOUT, connection
                                        .send_request(CloseSessionRequest::new(session_id_arc.clone()))
                                        .block_task())
                                        .await
                                    {
                                        Ok(Ok(_)) => {}
                                        Ok(Err(e)) => tracing::warn!(error = %e, "CloseSession request failed"),
                                        Err(_) => tracing::warn!("CloseSession request timed out"),
                                    }

                                    let _ = evt_tx.send(SessionEvent {
                                        session_id: session_id_clone.clone(),
                                        event_type: "closed".to_string(),
                                        data: serde_json::Value::Null,
                                    });
                                    // Unregister from file access control (single-agent mode detection)
                                    if let Ok(lock_manager) = crate::file_access::get_lock_manager(&cwd_clone).await {
                                        lock_manager.unregister_session();
                                    }
                                    manager().unregister(&session_id_clone).await;
                                    break;
                                }
                                None => {
                                    match timeout(std::time::Duration::from_secs(5), connection
                                        .send_request(CloseSessionRequest::new(session_id_arc.clone()))
                                        .block_task())
                                        .await
                                    {
                                        Ok(Ok(_)) => {}
                                        Ok(Err(e)) => tracing::warn!(error = %e, "CloseSession request failed"),
                                        Err(_) => tracing::warn!("CloseSession request timed out"),
                                    }
                                    // Unregister from file access control (single-agent mode detection)
                                    if let Ok(lock_manager) = crate::file_access::get_lock_manager(&cwd_clone).await {
                                        lock_manager.unregister_session();
                                    }
                                    manager().unregister(&session_id_clone).await;
                                    break;
                                }
                            }
                        }
                        Ok(())
                    }
                })
                .await;

            if let Err(e) = result {
                tracing::error!("Load session connection failed: {}", e);
                let tx = match session_id_tx.lock() {
                    Ok(mut g) => g.take(),
                    Err(e) => {
                        tracing::error!("session_id_tx mutex poisoned: {}", e);
                        None
                    }
                };
                if let Some(tx) = tx {
                    let _ = tx.send(Err(ErgataiError::network(format!(
                        "Connection failed: {}",
                        e
                    ))));
                }
            }
        }
    });
}

/// 恢复已有会话（与 load_session_task 类似，但发送 ResumeSessionRequest）
pub fn resume_session_task(
    config: AgentConfig,
    session_id: String,
    cwd: String,
    session_id_tx: tokio::sync::oneshot::Sender<ErgataiResult<String>>,
) {
    let agent_name = config.name.clone();
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<SessionCommand>();
    let evt_tx = event_tx().clone();

    let session_id_tx = std::sync::Arc::new(std::sync::Mutex::new(Some(session_id_tx)));

    let agent_config = build_acp_agent_config(&config);
    let agent = AcpAgent::new(agent_config);

    tokio::spawn({
        let session_id_tx = session_id_tx.clone();
        async move {
            let result = Client.builder()
                .on_receive_notification({
                    let evt_tx = evt_tx.clone();
                    async move |notification: agent_client_protocol::schema::v1::SessionNotification,
                                _connection: ConnectionTo<Agent>| {
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
                },
                agent_client_protocol::on_receive_notification!(),
                )
                .on_receive_request(
                    async move |request: agent_client_protocol::schema::v1::RequestPermissionRequest,
                                responder,
                                _connection: ConnectionTo<Agent>| {
                        // YOLO: 自动批准（resume 会话暂不支持前端权限确认）
                        let option_id = request.options.first().map(|opt| opt.option_id.clone());
                        if let Some(id) = option_id {
                            responder.respond(agent_client_protocol::schema::v1::RequestPermissionResponse::new(
                                agent_client_protocol::schema::v1::RequestPermissionOutcome::Selected(
                                    agent_client_protocol::schema::v1::SelectedPermissionOutcome::new(id),
                                ),
                            ))
                        } else {
                            responder.respond(agent_client_protocol::schema::v1::RequestPermissionResponse::new(
                                agent_client_protocol::schema::v1::RequestPermissionOutcome::Cancelled,
                            ))
                        }
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .connect_with(agent, {
                    let tx_for_closure = session_id_tx.clone();
                    let session_id_clone = session_id.clone();
                    let cwd_clone = cwd.clone();
                    move |connection: ConnectionTo<Agent>| async move {
                        let tx = match tx_for_closure.lock() {
                            Ok(mut g) => g.take(),
                            Err(e) => {
                                tracing::error!("session_id_tx mutex poisoned: {}", e);
                                None
                            }
                        };

                        // 1. 初始化
                        let init_result = timeout(SESSION_TIMEOUT, connection
                            .send_request(InitializeRequest::new(ProtocolVersion::V1))
                            .block_task())
                            .await;
                        match init_result {
                            Ok(Ok(_)) => {},
                            Ok(Err(e)) => return Err(agent_client_protocol::Error::internal_error().data(format!("Initialize failed: {}", e))),
                            Err(_) => return Err(agent_client_protocol::Error::internal_error().data("Initialize timeout")),
                        }

                        // 2. 恢复会话
                        let sid = SessionId::new(session_id_clone.clone());
                        let resume_result = timeout(SESSION_TIMEOUT, connection
                            .send_request(ResumeSessionRequest::new(sid, PathBuf::from(&cwd_clone)))
                            .block_task())
                            .await;
                        match resume_result {
                            Ok(Ok(_)) => {},
                            Ok(Err(e)) => return Err(agent_client_protocol::Error::internal_error().data(format!("ResumeSession failed: {}", e))),
                            Err(_) => return Err(agent_client_protocol::Error::internal_error().data("ResumeSession timeout")),
                        }

                        // 3. 注册到全局管理器
                        manager().register(SessionHandle {
                            session_id: session_id_clone.clone(),
                            agent_name: agent_name.clone(),
                            cwd: cwd_clone.clone(),
                            cmd_tx,
                            kind: SessionKind::Chat,
                        }).await;

                        // Register with file access control for single-agent mode detection
                        if let Ok(lock_manager) = crate::file_access::get_lock_manager(&cwd_clone).await {
                            lock_manager.register_session();
                        }

                        // 4. 通知 NAPI
                        if let Some(tx) = tx {
                            let _ = tx.send(Ok(session_id_clone.clone()));
                        }

                        // 5. 命令循环
                        let mut cmd_rx = cmd_rx;
                        let session_id_arc = SessionId::new(session_id_clone.clone());
                        loop {
                            match cmd_rx.recv().await {
                                Some(SessionCommand::SendPrompt { text, reply_tx }) => {
                                    let result = timeout(MAX_TURN_DURATION, connection
                                        .send_request(agent_client_protocol::schema::v1::PromptRequest::new(
                                            session_id_arc.clone(),
                                            vec![ContentBlock::Text(TextContent::new(text))],
                                        ))
                                        .block_task())
                                        .await
                                        .map_err(|_| anyhow::anyhow!("Prompt timeout"))
                                        .and_then(|r| r.map_err(|e| anyhow::anyhow!("Prompt failed: {}", e)));
                                    let _ = reply_tx.send(result.map(|_| ()));
                                }
                                Some(SessionCommand::SetMode { mode_id, reply_tx }) => {
                                    let result = timeout(SESSION_TIMEOUT, connection
                                        .send_request(SetSessionModeRequest::new(
                                            session_id_arc.clone(),
                                            SessionModeId::new(mode_id),
                                        ))
                                        .block_task())
                                        .await
                                        .map_err(|_| anyhow::anyhow!("SetMode timeout"))
                                        .and_then(|r| r.map_err(|e| anyhow::anyhow!("SetMode failed: {}", e)));
                                    let _ = reply_tx.send(result.map(|_| ()));
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
                                Some(SessionCommand::PermissionResponse { .. }) => {}
                                Some(SessionCommand::Steer { reply_tx, .. }) => {
                                    let _ = reply_tx.send(Err(anyhow::anyhow!(
                                        "Steering not supported for sessions loaded via legacy protocol"
                                    )));
                                }
                                Some(SessionCommand::Close) => {
                                    match timeout(SESSION_TIMEOUT, connection
                                        .send_request(CloseSessionRequest::new(session_id_arc.clone()))
                                        .block_task())
                                        .await
                                    {
                                        Ok(Ok(_)) => {}
                                        Ok(Err(e)) => tracing::warn!(error = %e, "CloseSession request failed"),
                                        Err(_) => tracing::warn!("CloseSession request timed out"),
                                    }
                                    let _ = evt_tx.send(SessionEvent {
                                        session_id: session_id_clone.clone(),
                                        event_type: "closed".to_string(),
                                        data: serde_json::Value::Null,
                                    });
                                    // Unregister from file access control (single-agent mode detection)
                                    if let Ok(lock_manager) = crate::file_access::get_lock_manager(&cwd_clone).await {
                                        lock_manager.unregister_session();
                                    }
                                    manager().unregister(&session_id_clone).await;
                                    break;
                                }
                                None => {
                                    match timeout(std::time::Duration::from_secs(5), connection
                                        .send_request(CloseSessionRequest::new(session_id_arc.clone()))
                                        .block_task())
                                        .await
                                    {
                                        Ok(Ok(_)) => {}
                                        Ok(Err(e)) => tracing::warn!(error = %e, "CloseSession request failed"),
                                        Err(_) => tracing::warn!("CloseSession request timed out"),
                                    }
                                    // Unregister from file access control (single-agent mode detection)
                                    if let Ok(lock_manager) = crate::file_access::get_lock_manager(&cwd_clone).await {
                                        lock_manager.unregister_session();
                                    }
                                    manager().unregister(&session_id_clone).await;
                                    break;
                                }
                            }
                        }
                        Ok(())
                    }
                })
                .await;

            if let Err(e) = result {
                tracing::error!("Resume session connection failed: {}", e);
                let tx = match session_id_tx.lock() {
                    Ok(mut g) => g.take(),
                    Err(e) => {
                        tracing::error!("session_id_tx mutex poisoned: {}", e);
                        None
                    }
                };
                if let Some(tx) = tx {
                    let _ = tx.send(Err(ErgataiError::network(format!(
                        "Connection failed: {}",
                        e
                    ))));
                }
            }
        }
    });
}

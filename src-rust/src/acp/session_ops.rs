use anyhow::Result;
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
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Agent, Client, ConnectionTo};

use super::manager::{event_tx, manager, SessionCommand, SessionEvent, SessionHandle, SessionKind};
use crate::agent::config::AgentConfig;

const SESSION_TIMEOUT: Duration = Duration::from_secs(30);

/// 通过临时连接执行一次性 ACP 操作。
/// 连接 → 初始化 → 执行闭包 → 断开。
async fn with_agent_connection<F, T, Fut>(config: &AgentConfig, f: F) -> Result<T>
where
    F: FnOnce(ConnectionTo<Agent>) -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut agent_config = AcpAgentConfig::new(&config.command).args(config.args.clone());
    for (k, v) in &config.env {
        agent_config = agent_config.env(k, v);
    }
    let agent = AcpAgent::new(agent_config);

    let result = Client.builder()
        .connect_with(agent, |connection: ConnectionTo<Agent>| async move {
            // 初始化
            timeout(SESSION_TIMEOUT, connection
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task())
                .await
                .map_err(|_| anyhow::anyhow!("Initialize timeout"))?
                .map_err(|e| anyhow::anyhow!("Initialize failed: {}", e))?;

            // 执行操作
            let result = f(connection).await?;
            Ok(result)
        })
        .await?;

    Ok(result)
}

/// 从 agent 查询会话列表
pub async fn list_sessions_from_agent(config: &AgentConfig, cwd: Option<String>) -> Result<Vec<SessionInfo>> {
    let cwd_path = cwd.map(PathBuf::from);

    with_agent_connection(config, |connection| async move {
        let mut request = ListSessionsRequest::new();
        if let Some(cwd) = &cwd_path {
            request = request.cwd(cwd.clone());
        }

        let response = timeout(SESSION_TIMEOUT, connection
            .send_request(request).block_task())
            .await
            .map_err(|_| anyhow::anyhow!("ListSessions timeout"))?
            .map_err(|e| anyhow::anyhow!("ListSessions failed: {}", e))?;

        Ok(response.sessions)
    })
    .await
}

/// 从 agent 删除会话
pub async fn delete_session_from_agent(config: &AgentConfig, session_id: &str) -> Result<()> {
    let sid = SessionId::new(session_id.to_string());

    with_agent_connection(config, |connection| async move {
        timeout(SESSION_TIMEOUT, connection
            .send_request(DeleteSessionRequest::new(sid)).block_task())
            .await
            .map_err(|_| anyhow::anyhow!("DeleteSession timeout"))?
            .map_err(|e| anyhow::anyhow!("DeleteSession failed: {}", e))?;

        Ok(())
    })
    .await
}

/// 加载已有会话（与 create_session 类似，但发送 LoadSessionRequest）
pub fn load_session_task(
    config: AgentConfig,
    session_id: String,
    cwd: String,
    session_id_tx: tokio::sync::oneshot::Sender<Result<String>>,
) {
    let agent_name = config.name.clone();
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<SessionCommand>();
    let evt_tx = event_tx().clone();

    let session_id_tx = std::sync::Arc::new(std::sync::Mutex::new(Some(session_id_tx)));

    let mut agent_config = AcpAgentConfig::new(&config.command).args(config.args.clone());
    for (k, v) in &config.env {
        agent_config = agent_config.env(k, v);
    }
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
                        let data = serde_json::to_value(&notification.update)
                            .unwrap_or(serde_json::Value::Null);
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
                        let tx = tx_for_closure.lock().unwrap().take();

                        // 1. 初始化
                        timeout(SESSION_TIMEOUT, connection
                            .send_request(InitializeRequest::new(ProtocolVersion::V1))
                            .block_task())
                            .await
                            .map_err(|_| anyhow::anyhow!("Initialize timeout"))?
                            .map_err(|e| anyhow::anyhow!("Initialize failed: {}", e))?;

                        // 2. 加载会话
                        let sid = SessionId::new(session_id_clone.clone());
                        timeout(SESSION_TIMEOUT, connection
                            .send_request(LoadSessionRequest::new(sid, PathBuf::from(&cwd_clone)))
                            .block_task())
                            .await
                            .map_err(|_| anyhow::anyhow!("LoadSession timeout"))?
                            .map_err(|e| anyhow::anyhow!("LoadSession failed: {}", e))?;

                        // 3. 注册到全局管理器
                        manager().register(SessionHandle {
                            session_id: session_id_clone.clone(),
                            agent_name: agent_name.clone(),
                            cwd: cwd_clone.clone(),
                            cmd_tx,
                            kind: SessionKind::Chat,
                        }).await;

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
                                    let result = timeout(SESSION_TIMEOUT, connection
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
                                    let _ = timeout(SESSION_TIMEOUT, connection
                                        .send_request(CloseSessionRequest::new(session_id_arc.clone()))
                                        .block_task())
                                        .await;

                                    let _ = evt_tx.send(SessionEvent {
                                        session_id: session_id_clone.clone(),
                                        event_type: "closed".to_string(),
                                        data: serde_json::Value::Null,
                                    });
                                    manager().unregister(&session_id_clone).await;
                                    break;
                                }
                                None => {
                                    let _ = timeout(std::time::Duration::from_secs(5), connection
                                        .send_request(CloseSessionRequest::new(session_id_arc.clone()))
                                        .block_task())
                                        .await;
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
                if let Some(tx) = session_id_tx.lock().unwrap().take() {
                    let _ = tx.send(Err(anyhow::anyhow!("Connection failed: {}", e)));
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
    session_id_tx: tokio::sync::oneshot::Sender<Result<String>>,
) {
    let agent_name = config.name.clone();
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<SessionCommand>();
    let evt_tx = event_tx().clone();

    let session_id_tx = std::sync::Arc::new(std::sync::Mutex::new(Some(session_id_tx)));

    let mut agent_config = AcpAgentConfig::new(&config.command).args(config.args.clone());
    for (k, v) in &config.env {
        agent_config = agent_config.env(k, v);
    }
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
                        let data = serde_json::to_value(&notification.update)
                            .unwrap_or(serde_json::Value::Null);
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
                        let tx = tx_for_closure.lock().unwrap().take();

                        // 1. 初始化
                        timeout(SESSION_TIMEOUT, connection
                            .send_request(InitializeRequest::new(ProtocolVersion::V1))
                            .block_task())
                            .await
                            .map_err(|_| anyhow::anyhow!("Initialize timeout"))?
                            .map_err(|e| anyhow::anyhow!("Initialize failed: {}", e))?;

                        // 2. 恢复会话
                        let sid = SessionId::new(session_id_clone.clone());
                        timeout(SESSION_TIMEOUT, connection
                            .send_request(ResumeSessionRequest::new(sid, PathBuf::from(&cwd_clone)))
                            .block_task())
                            .await
                            .map_err(|_| anyhow::anyhow!("ResumeSession timeout"))?
                            .map_err(|e| anyhow::anyhow!("ResumeSession failed: {}", e))?;

                        // 3. 注册到全局管理器
                        manager().register(SessionHandle {
                            session_id: session_id_clone.clone(),
                            agent_name: agent_name.clone(),
                            cwd: cwd_clone.clone(),
                            cmd_tx,
                            kind: SessionKind::Chat,
                        }).await;

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
                                    let result = timeout(SESSION_TIMEOUT, connection
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
                                    let _ = timeout(SESSION_TIMEOUT, connection
                                        .send_request(CloseSessionRequest::new(session_id_arc.clone()))
                                        .block_task())
                                        .await;
                                    let _ = evt_tx.send(SessionEvent {
                                        session_id: session_id_clone.clone(),
                                        event_type: "closed".to_string(),
                                        data: serde_json::Value::Null,
                                    });
                                    manager().unregister(&session_id_clone).await;
                                    break;
                                }
                                None => {
                                    let _ = timeout(std::time::Duration::from_secs(5), connection
                                        .send_request(CloseSessionRequest::new(session_id_arc.clone()))
                                        .block_task())
                                        .await;
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
                if let Some(tx) = session_id_tx.lock().unwrap().take() {
                    let _ = tx.send(Err(anyhow::anyhow!("Connection failed: {}", e)));
                }
            }
        }
    });
}

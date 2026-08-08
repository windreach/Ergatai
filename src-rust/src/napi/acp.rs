//! ACP (Agent Client Protocol) session management NAPI bindings.

use napi::bindgen_prelude::*;
use napi_derive::napi;

use super::guard;
use crate::error::ErgataiError;

/// 创建 ACP 会话（异步）
/// 返回 session_id
#[napi]
pub async fn acp_create_session(agent_name: String, cwd: String) -> Result<String> {
    guard();

    let config = crate::agent::config::get_agent_config(&agent_name)
        .map_err(super::to_napi)?;

    let (tx, rx) = tokio::sync::oneshot::channel();

    // Use SDK-based session implementation (official ACP SDK)
    crate::acp::sdk_session::spawn_session_task(config, cwd, tx);

    match rx.await {
        Ok(Ok(session_id)) => Ok(session_id),
        Ok(Err(e)) => Err(super::to_napi(e)),
        Err(_) => Err(super::to_napi(ErgataiError::ChannelError(
            "Session task died before responding".into(),
        ))),
    }
}

/// 发送 prompt 到指定会话
#[napi]
pub async fn acp_send_prompt(session_id: String, prompt: String) -> Result<()> {
    guard();
    let cmd_tx = crate::acp::manager::manager()
        .get_cmd_tx(&session_id)
        .await
        .ok_or_else(|| {
            super::to_napi(ErgataiError::SessionNotFound(session_id))
        })?;

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    cmd_tx
        .send(crate::acp::manager::SessionCommand::SendPrompt {
            text: prompt,
            reply_tx,
        })
        .map_err(|_| super::to_napi(ErgataiError::ChannelError("Session task is dead".into())))?;

    match reply_rx.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(super::to_napi(ErgataiError::internal(format!(
            "Prompt failed: {}",
            e
        )))),
        Err(_) => Err(super::to_napi(ErgataiError::ChannelError(
            "Session task died while processing prompt".into(),
        ))),
    }
}

/// 关闭指定会话
#[napi]
pub async fn acp_close_session(session_id: String) -> Result<bool> {
    guard();
    let cmd_tx = crate::acp::manager::manager()
        .get_cmd_tx(&session_id)
        .await
        .ok_or_else(|| {
            super::to_napi(ErgataiError::SessionNotFound(session_id))
        })?;

    cmd_tx
        .send(crate::acp::manager::SessionCommand::Close)
        .map_err(|_| super::to_napi(ErgataiError::ChannelError("Session task is dead".into())))?;

    Ok(true)
}

/// 列出所有活跃会话
#[napi]
pub async fn acp_list_sessions() -> Result<Vec<crate::acp::manager::NapiSessionInfo>> {
    guard();
    Ok(crate::acp::manager::manager().list_sessions().await)
}

/// 关闭所有会话（优雅关闭）
#[napi]
pub async fn acp_close_all_sessions() -> Result<()> {
    guard();
    tracing::info!("Closing all ACP sessions...");
    crate::acp::manager::manager().close_all().await;
    tracing::info!("All sessions closed");
    Ok(())
}

/// 轮询 session 事件（TS 侧定时调用）
#[napi]
pub fn acp_poll_events() -> Result<Vec<crate::acp::manager::NapiSessionEvent>> {
    Ok(crate::acp::manager::poll_events())
}

/// 从 agent 查询会话列表（通过 ACP session/list 协议）
#[napi]
pub async fn acp_list_agent_sessions(
    agent_name: String,
    cwd: Option<String>,
) -> Result<Vec<crate::acp::manager::NapiSessionInfo>> {
    guard();
    let config = crate::agent::config::get_agent_config(&agent_name)
        .map_err(super::to_napi)?;

    let sessions = crate::acp::session_ops::list_sessions_from_agent(&config, cwd)
        .await
        .map_err(|e| {
            super::to_napi(ErgataiError::internal(format!("ListSessions failed: {}", e)))
        })?;

    Ok(sessions
        .into_iter()
        .map(|s| crate::acp::manager::NapiSessionInfo {
            session_id: s.session_id.to_string(),
            agent_name: agent_name.clone(),
            cwd: s.cwd.to_string_lossy().into_owned(),
            status: "active".to_string(),
            title: s.title,
            updated_at: s.updated_at,
        })
        .collect())
}

/// 加载已有会话（通过 ACP session/load 协议）
#[napi]
pub async fn acp_load_session(
    agent_name: String,
    session_id: String,
    cwd: String,
) -> Result<String> {
    guard();
    let config = crate::agent::config::get_agent_config(&agent_name)
        .map_err(super::to_napi)?;

    let (tx, rx) = tokio::sync::oneshot::channel();
    crate::acp::session_ops::load_session_task(config, session_id.clone(), cwd, tx);

    match rx.await {
        Ok(Ok(sid)) => Ok(sid),
        Ok(Err(e)) => Err(super::to_napi(ErgataiError::internal(format!(
            "Load session failed: {}",
            e
        )))),
        Err(_) => Err(super::to_napi(ErgataiError::ChannelError(
            "Session task died before responding".into(),
        ))),
    }
}

/// 恢复已有会话（通过 ACP session/resume 协议）
#[napi]
pub async fn acp_resume_session(
    agent_name: String,
    session_id: String,
    cwd: String,
) -> Result<String> {
    guard();
    let config = crate::agent::config::get_agent_config(&agent_name)
        .map_err(super::to_napi)?;

    let (tx, rx) = tokio::sync::oneshot::channel();
    crate::acp::session_ops::resume_session_task(config, session_id.clone(), cwd, tx);

    match rx.await {
        Ok(Ok(sid)) => Ok(sid),
        Ok(Err(e)) => Err(super::to_napi(ErgataiError::internal(format!(
            "Resume session failed: {}",
            e
        )))),
        Err(_) => Err(super::to_napi(ErgataiError::ChannelError(
            "Session task died before responding".into(),
        ))),
    }
}

/// 响应权限请求（从前端发回给 agent）
#[napi]
pub async fn acp_respond_permission(
    session_id: String,
    request_id: String,
    option_id: Option<String>,
) -> Result<()> {
    guard();
    let cmd_tx = crate::acp::manager::manager()
        .get_cmd_tx(&session_id)
        .await
        .ok_or_else(|| {
            super::to_napi(ErgataiError::SessionNotFound(session_id))
        })?;

    cmd_tx
        .send(crate::acp::manager::SessionCommand::PermissionResponse {
            request_id,
            option_id,
        })
        .map_err(|_| super::to_napi(ErgataiError::ChannelError("Session task is dead".into())))?;

    Ok(())
}

/// 设置会话模式
#[napi]
pub async fn acp_set_session_mode(session_id: String, mode_id: String) -> Result<()> {
    guard();
    let cmd_tx = crate::acp::manager::manager()
        .get_cmd_tx(&session_id)
        .await
        .ok_or_else(|| {
            super::to_napi(ErgataiError::SessionNotFound(session_id))
        })?;

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    cmd_tx
        .send(crate::acp::manager::SessionCommand::SetMode {
            mode_id,
            reply_tx,
        })
        .map_err(|_| super::to_napi(ErgataiError::ChannelError("Session task is dead".into())))?;

    match reply_rx.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(super::to_napi(ErgataiError::internal(format!(
            "SetMode failed: {}",
            e
        )))),
        Err(_) => Err(super::to_napi(ErgataiError::ChannelError(
            "Session task died".into(),
        ))),
    }
}

/// 设置会话配置选项
#[napi]
pub async fn acp_set_config_option(
    session_id: String,
    config_id: String,
    value_id: String,
) -> Result<()> {
    guard();
    let cmd_tx = crate::acp::manager::manager()
        .get_cmd_tx(&session_id)
        .await
        .ok_or_else(|| {
            super::to_napi(ErgataiError::SessionNotFound(session_id))
        })?;

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    cmd_tx
        .send(crate::acp::manager::SessionCommand::SetConfigOption {
            config_id,
            value_id,
            reply_tx,
        })
        .map_err(|_| super::to_napi(ErgataiError::ChannelError("Session task is dead".into())))?;

    match reply_rx.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(super::to_napi(ErgataiError::internal(format!(
            "SetConfigOption failed: {}",
            e
        )))),
        Err(_) => Err(super::to_napi(ErgataiError::ChannelError(
            "Session task died".into(),
        ))),
    }
}

/// 从 agent 删除会话（通过 ACP session/delete 协议）+ 清除本地持久化
#[napi]
pub async fn acp_delete_session(agent_name: String, session_id: String) -> Result<()> {
    guard();
    let config = crate::agent::config::get_agent_config(&agent_name)
        .map_err(super::to_napi)?;

    // 尝试从 agent 删除
    let _ = crate::acp::session_ops::delete_session_from_agent(&config, &session_id).await;

    // 清除本地持久化
    let _ = crate::acp::persistence::delete_session(&session_id);

    Ok(())
}

/// 获取本地持久化的会话列表
#[napi]
pub fn acp_get_persisted_sessions() -> Result<Vec<crate::acp::persistence::PersistedSession>> {
    guard();
    crate::acp::persistence::load_all_sessions().map_err(|e| {
        super::to_napi(ErgataiError::internal(format!("Failed to load sessions: {}", e)))
    })
}

/// 保存会话元数据到本地
#[napi]
pub fn acp_save_session_meta(
    session_id: String,
    agent_name: String,
    cwd: String,
    title: Option<String>,
) -> Result<()> {
    guard();
    let now = chrono::Utc::now().to_rfc3339();
    let session = crate::acp::persistence::PersistedSession {
        session_id,
        agent_name,
        cwd,
        title,
        created_at: now.clone(),
        updated_at: now,
    };
    crate::acp::persistence::save_session(&session).map_err(|e| {
        super::to_napi(ErgataiError::internal(format!("Failed to save session: {}", e)))
    })
}

/// 更新会话标题
#[napi]
pub fn acp_update_session_title(session_id: String, title: String) -> Result<()> {
    guard();
    crate::acp::persistence::update_session_title(&session_id, &title).map_err(|e| {
        super::to_napi(ErgataiError::internal(format!("Failed to update title: {}", e)))
    })
}

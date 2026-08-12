use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::error::{ConfigError, ErgataiResult};

/// 本地保存的会话元数据（用于离线浏览和历史记录）
#[napi_derive::napi(object)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedSession {
    pub session_id: String,
    pub agent_name: String,
    pub cwd: String,
    pub title: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 获取会话存储目录
fn sessions_dir() -> ErgataiResult<PathBuf> {
    let config_dir = dirs::config_dir().ok_or(ConfigError::DirectoryNotFound)?;
    Ok(config_dir.join("ergatai").join("sessions"))
}

/// 保存会话元数据到磁盘
pub fn save_session(session: &PersistedSession) -> ErgataiResult<()> {
    let dir = sessions_dir()?;
    std::fs::create_dir_all(&dir)?;

    let path = session_path(&session.session_id)?;
    let content = serde_json::to_string_pretty(session)?;
    std::fs::write(&path, content)?;
    Ok(())
}

/// 加载所有保存的会话
pub fn load_all_sessions() -> ErgataiResult<Vec<PersistedSession>> {
    let dir = sessions_dir()?;
    if !dir.exists() {
        return Ok(vec![]);
    }

    let mut sessions = vec![];
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    if let Ok(session) = serde_json::from_str::<PersistedSession>(&content) {
                        sessions.push(session);
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to read session file {:?}: {}", path, e);
                }
            }
        }
    }

    // 按更新时间降序排列
    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(sessions)
}

/// 加载单个会话
pub fn load_session(session_id: &str) -> ErgataiResult<Option<PersistedSession>> {
    let path = session_path(session_id)?;
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)?;
    let session = serde_json::from_str(&content)?;
    Ok(Some(session))
}

/// 删除会话文件
pub fn delete_session(session_id: &str) -> ErgataiResult<()> {
    let path = session_path(session_id)?;
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

/// 更新会话标题
pub fn update_session_title(session_id: &str, title: &str) -> ErgataiResult<()> {
    if let Some(mut session) = load_session(session_id)? {
        session.title = Some(title.to_string());
        session.updated_at = chrono::Utc::now().to_rfc3339();
        save_session(&session)?;
    }
    Ok(())
}

fn session_path(session_id: &str) -> ErgataiResult<PathBuf> {
    // 用 session_id 的安全版本做文件名
    let safe_name: String = session_id
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    Ok(sessions_dir()?.join(format!("{}.json", safe_name)))
}

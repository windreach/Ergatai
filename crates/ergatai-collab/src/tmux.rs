//! Tmux Manager - 终端复用器管理
//!
//! 管理 tmux 会话和 pane，实现消息注入和内容捕获。
//! 这是 Ergatai 多 agent 协作的核心组件。

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Agent 在 tmux 中的信息
#[derive(Debug, Clone)]
pub struct TmuxAgent {
    pub agent_id: String,
    pub session: String,
    pub pane: String,
    pub command: String,
    pub mapped_to_mcp: Option<String>, // MCP agent ID if mapped
}

/// Tmux 管理器
pub struct TmuxManager {
    /// 默认 session 名称
    default_session: String,
    /// Agent 列表: agent_id -> TmuxAgent
    agents: Arc<RwLock<HashMap<String, TmuxAgent>>>,
    /// MCP agent_id 到 tmux pane 的映射
    /// Key: MCP agent_id (如 "opencode@9c15c5e4")
    /// Value: tmux pane ID (如 "ergatai-opencode:0.1")
    mcp_to_tmux_map: Arc<RwLock<HashMap<String, String>>>,
    /// 下一个 pane 索引
    next_pane_index: Arc<RwLock<u32>>,
}

impl TmuxManager {
    /// 创建新的 TmuxManager
    pub fn new(session_name: &str) -> Self {
        Self {
            default_session: session_name.to_string(),
            agents: Arc::new(RwLock::new(HashMap::new())),
            mcp_to_tmux_map: Arc::new(RwLock::new(HashMap::new())),
            next_pane_index: Arc::new(RwLock::new(0)),
        }
    }

    /// 检查 tmux 是否可用
    pub async fn check_tmux() -> Result<()> {
        let output = Command::new("tmux")
            .arg("-V")
            .output()
            .await
            .context("Failed to execute tmux command. Is tmux installed?")?;

        if !output.status.success() {
            anyhow::bail!("tmux command failed");
        }

        let version = String::from_utf8_lossy(&output.stdout);
        info!("tmux version: {}", version.trim());
        Ok(())
    }

    /// 创建新的 tmux session
    pub async fn create_session(&self, width: u32, height: u32) -> Result<()> {
        info!(
            "Creating tmux session: {} ({}x{})",
            self.default_session, width, height
        );

        let output = Command::new("tmux")
            .args(&[
                "new-session",
                "-d",
                "-s",
                &self.default_session,
                "-x",
                &width.to_string(),
                "-y",
                &height.to_string(),
            ])
            .output()
            .await
            .context("Failed to create tmux session")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to create tmux session: {}", stderr);
        }

        info!("Tmux session created: {}", self.default_session);
        Ok(())
    }

    /// 启动 agent 到新的 pane
    pub async fn launch_agent(&self, agent_id: &str, command: &str) -> Result<String> {
        info!("Launching agent {} with command: {}", agent_id, command);

        // 获取下一个 pane 索引
        let mut next_index = self.next_pane_index.write().await;
        let pane_index = *next_index;
        *next_index += 1;

        let pane_id = if pane_index == 0 {
            // 第一个 pane 使用 session 的默认 pane
            format!("{}:0.0", self.default_session)
        } else {
            // 分割窗口创建新 pane
            let output = Command::new("tmux")
                .args(&[
                    "split-window",
                    "-t",
                    &self.default_session,
                    "-h", // 水平分割
                    "-P", // 打印新 pane 的 ID
                    "-F",
                    "#{pane_id}",
                ])
                .output()
            .await
                .context("Failed to split window")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("Failed to split window: {}", stderr);
            }

            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };

        // 在 pane 中启动 agent
        let target = if pane_index == 0 {
            format!("{}:0.0", self.default_session)
        } else {
            pane_id.clone()
        };

        let output = Command::new("tmux")
            .args(&["send-keys", "-t", &target, command, "Enter"])
            .output()
            .await
            .context("Failed to send command to pane")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to launch agent: {}", stderr);
        }

        // 记录 agent 信息
        let agent = TmuxAgent {
            agent_id: agent_id.to_string(),
            session: self.default_session.clone(),
            pane: target.clone(),
            command: command.to_string(),
            mapped_to_mcp: None,
        };

        self.agents
            .write()
            .await
            .insert(agent_id.to_string(), agent);

        info!("Agent {} launched in pane {}", agent_id, target);
        Ok(target)
    }

    /// 向 agent 注入消息
    pub async fn inject_message(&self, agent_id: &str, message: &str) -> Result<()> {
        debug!("Injecting message to agent {}: {}", agent_id, message);

        let agents = self.agents.read().await;
        let agent = agents
            .get(agent_id)
            .ok_or_else(|| anyhow::anyhow!("Agent {} not found", agent_id))?;

        // 使用 send-keys -l 注入消息（-l 表示字面量，不会解释特殊按键名称）
        let output = Command::new("tmux")
            .args(&[
                "send-keys",
                "-l",  // Literal mode: treat message as text, not key names
                "-t",
                &agent.pane,
                message,
            ])
            .output()
            .await
            .context("Failed to inject message")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to inject message: {}", stderr);
        }

        // 发送 Enter 键来提交消息
        let output = Command::new("tmux")
            .args(&[
                "send-keys",
                "-t",
                &agent.pane,
                "Enter",
            ])
            .output()
            .await
            .context("Failed to send Enter key")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to inject message: {}", stderr);
        }

        info!("Message injected to agent {}", agent_id);
        Ok(())
    }

    /// 捕获 agent 的输出
    pub async fn capture_pane(&self, agent_id: &str) -> Result<String> {
        debug!("Capturing output from agent {}", agent_id);

        let agents = self.agents.read().await;
        let agent = agents
            .get(agent_id)
            .ok_or_else(|| anyhow::anyhow!("Agent {} not found", agent_id))?;

        // 使用 capture-pane 捕获内容
        let output = Command::new("tmux")
            .args(&["capture-pane", "-t", &agent.pane, "-p"])
            .output()
            .await
            .context("Failed to capture pane")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to capture pane: {}", stderr);
        }

        let content = String::from_utf8_lossy(&output.stdout).to_string();
        Ok(content)
    }

    /// 列出所有 agent
    pub async fn list_agents(&self) -> Vec<TmuxAgent> {
        self.agents.read().await.values().cloned().collect()
    }

    /// 获取 agent 信息
    pub async fn get_agent(&self, agent_id: &str) -> Option<TmuxAgent> {
        self.agents.read().await.get(agent_id).cloned()
    }

    /// 停止 agent（关闭 pane）
    pub async fn stop_agent(&self, agent_id: &str) -> Result<()> {
        info!("Stopping agent {}", agent_id);

        let mut agents = self.agents.write().await;
        let agent = agents
            .remove(agent_id)
            .ok_or_else(|| anyhow::anyhow!("Agent {} not found", agent_id))?;

        // 关闭 pane
        let output = Command::new("tmux")
            .args(&["kill-pane", "-t", &agent.pane])
            .output()
            .await
            .context("Failed to kill pane")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Failed to kill pane (may already be closed): {}", stderr);
        }

        info!("Agent {} stopped", agent_id);
        Ok(())
    }

    /// 关闭整个 session
    pub async fn kill_session(&self) -> Result<()> {
        info!("Killing tmux session: {}", self.default_session);

        let output = Command::new("tmux")
            .args(&["kill-session", "-t", &self.default_session])
            .output()
            .await
            .context("Failed to kill session")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Failed to kill session (may already be closed): {}", stderr);
        }

        // 清空 agent 列表
        self.agents.write().await.clear();
        *self.next_pane_index.write().await = 0;

        info!("Tmux session killed");
        Ok(())
    }

    /// 扫描 tmux session 中的所有 pane，注册为 agent
    ///
    /// 这个方法用于发现已经在 tmux 中运行但没有通过 MCP 连接的 agent。
    /// 扫描后，这些 agent 会被添加到 TmuxManager 的内部列表中。
    pub async fn scan_and_register_panes(&self) -> Result<Vec<String>> {
        info!("Scanning tmux session for existing panes: {}", self.default_session);

        // 使用 tmux list-panes 获取所有 pane
        let output = Command::new("tmux")
            .args(&[
                "list-panes",
                "-t",
                &self.default_session,
                "-F",
                "#{pane_id}:#{pane_current_command}:#{pane_pid}",
            ])
            .output()
            .await
            .context("Failed to list panes")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to list panes: {}", stderr);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut registered_agents = Vec::new();

        // 解析每一行：pane_id:command:pid
        for line in stdout.lines() {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() >= 3 {
                let pane_id = parts[0];
                let command = parts[1];
                let pid = parts[2];

                // 使用 pane_id 作为唯一标识（稳定，不会重复）
                // 格式：pane_%{数字}，例如 pane_%0, pane_%1
                let agent_id = format!("pane_{}", pane_id.replace('%', ""));

                // 检查是否已经注册
                if self.agents.read().await.contains_key(&agent_id) {
                    debug!("Agent {} already registered, skipping", agent_id);
                    continue;
                }

                // 注册 agent
                let agent = TmuxAgent {
                    agent_id: agent_id.clone(),
                    session: self.default_session.clone(),
                    pane: pane_id.to_string(),
                    command: command.to_string(),
                    mapped_to_mcp: None,
                };

                self.agents.write().await.insert(agent_id.clone(), agent);
                registered_agents.push(agent_id.clone());

                info!("Registered tmux pane as agent: {} (command: {}, pid: {})", agent_id, command, pid);
            }
        }

        info!("Scanned and registered {} agents", registered_agents.len());
        Ok(registered_agents)
    }

    /// 检查 agent 是否在 tmux 中
    pub async fn is_agent_in_tmux(&self, agent_id: &str) -> bool {
        self.agents.read().await.contains_key(agent_id)
    }

    /// Atomically find an unmapped tmux pane and claim it for the given MCP agent.
    ///
    /// Returns the claimed pane ID, or `None` if no unmapped pane exists.
    /// This method holds the agents write lock across the find-and-claim to prevent
    /// concurrent MCP connections from mapping to the same pane.
    pub async fn try_claim_unmapped_pane(&self, mcp_agent_id: &str) -> Option<String> {
        let mut agents = self.agents.write().await;
        let unmapped = agents.values_mut().find(|a| a.mapped_to_mcp.is_none())?;
        let pane = unmapped.pane.clone();
        unmapped.mapped_to_mcp = Some(mcp_agent_id.to_string());
        drop(agents); // release agents lock before acquiring mcp_to_tmux_map lock

        // Also record in the mcp_to_tmux_map
        self.mcp_to_tmux_map
            .write()
            .await
            .insert(mcp_agent_id.to_string(), pane.clone());

        info!(
            "Atomically claimed tmux pane {} for MCP agent {}",
            pane, mcp_agent_id
        );
        Some(pane)
    }

    /// 注册 MCP agent_id 到 tmux pane 的映射
    ///
    /// 这用于将 MCP 连接（如 "opencode@9c15c5e4"）映射到 tmux pane。
    /// 当 agent 通过 MCP 连接时，我们可以根据进程信息找到对应的 tmux pane。
    pub async fn register_mcp_to_tmux_mapping(
        &self,
        mcp_agent_id: &str,
        tmux_pane: &str,
    ) -> Result<()> {
        info!(
            "Registering MCP to tmux mapping: {} -> {}",
            mcp_agent_id, tmux_pane
        );

        // 存储映射关系
        self.mcp_to_tmux_map
            .write()
            .await
            .insert(mcp_agent_id.to_string(), tmux_pane.to_string());

        // 标记对应的 tmux agent 为已映射
        let mut agents = self.agents.write().await;
        for agent in agents.values_mut() {
            if agent.pane == tmux_pane {
                agent.mapped_to_mcp = Some(mcp_agent_id.to_string());
                break;
            }
        }

        Ok(())
    }

    /// 根据 MCP agent_id 获取 tmux pane
    pub async fn get_tmux_pane_for_mcp_agent(&self, mcp_agent_id: &str) -> Option<String> {
        self.mcp_to_tmux_map.read().await.get(mcp_agent_id).cloned()
    }

    /// 根据 MCP agent_id 注入消息
    ///
    /// 这会查找 MCP agent_id 对应的 tmux pane，然后注入消息。
    pub async fn inject_message_by_mcp_id(
        &self,
        mcp_agent_id: &str,
        message: &str,
    ) -> Result<()> {
        // 查找映射
        let tmux_pane = self
            .get_tmux_pane_for_mcp_agent(mcp_agent_id)
            .await
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No tmux pane mapped for MCP agent {}. Call register_mcp_to_tmux_mapping first.",
                    mcp_agent_id
                )
            })?;

        info!(
            "Injecting message to MCP agent {} via tmux pane {}",
            mcp_agent_id, tmux_pane
        );

        // 直接注入到 tmux pane（使用 -l 字面量模式）
        let output = Command::new("tmux")
            .args(&["send-keys", "-l", "-t", &tmux_pane, message])
            .output()
            .await
            .context("Failed to inject message via tmux")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to inject message: {}", stderr);
        }

        // 发送 Enter 键来提交消息
        let output = Command::new("tmux")
            .args(&["send-keys", "-t", &tmux_pane, "Enter"])
            .output()
            .await
            .context("Failed to send Enter key")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to inject message: {}", stderr);
        }

        info!("Message injected to MCP agent {} via tmux", mcp_agent_id);
        Ok(())
    }
}

impl Default for TmuxManager {
    fn default() -> Self {
        Self::new("ergatai")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_check_tmux() {
        // 这个测试需要 tmux 安装
        let result = TmuxManager::check_tmux().await;
        assert!(result.is_ok());
    }
}

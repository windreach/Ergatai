//! 扫描 tmux 并注册 agent
//!
//! 运行: cargo run --bin scan-tmux-agents

use ergatai_core::tmux::TmuxManager;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter("ergatai=info")
        .init();

    println!("🔍 扫描 tmux session 中的 agent");
    println!("================================\n");

    // 创建 tmux manager
    let manager = Arc::new(TmuxManager::new("ergatai"));

    // 检查 tmux
    println!("1. 检查 tmux...");
    TmuxManager::check_tmux().await?;
    println!("✅ tmux 可用\n");

    // 扫描 pane
    println!("2. 扫描 tmux session 'ergatai'...");
    let registered = manager.scan_and_register_panes().await?;

    if registered.is_empty() {
        println!("⚠️  没有找到任何 pane");
        println!("\n💡 提示：");
        println!("   1. 确保 tmux session 'ergatai' 存在");
        println!("   2. 或者先运行: ./test-opencode-collaboration.sh");
        return Ok(());
    }

    println!("✅ 发现并注册了 {} 个 agent\n", registered.len());

    // 列出所有 agent
    println!("3. 已注册的 agent:");
    let agents = manager.list_agents().await;
    for agent in agents {
        println!("   - {} (pane: {}, command: {})",
                 agent.agent_id, agent.pane, agent.command);
    }
    println!();

    println!("🎉 扫描完成！");
    println!("\n💡 现在这些 agent 可以通过 Ergatai 进行通信");
    println!("   Agent 可以调用 MCP 工具 send_message 发送消息");
    println!("   Ergatai 会通过 tmux 注入消息到目标 agent");

    Ok(())
}

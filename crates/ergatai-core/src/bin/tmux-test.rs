//! Tmux Manager 测试二进制
//!
//! 运行: cargo run --bin tmux-test

use ergatai_core::tmux::TmuxManager;
use std::time::Duration;
use tokio::time::sleep;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter("ergatai=info")
        .init();

    println!("🚀 TmuxManager 测试");
    println!("====================\n");

    // 检查 tmux 是否可用
    println!("1. 检查 tmux...");
    TmuxManager::check_tmux().await?;
    println!("✅ tmux 可用\n");

    // 创建 manager
    let manager = TmuxManager::new("ergatai-test");

    // 创建 session
    println!("2. 创建 tmux session...");
    manager.create_session(200, 50).await?;
    println!("✅ Session 创建成功\n");

    // 启动第一个 agent（使用简单的命令演示）
    println!("3. 启动 Agent A (echo 命令)...");
    let pane_a = manager
        .launch_agent("agent-a", "echo 'Agent A started' && sleep 100")
        .await?;
    println!("✅ Agent A 启动在 pane: {}\n", pane_a);

    // 等待一下让 agent 启动
    sleep(Duration::from_secs(2)).await;

    // 启动第二个 agent
    println!("4. 启动 Agent B...");
    let pane_b = manager
        .launch_agent("agent-b", "echo 'Agent B started' && sleep 100")
        .await?;
    println!("✅ Agent B 启动在 pane: {}\n", pane_b);

    sleep(Duration::from_secs(2)).await;

    // 列出所有 agent
    println!("5. 列出所有 agent:");
    let agents = manager.list_agents().await;
    for agent in agents {
        println!("   - {} (pane: {})", agent.agent_id, agent.pane);
    }
    println!();

    // 捕获 agent A 的输出
    println!("6. 捕获 Agent A 的输出:");
    let output = manager.capture_pane("agent-a").await?;
    println!("---");
    println!("{}", output);
    println!("---\n");

    // 向 agent B 注入消息
    println!("7. 向 Agent B 注入消息...");
    manager
        .inject_message("agent-b", "echo 'Message from Agent A: Hello!'")
        .await?;
    println!("✅ 消息已注入\n");

    sleep(Duration::from_secs(2)).await;

    // 再次捕获 agent B 的输出
    println!("8. 捕获 Agent B 的输出（应该看到注入的消息）:");
    let output = manager.capture_pane("agent-b").await?;
    println!("---");
    println!("{}", output);
    println!("---\n");

    // 停止 agent
    println!("9. 停止 Agent A...");
    manager.stop_agent("agent-a").await?;
    println!("✅ Agent A 已停止\n");

    // 列出 agent（应该只有 B）
    println!("10. 列出剩余 agent:");
    let agents = manager.list_agents().await;
    for agent in agents {
        println!("   - {} (pane: {})", agent.agent_id, agent.pane);
    }
    println!();

    // 关闭整个 session
    println!("11. 关闭整个 session...");
    manager.kill_session().await?;
    println!("✅ Session 已关闭\n");

    println!("🎉 测试完成！");

    Ok(())
}

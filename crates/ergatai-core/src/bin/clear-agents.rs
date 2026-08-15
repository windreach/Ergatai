//! 清除所有注册的 agent
//!
//! 运行: cargo run --bin clear-agents

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("ergatai=info")
        .init();

    println!("🧹 清除所有注册的 Agent");
    println!("========================\n");

    // 这个方法需要访问 AgentRegistry，但我们没有直接访问
    // 简化版本：提示用户重启 Ergatai

    println!("💡 清除 agent 的最简单方法是重启 Ergatai：\n");
    println!("   1. 停止 Ergatai:");
    println!("      pkill -9 -f ergatai-api");
    println!("");
    println!("   2. 清理 tmux session:");
    println!("      tmux kill-session -t ergatai-opencode");
    println!("");
    println!("   3. 重新启动:");
    println!("      ./clean-restart.sh");
    println!("");
    println!("   4. 运行测试:");
    println!("      ./test-opencode-auto-register.sh");
    println!("");

    println!("🎯 或者使用一键清理脚本：");
    println!("   ./clean-restart.sh");
    println!("");

    Ok(())
}

//! NATS Transport Demo
//!
//! 运行: cd network-demo && cargo run --bin nats_demo
//! 需要先启动 NATS server: docker run -d --name nats-server -p 4222:4222 nats:latest -js

mod transport;
mod state;
mod in_memory;
mod nats_transport;
mod message;
mod parser;
mod formatter;
mod task;
mod agent_network;

use nats_transport::NatsTransport;
use transport::{AgentTransport, Message};
use std::sync::Arc;
use tokio::time::{sleep, Duration};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== NATS Transport Demo ===\n");

    // 1. 连接到 NATS server
    println!("[1] 连接到 NATS server (nats://localhost:4222)...");
    let transport = Arc::new(NatsTransport::connect("nats://localhost:4222", "demo").await?);
    println!("✓ 连接成功\n");

    // 2. 注册三个 agent
    println!("[2] 注册 agents...");
    let mut rx_alice = transport.subscribe(&"alice".to_string()).await?;
    let mut rx_bob = transport.subscribe(&"bob".to_string()).await?;
    let mut rx_charlie = transport.subscribe(&"charlie".to_string()).await?;
    println!("✓ Alice, Bob, Charlie 已上线\n");

    // 3. 加入频道
    println!("[3] 加入频道 code_review...");
    transport.join_channel(&"bob".to_string(), "code_review").await?;
    transport.join_channel(&"charlie".to_string(), "code_review").await?;
    println!("✓ Bob 和 Charlie 订阅了 code_review 频道\n");

    // 4. 健康检查（初始状态）
    println!("[4] 健康检查");
    let health = transport.health_check();
    println!("  健康状态: {}", if health.healthy { "✓ 健康" } else { "✗ 异常" });
    println!("  在线 agents: {} - {:?}", health.agent_count, health.agents);
    println!("  活跃频道: {} - {:?}", health.channel_count, health.channels);
    println!("  连接状态: {}", health.details["connected"]);
    println!();

    // 5. 点对点通信：Alice 发任务给 Bob
    println!("[5] 点对点通信：Alice → Bob");
    transport.send(
        &"bob".to_string(),
        Message::Task {
            from: "alice".to_string(),
            task_id: "task-001".to_string(),
            payload: serde_json::json!({
                "action": "review_code",
                "file": "src/main.rs",
                "priority": "high"
            }),
        },
    ).await?;
    println!("✓ Alice 发送了代码审查任务给 Bob\n");

    // Bob 接收任务
    let msg = tokio::time::timeout(Duration::from_secs(5), rx_bob.recv())
        .await?
        .ok_or_else(|| anyhow::anyhow!("Channel closed"))?;
    if let Message::Task { from, task_id, payload } = msg.msg {
        println!("Bob 收到任务:");
        println!("  来自: {}", from);
        println!("  任务ID: {}", task_id);
        println!("  内容: {}\n", payload);
    }

    // 6. 发布订阅：Alice 广播事件到频道
    println!("[6] 发布订阅：Alice → code_review 频道");
    transport.publish(
        "code_review",
        Message::Event {
            from: "alice".to_string(),
            channel: "code_review".to_string(),
            data: serde_json::json!({
                "event": "code_pushed",
                "branch": "feature/nats-demo",
                "commits": 3
            }),
        },
    ).await?;
    println!("✓ Alice 广播了代码推送事件\n");

    // Bob 和 Charlie 都会收到
    let msg_bob = tokio::time::timeout(Duration::from_secs(5), rx_bob.recv())
        .await?
        .ok_or_else(|| anyhow::anyhow!("Bob channel closed"))?;
    let msg_charlie = tokio::time::timeout(Duration::from_secs(5), rx_charlie.recv())
        .await?
        .ok_or_else(|| anyhow::anyhow!("Charlie channel closed"))?;

    if let Message::Event { from, channel, data } = msg_bob.msg {
        println!("Bob 收到事件: {} → {} ({})", from, channel, data);
    }
    if let Message::Event { from, channel, data } = msg_charlie.msg {
        println!("Charlie 收到事件: {} → {} ({})\n", from, channel, data);
    }

    // 7. 任务回报：Bob 完成任务后通知 Alice
    println!("[7] 任务回报：Bob → Alice");
    sleep(Duration::from_millis(500)).await; // 模拟处理时间

    transport.send(
        &"alice".to_string(),
        Message::Result {
            from: "bob".to_string(),
            task_id: "task-001".to_string(),
            payload: serde_json::json!({
                "status": "completed",
                "issues_found": 2,
                "suggestions": ["使用更清晰的变量名", "添加错误处理"]
            }),
        },
    ).await?;
    println!("✓ Bob 完成了任务并回报给 Alice\n");

    // Alice 接收结果
    let msg = tokio::time::timeout(Duration::from_secs(5), rx_alice.recv())
        .await?
        .ok_or_else(|| anyhow::anyhow!("Alice channel closed"))?;
    if let Message::Result { from, task_id, payload } = msg.msg {
        println!("Alice 收到结果:");
        println!("  来自: {}", from);
        println!("  任务ID: {}", task_id);
        println!("  结果: {}\n", payload);
    }

    // 8. 检查 agent 状态
    println!("[8] Agent 状态检查");
    println!("  Alice 在线: {}", transport.is_alive(&"alice".to_string()));
    println!("  Bob 在线: {}", transport.is_alive(&"bob".to_string()));
    println!("  Charlie 在线: {}", transport.is_alive(&"charlie".to_string()));

    // 9. 注销 agent
    println!("\n[9] 注销 Charlie");
    transport.unsubscribe(&"charlie".to_string()).await?;
    println!("✓ Charlie 已下线");
    println!("  Charlie 在线: {}", transport.is_alive(&"charlie".to_string()));

    // 10. 测试发送给已注销的 agent
    println!("\n[10] 测试错误处理：发送给已注销的 agent");
    let result = transport.send(
        &"charlie".to_string(),
        Message::Event {
            from: "alice".to_string(),
            channel: "test".to_string(),
            data: serde_json::json!({"test": true}),
        },
    ).await;

    match result {
        Ok(_) => println!("✗ 不应该成功"),
        Err(e) => println!("✓ 预期错误: {}", e),
    }

    // 11. 再次健康检查
    println!("\n[11] 健康检查（最终状态）");
    let health = transport.health_check();
    println!("  健康状态: {}", if health.healthy { "✓ 健康" } else { "✗ 异常" });
    println!("  在线 agents: {} - {:?}", health.agent_count, health.agents);
    println!("  活跃频道: {} - {:?}", health.channel_count, health.channels);

    // 12. 优雅关闭
    println!("\n[12] 优雅关闭");
    transport.shutdown().await;
    println!("✓ 传输层已关闭");

    // 验证关闭后状态
    let health_after = transport.health_check();
    println!("  关闭后 agents: {}", health_after.agent_count);
    println!("  关闭后 channels: {}", health_after.channel_count);

    println!("\n=== NATS Demo 完成 ===");
    println!("\n提示：消息已通过 NATS JetStream 持久化，即使重启 server 也不会丢失。");
    println!("查看 NATS 监控: http://localhost:8222");

    Ok(())
}

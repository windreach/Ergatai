//! Agent Network Demo
//!
//! 演示如何使用 network 模块实现多 agent 通信
//!
//! 运行: cargo run --example network_demo

use ergatai::network::{
    agent_network::AgentNetwork,
    in_memory::InMemoryTransport,
    message::Message,
    transport::AgentId,
};
use std::sync::Arc;
use tokio::time::{sleep, Duration};

#[tokio::main]
async fn main() {
    println!("=== Agent Network Demo ===\n");

    // 1. 创建网络（使用 InMemory 传输层）
    let transport = Arc::new(InMemoryTransport::new());
    let network = AgentNetwork::new(transport.clone());

    // 2. 注册三个 agent
    println!("[1] 注册 agents...");
    let mut rx_alice = network.register_agent("alice".to_string()).await.unwrap();
    let mut rx_bob = network.register_agent("bob".to_string()).await.unwrap();
    let mut rx_charlie = network.register_agent("charlie".to_string()).await.unwrap();
    println!("✓ Alice, Bob, Charlie 已上线\n");

    // 3. 加入频道（用于发布订阅）
    println!("[2] 加入频道 code_review...");
    transport.join_channel(&"bob".to_string(), "code_review");
    transport.join_channel(&"charlie".to_string(), "code_review");
    println!("✓ Bob 和 Charlie 订阅了 code_review 频道\n");

    // 4. 点对点通信：Alice 发任务给 Bob
    println!("[3] 点对点通信：Alice → Bob");
    transport
        .send(
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
        )
        .await
        .unwrap();
    println!("✓ Alice 发送了代码审查任务给 Bob\n");

    // Bob 接收任务
    let msg = rx_bob.recv().await.unwrap();
    if let Message::Task { from, task_id, payload } = msg.msg {
        println!("Bob 收到任务:");
        println!("  来自: {}", from);
        println!("  任务ID: {}", task_id);
        println!("  内容: {}\n", payload);
    }

    // 5. 发布订阅：Alice 广播事件到频道
    println!("[4] 发布订阅：Alice → code_review 频道");
    transport
        .publish(
            "code_review",
            Message::Event {
                from: "alice".to_string(),
                channel: "code_review".to_string(),
                data: serde_json::json!({
                    "event": "code_pushed",
                    "branch": "feature/network-demo",
                    "commits": 3
                }),
            },
        )
        .await
        .unwrap();
    println!("✓ Alice 广播了代码推送事件\n");

    // Bob 和 Charlie 都会收到
    let msg_bob = rx_bob.recv().await.unwrap();
    let msg_charlie = rx_charlie.recv().await.unwrap();

    if let Message::Event { from, channel, data } = msg_bob.msg {
        println!("Bob 收到事件: {} → {} ({})", from, channel, data);
    }
    if let Message::Event { from, channel, data } = msg_charlie.msg {
        println!("Charlie 收到事件: {} → {} ({})\n", from, channel, data);
    }

    // 6. 任务回报：Bob 完成任务后通知 Alice
    println!("[5] 任务回报：Bob → Alice");
    sleep(Duration::from_millis(500)).await; // 模拟处理时间

    transport
        .send(
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
        )
        .await
        .unwrap();
    println!("✓ Bob 完成了任务并回报给 Alice\n");

    // Alice 接收结果
    let msg = rx_alice.recv().await.unwrap();
    if let Message::Result { from, task_id, payload } = msg.msg {
        println!("Alice 收到结果:");
        println!("  来自: {}", from);
        println!("  任务ID: {}", task_id);
        println!("  结果: {}\n", payload);
    }

    // 7. 检查 agent 状态
    println!("[6] Agent 状态检查");
    println!("  Alice 在线: {}", transport.is_alive(&"alice".to_string()));
    println!("  Bob 在线: {}", transport.is_alive(&"bob".to_string()));
    println!("  Charlie 在线: {}", transport.is_alive(&"charlie".to_string()));

    // 8. 注销 agent
    println!("\n[7] 注销 Charlie");
    network.unregister_agent(&"charlie".to_string()).await.unwrap();
    println!("✓ Charlie 已下线");
    println!("  Charlie 在线: {}", transport.is_alive(&"charlie".to_string()));

    // 9. 测试发送给已注销的 agent
    println!("\n[8] 测试错误处理：发送给已注销的 agent");
    let result = transport
        .send(
            &"charlie".to_string(),
            Message::Event {
                from: "alice".to_string(),
                channel: "test".to_string(),
                data: serde_json::json!({"test": true}),
            },
        )
        .await;

    match result {
        Ok(_) => println!("✗ 不应该成功"),
        Err(e) => println!("✓ 预期错误: {}", e),
    }

    println!("\n=== Demo 完成 ===");
}

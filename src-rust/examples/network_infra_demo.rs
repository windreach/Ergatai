//! 网络基础设施 Demo
//!
//! 演示指标监控、重试/熔断、限流等功能

use ergatai::network::{
    metrics::NetworkMetrics,
    resilience::{CircuitBreakerConfig, ResilientExecutor, RetryConfig},
    rate_limit::{RateLimiter, RateLimitConfig, RateLimiterWithStats},
    transport::{AgentTransport, Message},
    in_memory::InMemoryTransport,
};
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() {
    println!("=== 网络基础设施 Demo ===\n");

    // 1. 指标监控
    println!("[1] 指标监控");
    let metrics = Arc::new(NetworkMetrics::new());

    // 模拟一些消息
    metrics.record_send("agent_a", ergatai::network::metrics::MessageType::PointToPoint, Duration::from_millis(10));
    metrics.record_send("agent_b", ergatai::network::metrics::MessageType::Broadcast, Duration::from_millis(20));
    metrics.record_receive("agent_a");
    metrics.record_failure("agent_c");

    let snapshot = metrics.snapshot();
    println!("  总发送: {}", snapshot.total_sent);
    println!("  总接收: {}", snapshot.total_received);
    println!("  总失败: {}", snapshot.total_failed);
    println!("  平均延迟: {:.2}ms", snapshot.avg_latency_ms);
    println!("  P95 延迟: {:.2}ms", snapshot.p95_latency_ms);
    println!("  错误率: {:.2}%", snapshot.error_rate * 100.0);
    println!("  吞吐量: {:.2} msg/s", snapshot.throughput);
    println!();

    // 2. 重试机制
    println!("[2] 重试机制");
    let retry_config = RetryConfig {
        max_retries: 3,
        initial_delay: Duration::from_millis(100),
        max_delay: Duration::from_secs(5),
        multiplier: 2.0,
        jitter: true,
    };

    let retry_executor = ergatai::network::resilience::RetryExecutor::new(retry_config);
    let mut attempt = 0;

    let result: Result<i32, &str> = retry_executor
        .execute(|| {
            attempt += 1;
            async move {
                if attempt < 3 {
                    println!("    尝试 {} 失败", attempt);
                    Err("模拟失败")
                } else {
                    println!("    尝试 {} 成功", attempt);
                    Ok(42)
                }
            }
        })
        .await;

    println!("  最终结果: {:?}", result);
    println!();

    // 3. 熔断器
    println!("[3] 熔断器");
    let cb_config = CircuitBreakerConfig {
        failure_threshold: 3,
        recovery_timeout: Duration::from_secs(2),
        success_threshold: 2,
    };

    let resilient = ResilientExecutor::new(retry_config, cb_config);
    let cb = resilient.circuit_breaker();

    println!("  初始状态: {:?}", cb.state());

    // 模拟失败
    for i in 1..=3 {
        cb.record_failure();
        println!("  失败 {} 次后状态: {:?}", i, cb.state());
    }

    println!("  是否允许请求: {}", cb.allow_request());

    // 等待恢复
    println!("  等待 3 秒...");
    tokio::time::sleep(Duration::from_secs(3)).await;
    println!("  恢复后状态: {:?}", cb.state());
    println!("  是否允许请求: {}", cb.allow_request());
    println!();

    // 4. 限流器
    println!("[4] 限流器");

    // 令牌桶
    println!("  令牌桶限流器 (10 req/s, burst=5):");
    let bucket_config = RateLimitConfig {
        rate: 10,
        burst: 5,
    };
    let bucket = RateLimiterWithStats::new(RateLimiter::token_bucket(bucket_config));

    for i in 1..=7 {
        let result = bucket.try_acquire();
        println!("    请求 {}: {}", i, if result.allowed { "✓ 允许" } else { "✗ 拒绝" });
    }

    let stats = bucket.stats();
    println!("  统计: 总请求={}, 拒绝={}, 拒绝率={:.1}%",
             stats.total_requests, stats.rejected_requests, stats.rejection_rate * 100.0);
    println!();

    // 5. 综合示例：带指标和限流的传输层
    println!("[5] 综合示例");
    let transport = Arc::new(InMemoryTransport::new());
    let metrics = Arc::new(NetworkMetrics::new());

    // 注册 agent
    let mut rx_alice = transport.subscribe(&"alice".to_string()).await.unwrap();
    let mut rx_bob = transport.subscribe(&"bob".to_string()).await.unwrap();

    // 发送消息并记录指标
    let start = std::time::Instant::now();
    transport
        .send(
            &"bob".to_string(),
            Message::Task {
                from: "alice".to_string(),
                task_id: "task-001".to_string(),
                payload: serde_json::json!({"action": "test"}),
            },
        )
        .await
        .unwrap();
    let latency = start.elapsed();

    metrics.record_send("alice", ergatai::network::metrics::MessageType::PointToPoint, latency);
    metrics.record_receive("bob");

    // 接收消息
    let msg = rx_bob.recv().await.unwrap();
    println!("  Bob 收到消息: {:?}", msg.msg);

    // 查看指标
    let snapshot = metrics.snapshot();
    println!("  指标快照:");
    println!("    发送: {}", snapshot.total_sent);
    println!("    接收: {}", snapshot.total_received);
    println!("    延迟: {:.2}ms", snapshot.avg_latency_ms);
    println!();

    println!("=== Demo 完成 ===");
}

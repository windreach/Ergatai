// Test compilation of network infrastructure modules

use std::time::Duration;

// Test metrics
fn test_metrics() {
    use ergatai::network::metrics::{NetworkMetrics, MessageType};

    let metrics = NetworkMetrics::new();
    metrics.record_send("agent_a", MessageType::PointToPoint, Duration::from_millis(10));
    metrics.record_receive("agent_a");

    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.total_sent, 1);
    assert_eq!(snapshot.total_received, 1);
}

// Test resilience
fn test_resilience() {
    use ergatai::network::resilience::{RetryConfig, RetryExecutor, CircuitBreakerConfig, CircuitBreaker};

    let retry_config = RetryConfig::default();
    let _executor = RetryExecutor::new(retry_config);

    let cb_config = CircuitBreakerConfig::default();
    let _cb = CircuitBreaker::new(cb_config);
}

// Test rate limiting
fn test_rate_limit() {
    use ergatai::network::rate_limit::{RateLimiter, RateLimitConfig};

    let config = RateLimitConfig::default();
    let _limiter = RateLimiter::token_bucket(config);
}

fn main() {
    test_metrics();
    test_resilience();
    test_rate_limit();
    println!("All network infrastructure modules compiled successfully!");
}

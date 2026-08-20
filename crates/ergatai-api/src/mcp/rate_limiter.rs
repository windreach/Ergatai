//! Per-agent rate limiter for send_message.
//! Uses a sliding-window counter keyed by runtime agent ID.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const DEFAULT_MESSAGES_PER_MINUTE: u64 = 60;
const WINDOW: Duration = Duration::from_secs(60);

struct AgentWindow {
    timestamps: Vec<Instant>,
}

pub struct AgentRateLimiter {
    windows: Mutex<HashMap<String, AgentWindow>>,
    limit_per_minute: u64,
}

static RATE_LIMITER: OnceLock<AgentRateLimiter> = OnceLock::new();

pub fn get_rate_limiter() -> &'static AgentRateLimiter {
    RATE_LIMITER.get_or_init(|| AgentRateLimiter {
        windows: Mutex::new(HashMap::new()),
        limit_per_minute: DEFAULT_MESSAGES_PER_MINUTE,
    })
}

#[derive(Debug)]
pub struct RateLimitError {
    pub agent_id: String,
    pub used: u64,
    pub limit: u64,
}

impl std::fmt::Display for RateLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "rate limit exceeded for agent {}: {}/{} per minute",
            self.agent_id, self.used, self.limit
        )
    }
}

impl std::error::Error for RateLimitError {}

impl AgentRateLimiter {
    /// Returns Ok(()) if the agent is under the limit, or Err with the
    /// number of messages sent in the current window and the limit.
    pub async fn check(&self, agent_id: &str) -> Result<(), RateLimitError> {
        let mut windows = self.windows.lock().await;
        let window = windows.entry(agent_id.to_string()).or_insert_with(|| {
            AgentWindow { timestamps: Vec::new() }
        });
        let now = Instant::now();
        window.timestamps.retain(|t| now.duration_since(*t) < WINDOW);
        if window.timestamps.len() as u64 >= self.limit_per_minute {
            Err(RateLimitError {
                agent_id: agent_id.to_string(),
                used: window.timestamps.len() as u64,
                limit: self.limit_per_minute,
            })
        } else {
            Ok(())
        }
    }

    /// Record a message send. Must be called after check() succeeds.
    pub async fn record(&self, agent_id: &str) {
        let mut windows = self.windows.lock().await;
        let window = windows.entry(agent_id.to_string()).or_insert_with(|| {
            AgentWindow { timestamps: Vec::new() }
        });
        window.timestamps.push(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rate_limiter_allows_under_limit() {
        let rl = AgentRateLimiter {
            windows: Mutex::new(HashMap::new()),
            limit_per_minute: 3,
        };
        for _ in 0..3 {
            rl.check("a").await.unwrap();
            rl.record("a").await;
        }
        assert!(rl.check("a").await.is_err());
    }

    #[tokio::test]
    async fn rate_limiter_is_per_agent() {
        let rl = AgentRateLimiter {
            windows: Mutex::new(HashMap::new()),
            limit_per_minute: 1,
        };
        rl.check("a").await.unwrap();
        rl.record("a").await;
        assert!(rl.check("a").await.is_err());
        rl.check("b").await.unwrap();  // different agent, unaffected
    }
}

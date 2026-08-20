//! Conversation management — AutoGen-style loop prevention for agent-to-agent messaging.
//!
//! ## Overview
//!
//! Enforces **one-question-one-answer** (一问一答) between agent pairs:
//! - A→B counts as turn 1 (question), B→A counts as turn 2 (answer)
//! - When `max_turns` is reached (default: 2), the conversation **auto-restarts**:
//!   turn counter resets to 0 and state returns to Active
//! - Agents can also end a conversation early with the `TERMINATE` keyword
//!
//! ## Example
//!
//! ```ignore
//! let config = ConversationConfig {
//!     max_turns: 2,              // 一问一答
//!     max_consecutive_auto_reply: 5,
//!     max_execution_time: Duration::from_secs(300),
//! };
//!
//! let manager = ConversationManager::new(config);
//!
//! // Turn 1: A→B (question)
//! manager.check_and_record("agent_a", "agent_b", "Hello").await?;
//!
//! // Turn 2: B→A (answer) — reaches max_turns, auto-restarts
//! manager.check_and_record("agent_b", "agent_a", "Hi there").await?;
//!
//! // Turn 1 (new cycle): A→B — allowed because conversation auto-restarted
//! manager.check_and_record("agent_a", "agent_b", "New topic").await?;
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use ergatai_error::{ErgataiError, ErgataiResult};

/// Maximum number of consecutive cycles the initiator can send without
/// receiving a response from the non-initiator. After this limit, the
/// Who holds the conversation token.
///
/// The token model enforces **strict turn-taking**: only the token holder
/// can send a message. After sending, the token transfers to the other party.
/// TERMINATE releases the token (both parties can start a new cycle).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TokenOwner {
    /// No one holds the token — either party can send (start a new cycle).
    #[default]
    Free,
    /// A specific agent holds the token and is the only one who can send.
    Held(String),
}

/// Conversation configuration — controls loop prevention thresholds.
///
/// Default configuration enforces **one-question-one-answer** (一问一答):
/// `max_turns = 2` means A→B (question) + B→A (answer), then auto-restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationConfig {
    /// Maximum total turns before auto-restart.
    /// Default: 2 (一问一答 — A→B + B→A, then conversation resets)
    pub max_turns: u32,

    /// Maximum consecutive auto-replies from the same agent.
    /// Prevents A→A→A chains (agent sending multiple messages in a row).
    /// Default: 5
    pub max_consecutive_auto_reply: u32,

    /// Maximum conversation duration before automatic termination.
    /// Default: 5 minutes (in seconds)
    pub max_execution_time_secs: u64,
}

impl Default for ConversationConfig {
    fn default() -> Self {
        Self {
            // 一问一答: A→B (turn 1) + B→A (turn 2) = one question + one answer.
            // After reaching max_turns, the conversation auto-restarts (turn_count resets).
            max_turns: 2,
            max_consecutive_auto_reply: 5,
            max_execution_time_secs: 300,
        }
    }
}

/// Conversation state — tracks lifecycle of an agent-to-agent dialogue.
///
/// ## Directional model (一问一答)
///
/// Conversations follow a strict command-response pattern:
/// 1. **Initiator** sends a message (command) → `awaiting_reply = true`
/// 2. **Non-initiator** replies (response) → `awaiting_reply = false`, cycle complete
/// 3. If the **initiator** sends again → treated as a NEW cycle (auto-restart)
/// 4. If the **non-initiator** sends when not awaiting reply → BLOCKED (no unsolicited messages)
///
/// This models the power asymmetry of terminal injection: the sender commands,
/// the receiver executes and reports back. The receiver cannot initiate conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    /// Unique conversation ID
    pub id: String,

    /// Participants (agent_a, agent_b) — sorted alphabetically for consistency
    pub participants: (String, String),

    /// Current state
    pub state: ConversationState,

    /// Total turn count (each message = 1 turn)
    pub turn_count: u32,

    /// Consecutive auto-reply count per agent.
    /// Resets when the other agent sends a message.
    pub consecutive_auto_replies: HashMap<String, u32>,

    /// Who holds the conversation token.
    ///
    /// ## Token model (会话对齐)
    ///
    /// Each conversation has a single **token** that enforces strict turn-taking:
    /// - `Free`: no one holds the token — either party can start a new cycle
    /// - `Held(agent)`: only that agent can send
    ///
    /// After sending, the token **transfers** to the other party (alternating turns).
    /// TERMINATE **releases** the token (Free) — either party can start a new cycle.
    ///
    /// This replaces the older directional model (initiator + awaiting_reply) with
    /// a simpler symmetric mechanism: only the token holder can speak.
    pub token_owner: TokenOwner,

    /// When the conversation started
    pub started_at: DateTime<Utc>,

    /// Last activity timestamp
    pub last_activity: DateTime<Utc>,

    /// Total message count
    pub message_count: u32,
}

/// Conversation lifecycle states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConversationState {
    /// Conversation is active and accepting messages
    Active,

    /// Conversation terminated (reason describes why).
    Terminated {
        /// Why the conversation was terminated
        reason: TerminationReason,
    },
}

/// Why a conversation was terminated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminationReason {
    /// Completed normally (TERMINATE keyword or max turns/auto-replies reached).
    Completed,
    /// Failed due to error.
    Failed,
    /// Timed out (max execution time exceeded).
    TimedOut,
    /// Manually canceled.
    Canceled,
}

impl Conversation {
    /// Create a new conversation between two agents.
    pub fn new(agent_a: &str, agent_b: &str) -> Self {
        // Sort participants alphabetically for consistent ID generation
        let (a, b) = if agent_a < agent_b {
            (agent_a.to_string(), agent_b.to_string())
        } else {
            (agent_b.to_string(), agent_a.to_string())
        };

        let id = format!("conv-{}-{}", a, b);

        Self {
            id,
            participants: (a, b),
            state: ConversationState::Active,
            turn_count: 0,
            consecutive_auto_replies: HashMap::new(),
            token_owner: TokenOwner::Free,
            started_at: Utc::now(),
            last_activity: Utc::now(),
            message_count: 0,
        }
    }

    /// Check if the conversation is in a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(self.state, ConversationState::Terminated { .. })
    }

    /// Get the other participant given one participant.
    pub fn other_participant(&self, agent: &str) -> Option<&str> {
        if agent == self.participants.0 {
            Some(&self.participants.1)
        } else if agent == self.participants.1 {
            Some(&self.participants.0)
        } else {
            None
        }
    }
}

/// Manages conversations and enforces loop prevention rules.
pub struct ConversationManager {
    config: ConversationConfig,
    conversations: Arc<RwLock<HashMap<String, Conversation>>>,
}

impl ConversationManager {
    /// Create a new conversation manager with the given configuration.
    pub fn new(config: ConversationConfig) -> Self {
        Self {
            config,
            conversations: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Check if a message is allowed and record it.
    ///
    /// Enforces **token-based turn-taking** (会话对齐):
    /// - Each conversation has a single token that alternates between parties.
    /// - `Free`: either party can send (start a new cycle).
    /// - `Held(agent)`: only that agent can send.
    /// - After sending (without TERMINATE), the token transfers to the other party.
    /// - TERMINATE releases the token (Free) — either party can start a new cycle.
    ///
    /// # Arguments
    /// * `from` — Sender agent ID
    /// * `to` — Target agent ID
    /// * `message` — Message content (checked for TERMINATE keyword)
    pub async fn check_and_record(&self, from: &str, to: &str, message: &str) -> ErgataiResult<()> {
        let conv_id = self.conversation_id(from, to);

        // Get or create conversation
        let mut conversations = self.conversations.write().await;
        let conv = conversations
            .entry(conv_id.clone())
            .or_insert_with(|| Conversation::new(from, to));

        // ── Terminal state check ──
        if conv.is_terminal() {
            warn!(
                conv_id = %conv.id,
                state = ?conv.state,
                "Conversation already terminated"
            );
            return Err(ErgataiError::internal(format!(
                "Conversation {} already terminated (state: {:?}). Start a new conversation.",
                conv.id, conv.state
            )));
        }

        // ── Timeout check ──
        let elapsed = Utc::now()
            .signed_duration_since(conv.started_at)
            .num_seconds()
            .unsigned_abs();
        if elapsed > self.config.max_execution_time_secs {
            warn!(
                conv_id = %conv.id,
                elapsed_secs = elapsed,
                max_secs = self.config.max_execution_time_secs,
                "Conversation timeout"
            );
            conv.state = ConversationState::Terminated {
                reason: TerminationReason::TimedOut,
            };
            return Err(ErgataiError::internal(format!(
                "Conversation {} exceeded max execution time ({}s). Start a new conversation.",
                conv.id, self.config.max_execution_time_secs
            )));
        }

        // ── Consecutive auto-reply check (same agent spamming when token is Free) ──
        // With the token model, same-agent consecutive sends only happen when the
        // token is repeatedly released (via TERMINATE) and re-claimed by the same agent.
        let auto_reply_count = conv
            .consecutive_auto_replies
            .get(from)
            .copied()
            .unwrap_or(0);
        if auto_reply_count >= self.config.max_consecutive_auto_reply {
            warn!(
                conv_id = %conv.id,
                from = from,
                auto_reply_count = auto_reply_count,
                max = self.config.max_consecutive_auto_reply,
                "Max consecutive auto-replies reached"
            );
            conv.state = ConversationState::Terminated {
                reason: TerminationReason::Completed,
            };
            return Err(ErgataiError::internal(format!(
                "Agent {} exceeded max consecutive auto-replies ({}). Conversation {} terminated.",
                from, self.config.max_consecutive_auto_reply, conv.id
            )));
        }

        // ── Token check (会话对齐 enforcement) ──
        let has_terminate = message.contains("TERMINATE");
        match &conv.token_owner {
            TokenOwner::Free => {
                // Either party can claim the token by sending.
                // After sending: token transfers to the other party (normal),
                // or releases back to Free (if TERMINATE).
                debug!(
                    conv_id = %conv.id,
                    from = from,
                    "Token free — {} claims and sends",
                    from
                );
            }

            TokenOwner::Held(holder) if holder == from => {
                // Token holder sends — allowed.
            }

            TokenOwner::Held(holder) => {
                // Non-holder trying to send → BLOCKED
                warn!(
                    conv_id = %conv.id,
                    from = from,
                    holder = %holder,
                    "Token held by other agent — message blocked"
                );
                return Err(ErgataiError::internal(format!(
                    "Agent '{}' cannot send: token is held by '{}'. Wait for your turn.",
                    from, holder
                )));
            }
        }

        // ── Token transfer / release ──
        if has_terminate {
            // TERMINATE releases the token — either party can start a new cycle.
            info!(
                conv_id = %conv.id,
                from = from,
                "TERMINATE detected — releasing token (会话 cycle complete)"
            );
            conv.token_owner = TokenOwner::Free;
        } else {
            // Normal send: token transfers to the other party.
            let other = conv.other_participant(from).map(|s| s.to_string());
            if let Some(other_id) = other {
                debug!(
                    conv_id = %conv.id,
                    from = from,
                    next_holder = %other_id,
                    "Token transferred"
                );
                conv.token_owner = TokenOwner::Held(other_id);
            }
        }

        // ── Record the message ──
        conv.turn_count += 1;
        conv.message_count += 1;
        conv.last_activity = Utc::now();

        // Update consecutive auto-reply counters
        *conv
            .consecutive_auto_replies
            .entry(from.to_string())
            .or_insert(0) += 1;

        // Reset the other agent's counter (they're no longer "auto-replying")
        let other = conv.other_participant(from).map(|s| s.to_string());
        if let Some(other_id) = other {
            if let Some(count) = conv.consecutive_auto_replies.get_mut(&other_id) {
                *count = 0;
            }
        }

        debug!(
            conv_id = %conv.id,
            from = from,
            to = to,
            turn_count = conv.turn_count,
            token_owner = ?conv.token_owner,
            "Message recorded in conversation"
        );

        Ok(())
    }

    /// Get conversation ID for a pair of agents (sorted alphabetically).
    fn conversation_id(&self, agent_a: &str, agent_b: &str) -> String {
        let (a, b) = if agent_a < agent_b {
            (agent_a, agent_b)
        } else {
            (agent_b, agent_a)
        };
        format!("conv-{}-{}", a, b)
    }

    /// Get a conversation by ID.
    #[allow(dead_code)]
    pub async fn get_conversation(&self, conv_id: &str) -> Option<Conversation> {
        let conversations = self.conversations.read().await;
        conversations.get(conv_id).cloned()
    }

    /// List all active conversations.
    #[allow(dead_code)]
    pub async fn list_active_conversations(&self) -> Vec<Conversation> {
        let conversations = self.conversations.read().await;
        conversations
            .values()
            .filter(|c| c.state == ConversationState::Active)
            .cloned()
            .collect()
    }

    /// List all conversations (for debugging).
    #[allow(dead_code)]
    pub async fn list_all_conversations(&self) -> Vec<Conversation> {
        let conversations = self.conversations.read().await;
        conversations.values().cloned().collect()
    }

    /// Number of tracked conversations (for diagnostics / reaper logging).
    pub async fn len(&self) -> usize {
        self.conversations.read().await.len()
    }

    /// Clean up old conversations (older than `max_age`).
    pub async fn cleanup_old_conversations(&self, max_age: Duration) {
        let mut conversations = self.conversations.write().await;
        let now = Utc::now();

        conversations.retain(|id, conv| {
            let age = now
                .signed_duration_since(conv.last_activity)
                .num_seconds()
                .unsigned_abs();
            let keep = age < max_age.as_secs();
            if !keep {
                info!(conv_id = %id, age_secs = age, "Cleaning up old conversation");
            }
            keep
        });
    }

    /// Manually terminate a conversation.
    #[allow(dead_code)]
    pub async fn terminate_conversation(
        &self,
        conv_id: &str,
        reason: TerminationReason,
    ) -> ErgataiResult<()> {
        let mut conversations = self.conversations.write().await;

        if let Some(conv) = conversations.get_mut(conv_id) {
            if conv.is_terminal() {
                return Err(ErgataiError::internal(format!(
                    "Conversation {} already terminated",
                    conv_id
                )));
            }
            conv.state = ConversationState::Terminated { reason };
            info!(conv_id = %conv_id, reason = ?reason, "Conversation terminated");
            Ok(())
        } else {
            Err(ErgataiError::internal(format!(
                "Conversation {} not found",
                conv_id
            )))
        }
    }
}

/// Default maximum age for a conversation before cleanup (1 hour).
///
/// Conversations inactive for longer than this are swept by the reaper.
/// Active conversations with recent activity are retained regardless of state.
const DEFAULT_CONVERSATION_MAX_AGE_SECS: u64 = 3600;

/// Default interval for the conversation reaper sweep (5 minutes).
const CONVERSATION_REAPER_INTERVAL_SECS: u64 = 300;

/// Start a background task that periodically sweeps stale conversations.
///
/// Mirrors the peer reaper pattern: runs on a fixed interval, calls
/// `cleanup_old_conversations` with the configured `max_age`, and stops
/// cleanly when the `CancellationToken` fires.
///
/// This prevents unbounded memory growth in long-running deployments:
/// every MCP session's conversation state would otherwise accumulate
/// forever in the `ConversationManager`'s `RwLock<HashMap>`.
pub fn start_conversation_reaper(
    manager: Arc<ConversationManager>,
    cancellation_token: CancellationToken,
) {
    tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(Duration::from_secs(CONVERSATION_REAPER_INTERVAL_SECS));
        // First tick fires immediately — skip it so we don't sweep before any
        // conversations have a chance to be created.
        interval.tick().await;

        let max_age = Duration::from_secs(DEFAULT_CONVERSATION_MAX_AGE_SECS);

        loop {
            tokio::select! {
                _ = cancellation_token.cancelled() => {
                    info!("Conversation reaper shutting down");
                    break;
                }
                _ = interval.tick() => {
                    let before = manager.len().await;
                    manager.cleanup_old_conversations(max_age).await;
                    let after = manager.len().await;
                    if before != after {
                        info!(
                            swept = before - after,
                            remaining = after,
                            max_age_secs = DEFAULT_CONVERSATION_MAX_AGE_SECS,
                            "Conversation reaper swept stale conversations"
                        );
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ConversationConfig::default();
        assert_eq!(config.max_turns, 2); // 一问一答
        assert_eq!(config.max_consecutive_auto_reply, 5);
        assert_eq!(config.max_execution_time_secs, 300);
    }

    #[test]
    fn test_conversation_creation() {
        let conv = Conversation::new("agent_a", "agent_b");
        assert_eq!(conv.participants.0, "agent_a");
        assert_eq!(conv.participants.1, "agent_b");
        assert_eq!(conv.state, ConversationState::Active);
        assert_eq!(conv.turn_count, 0);
        assert_eq!(conv.token_owner, TokenOwner::Free);
        assert!(conv.consecutive_auto_replies.is_empty());
    }

    #[test]
    fn test_conversation_participants_sorted() {
        // Participants should be sorted alphabetically
        let conv1 = Conversation::new("agent_b", "agent_a");
        let conv2 = Conversation::new("agent_a", "agent_b");

        assert_eq!(conv1.participants, conv2.participants);
        assert_eq!(conv1.id, conv2.id);
    }

    #[test]
    fn test_other_participant() {
        let conv = Conversation::new("agent_a", "agent_b");
        assert_eq!(conv.other_participant("agent_a"), Some("agent_b"));
        assert_eq!(conv.other_participant("agent_b"), Some("agent_a"));
        assert_eq!(conv.other_participant("agent_c"), None);
    }

    #[test]
    fn test_terminal_states() {
        let mut conv = Conversation::new("a", "b");
        assert!(!conv.is_terminal());

        conv.state = ConversationState::Terminated {
            reason: TerminationReason::Completed,
        };
        assert!(conv.is_terminal());

        conv.state = ConversationState::Terminated {
            reason: TerminationReason::Failed,
        };
        assert!(conv.is_terminal());

        conv.state = ConversationState::Terminated {
            reason: TerminationReason::TimedOut,
        };
        assert!(conv.is_terminal());

        conv.state = ConversationState::Terminated {
            reason: TerminationReason::Canceled,
        };
        assert!(conv.is_terminal());
    }

    #[tokio::test]
    async fn test_conversation_manager_basic_flow() {
        let config = ConversationConfig {
            max_turns: 10,
            max_consecutive_auto_reply: 5,
            max_execution_time_secs: 300,
        };
        let manager = ConversationManager::new(config);

        // A sends to B — should succeed
        let result = manager
            .check_and_record("agent_a", "agent_b", "Hello")
            .await;
        assert!(result.is_ok());

        // B replies to A — should succeed
        let result = manager.check_and_record("agent_b", "agent_a", "Hi").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_token_transfer_basic() {
        // Token alternates: A sends → token to B, B sends → token to A
        let config = ConversationConfig::default();
        let manager = ConversationManager::new(config);

        // Initially: token is Free
        let conv = manager.get_conversation("conv-agent_a-agent_b").await;
        assert!(conv.is_none()); // Not yet created

        // A sends — token transfers to B
        manager
            .check_and_record("agent_a", "agent_b", "Hello")
            .await
            .unwrap();
        let conv = manager
            .get_conversation("conv-agent_a-agent_b")
            .await
            .unwrap();
        assert_eq!(conv.token_owner, TokenOwner::Held("agent_b".to_string()));

        // B sends — token transfers back to A
        manager
            .check_and_record("agent_b", "agent_a", "Hi")
            .await
            .unwrap();
        let conv = manager
            .get_conversation("conv-agent_a-agent_b")
            .await
            .unwrap();
        assert_eq!(conv.token_owner, TokenOwner::Held("agent_a".to_string()));
    }

    #[tokio::test]
    async fn test_token_holder_check() {
        // Only the token holder can send. Non-holder is BLOCKED.
        let config = ConversationConfig {
            max_consecutive_auto_reply: 100, // disable for this test
            ..ConversationConfig::default()
        };
        let manager = ConversationManager::new(config);

        // A sends — token goes to B
        manager
            .check_and_record("agent_a", "agent_b", "Question")
            .await
            .unwrap();

        // A tries to send again — BLOCKED (token held by B)
        let result = manager
            .check_and_record("agent_a", "agent_b", "Another msg")
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("token is held by"));

        // B sends — allowed (B holds token)
        manager
            .check_and_record("agent_b", "agent_a", "Answer")
            .await
            .unwrap();

        // Now B tries to send again — BLOCKED (token held by A)
        let result = manager.check_and_record("agent_b", "agent_a", "More").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_terminate_releases_token() {
        // TERMINATE releases the token to Free — either party can send next.
        let config = ConversationConfig::default();
        let manager = ConversationManager::new(config);

        // A sends — token to B
        manager
            .check_and_record("agent_a", "agent_b", "Hello")
            .await
            .unwrap();

        // B sends TERMINATE — token released to Free
        manager
            .check_and_record("agent_b", "agent_a", "Done. TERMINATE")
            .await
            .unwrap();
        let conv = manager
            .get_conversation("conv-agent_a-agent_b")
            .await
            .unwrap();
        assert_eq!(conv.token_owner, TokenOwner::Free);

        // Either party can now send (new cycle)
        // A sends — token to B
        manager
            .check_and_record("agent_a", "agent_b", "New topic")
            .await
            .unwrap();
        let conv = manager
            .get_conversation("conv-agent_a-agent_b")
            .await
            .unwrap();
        assert_eq!(conv.token_owner, TokenOwner::Held("agent_b".to_string()));
    }

    #[tokio::test]
    async fn test_terminate_from_either_party() {
        // TERMINATE can be sent by any token holder (not just the "initiator").
        let config = ConversationConfig::default();
        let manager = ConversationManager::new(config);

        // A sends — token to B
        manager
            .check_and_record("agent_a", "agent_b", "msg 1")
            .await
            .unwrap();

        // B sends TERMINATE — token released
        manager
            .check_and_record("agent_b", "agent_a", "reply. TERMINATE")
            .await
            .unwrap();
        let conv = manager
            .get_conversation("conv-agent_a-agent_b")
            .await
            .unwrap();
        assert_eq!(conv.token_owner, TokenOwner::Free);

        // B can also start a new cycle now (token is Free)
        manager
            .check_and_record("agent_b", "agent_a", "B initiates")
            .await
            .unwrap();
        let conv = manager
            .get_conversation("conv-agent_a-agent_b")
            .await
            .unwrap();
        assert_eq!(conv.token_owner, TokenOwner::Held("agent_a".to_string()));
    }

    #[tokio::test]
    async fn test_consecutive_auto_reply_with_terminate() {
        // When the same agent repeatedly sends TERMINATE to release the token
        // and re-claims it, consecutive_auto_reply catches the spam.
        let config = ConversationConfig {
            max_consecutive_auto_reply: 3,
            ..ConversationConfig::default()
        };
        let manager = ConversationManager::new(config);

        // A sends TERMINATE (token released)
        manager
            .check_and_record("agent_a", "agent_b", "msg 1. TERMINATE")
            .await
            .unwrap();
        // A sends again (token is Free, A re-claims) — TERMINATE again
        manager
            .check_and_record("agent_a", "agent_b", "msg 2. TERMINATE")
            .await
            .unwrap();
        // A sends again — 3rd consecutive
        manager
            .check_and_record("agent_a", "agent_b", "msg 3. TERMINATE")
            .await
            .unwrap();

        // 4th consecutive from A — BLOCKED by consecutive_auto_reply
        let result = manager
            .check_and_record("agent_a", "agent_b", "msg 4. TERMINATE")
            .await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("consecutive auto-replies"));
    }

    #[tokio::test]
    async fn test_token_prevents_one_sided_spam() {
        // Without TERMINATE, the token model itself prevents one-sided spam.
        // A sends → token to B → A is blocked until B replies.
        let config = ConversationConfig {
            max_consecutive_auto_reply: 100, // disable to test token alone
            ..ConversationConfig::default()
        };
        let manager = ConversationManager::new(config);

        // A sends 1 message
        manager
            .check_and_record("agent_a", "agent_b", "Question")
            .await
            .unwrap();

        // A tries to send 99 more times — ALL BLOCKED (token held by B)
        for i in 2..=100 {
            let result = manager
                .check_and_record("agent_a", "agent_b", &format!("spam {}", i))
                .await;
            assert!(
                result.is_err(),
                "A's send #{} should be blocked (token held by B)",
                i
            );
        }

        // B replies — token transfers to A
        manager
            .check_and_record("agent_b", "agent_a", "Answer")
            .await
            .unwrap();

        // Now A can send again
        let result = manager
            .check_and_record("agent_a", "agent_b", "New question")
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_list_active_conversations() {
        let config = ConversationConfig::default();
        let manager = ConversationManager::new(config);

        // Create 2 conversations
        manager
            .check_and_record("agent_a", "agent_b", "Hello")
            .await
            .unwrap();
        manager
            .check_and_record("agent_c", "agent_d", "Hi")
            .await
            .unwrap();

        let active = manager.list_active_conversations().await;
        assert_eq!(active.len(), 2);

        // Terminate one
        manager
            .terminate_conversation("conv-agent_a-agent_b", TerminationReason::Completed)
            .await
            .unwrap();

        let active = manager.list_active_conversations().await;
        assert_eq!(active.len(), 1);
    }
}

//! MCP Server implementation using rmcp (Rust MCP SDK)
//!
//! Implements MCP protocol 2025-06-18 with Streamable HTTP transport.
//! Agents connect via POST/GET /mcp and can call tools like list_agents,
//! send_message, submit_orchestration, etc.

use std::collections::HashMap;
use std::sync::Arc;

use rmcp::{elicit_safe, service::ElicitationError};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, InitializeRequestParams, InitializeResult,
        ServerCapabilities, ServerInfo,
    },
    service::{Peer, RequestContext},
    tool, tool_handler, tool_router, ErrorData, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use ergatai_core::agent_registry::AgentRegistry;
use ergatai_runtime::get_agent_runtime;
use std::time::{SystemTime, UNIX_EPOCH};

use super::conversation::{ConversationManager, TokenOwner};

/// Shared registry of MCP peer handles for pushing notifications to agents.
/// Key: agent_id (e.g., "opencode@abcd1234")
/// Value: Peer handle for sending notifications to that agent's MCP session.
pub type PeerRegistry = Arc<RwLock<HashMap<String, Peer<RoleServer>>>>;

/// Create a new empty PeerRegistry.
pub fn new_peer_registry() -> PeerRegistry {
    Arc::new(RwLock::new(HashMap::new()))
}

/// MCP Server state - shared across all sessions via Arc
#[derive(Clone)]
pub struct ErgataiMcpServer {
    tool_router: ToolRouter<Self>,
    registry: Arc<AgentRegistry>,
    /// Shared peer registry for pushing notifications to agents
    peer_registry: PeerRegistry,
    /// Per-session agent ID (set during initialize, used in send_message)
    session_agent_id: Arc<RwLock<Option<String>>>,
    /// Conversation manager for loop prevention (AutoGen-style)
    conversation_manager: Arc<ConversationManager>,
    /// Agent identifier from URL path (e.g., "agent-1", "agent-2")
    /// Used to bind MCP connections to specific rmux panes
    agent_identifier: Option<String>,
}

impl std::fmt::Debug for ErgataiMcpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ErgataiMcpServer").finish_non_exhaustive()
    }
}

impl ErgataiMcpServer {
    /// Create a new server instance (called per-session by the factory)
    pub fn new(
        registry: Arc<AgentRegistry>,
        peer_registry: PeerRegistry,
        conversation_manager: Arc<ConversationManager>,
        agent_identifier: Option<String>,
    ) -> Self {
        Self {
            tool_router: Self::tool_router(),
            registry,
            peer_registry,
            session_agent_id: Arc::new(RwLock::new(None)),
            conversation_manager,
            agent_identifier,
        }
    }
}

/// When the per-session `ErgataiMcpServer` is dropped (session ends — client
/// disconnect, idle timeout, or server shutdown), automatically unregister the
/// agent from the shared registry and remove its peer handle. Without this,
/// dead agents accumulate as zombies because rmcp's `ServerHandler` has no
/// `on_close` callback.
impl Drop for ErgataiMcpServer {
    fn drop(&mut self) {
        // `Drop` is synchronous — use `try_read` (non-blocking) to grab the
        // agent ID, then spawn the async cleanup on the tokio runtime.
        // The session worker task is still on the runtime when it drops us,
        // so `tokio::spawn` is safe here.
        let agent_id = match self.session_agent_id.try_read() {
            Ok(guard) => guard.clone(),
            Err(_) => {
                warn!(
                    "ErgataiMcpServer::drop: session_agent_id lock contended, \
                     skipping unregister (stale-agent reaper will clean up)"
                );
                None
            }
        };

        if let Some(agent_id) = agent_id {
            let registry = self.registry.clone();
            let peer_registry = self.peer_registry.clone();
            info!("MCP session ending, unregistering agent: {}", agent_id);
            tokio::spawn(async move {
                do_unregister_agent(&registry, &peer_registry, &agent_id, "MCP session closed")
                    .await;
            });
        }
    }
}

/// Unregister an agent from the registry and remove its peer handle.
/// Centralized helper used by Drop, peer reaper, and send_message failure handler.
async fn do_unregister_agent(
    registry: &AgentRegistry,
    peer_registry: &PeerRegistry,
    agent_id: &str,
    reason: &str,
) {
    registry.unregister_agent(agent_id).await;
    peer_registry.write().await.remove(agent_id);
    info!("Agent {} unregistered ({})", agent_id, reason);
}

// ── Tool parameter types ──

#[derive(Debug, Deserialize, JsonSchema)]
struct ListAgentsParams {
    /// Whether to include agent capabilities
    #[serde(default)]
    include_capabilities: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SendMessageParams {
    /// ID of the target agent
    target_agent_id: String,
    /// Message content
    message: String,
    /// Type of message (request, response, broadcast)
    #[serde(default)]
    message_type: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RegisterAgentNameParams {
    /// Human-readable display name for this agent (e.g., "frontend-dev", "code-reviewer")
    /// Once set, other agents can send messages to you using this name instead of your auto-generated ID.
    /// Names must be unique across all agents.
    display_name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SubmitOrchestrationParams {
    /// DAG definition in YAML or Markdown format.
    ///
    /// YAML format (recommended):
    /// ```yaml
    /// tasks:
    ///   - name: Task A
    ///     agent: agent-a
    ///     task: tasks/a.md
    ///   - name: Task B
    ///     agent: agent-b
    ///     depends_on: [Task A]
    ///     timeout: 300
    /// ```
    ///
    /// Markdown format (legacy):
    /// ```markdown
    /// ## Task A
    /// - **agent**: agent-a
    /// - **task**: tasks/a.md
    /// ```
    dag_definition: String,
    /// Optional context variables
    #[serde(default)]
    context: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CheckDagStatusParams {
    /// DAG ID to check
    dag_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct GetCollaborationStatusParams {
    /// Optional DAG ID. If omitted, returns the most recently submitted session.
    dag_id: Option<String>,
}

// ── File Access Control parameter types ──

#[derive(Debug, Deserialize, JsonSchema)]
struct RequestFileAccessParams {
    /// File path to access (absolute or relative to project root)
    file_path: String,
    /// Access mode: "READ" or "WRITE"
    mode: String,
    /// Reason for requesting access
    reason: Option<String>,
    /// Glob pattern scope (e.g., "src/**" or specific file)
    #[serde(default = "default_scope")]
    scope: String,
}

fn default_scope() -> String {
    "**".to_string()
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ReleaseFileAccessParams {
    /// File path to release
    file_path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ListActiveLocksParams {
    /// Filter by agent ID (optional)
    agent_id: Option<String>,
}

// ── MCP Elicitation types for user approval ──

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ApprovalResponse {
    /// User's approval decision: "yes" or "no"
    decision: String,
}

elicit_safe!(ApprovalResponse);

// ── Tool implementations ──

#[tool_router]
impl ErgataiMcpServer {
    /// List all connected agents and their status
    #[tool(description = "List all connected agents and their status")]
    async fn list_agents(
        &self,
        params: Parameters<ListAgentsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let _include_capabilities = params.0.include_capabilities.unwrap_or(false);

        // Get runtime agents (discovered via rmux) instead of just MCP agents
        let runtime = get_agent_runtime();
        let runtime_agents = runtime.list_agents().await;

        // Get the calling agent's ID to mark is_self
        let my_agent_id = self.session_agent_id.read().await.clone();

        let agents_json: Vec<serde_json::Value> = runtime_agents
            .iter()
            .map(|info| {
                let is_self = my_agent_id.as_ref().is_some_and(|id| {
                    // Check both runtime ID and MCP ID
                    id == &info.agent_id
                        || info
                            .mcp_agent_id
                            .as_ref()
                            .is_some_and(|mcp_id| mcp_id == id)
                });

                serde_json::json!({
                    "agent_id": info.agent_id,
                    "agent_uuid": info.agent_uuid,
                    "display_name": info.display_name,
                    "mcp_agent_id": info.mcp_agent_id,
                    "workspace_id": info.workspace_id,
                    // Lifecycle state (lowercase) from unified state machine
                    "state": info.lifecycle.state_name(),
                    "lifecycle_state": info.lifecycle.state_name(),
                    "task_id": info.task_id,
                    "is_alive": info.lifecycle.is_alive(),
                    "is_idle": info.lifecycle.is_idle(),
                    "is_processing": info.lifecycle.is_processing(),
                    "status": if info.mcp_agent_id.is_some() { "active" } else { "discovered" },
                    "is_self": is_self,
                    "ergatai_agent_id": info.handle.metadata.get("ergatai_agent_id"),
                    "last_heartbeat": info.last_heartbeat.to_rfc3339(),
                })
            })
            .collect();

        let result = serde_json::json!({
            "agents": agents_json,
            "total": agents_json.len()
        });

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&result).unwrap_or_default(),
        )]))
    }

    /// Register a human-readable display name for this agent.
    ///
    /// Once registered, other agents can send messages to you using this name
    /// instead of the auto-generated ID (e.g., "%198"). Names must be unique.
    /// You can call this multiple times to change your display name.
    #[tool(
        description = "Register a human-readable display name for this agent (enables name-based messaging)"
    )]
    async fn register_agent_name(
        &self,
        params: Parameters<RegisterAgentNameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let display_name = &params.0.display_name;

        // Validate display name format
        if display_name.is_empty() {
            return Ok(CallToolResult::error(vec![ContentBlock::text(
                "Display name cannot be empty",
            )]));
        }
        if display_name.len() > 64 {
            return Ok(CallToolResult::error(vec![ContentBlock::text(
                "Display name must be 64 characters or less",
            )]));
        }
        if !display_name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        {
            return Ok(CallToolResult::error(vec![ContentBlock::text(
                "Display name can only contain letters, numbers, hyphens, and underscores",
            )]));
        }

        // Get the calling agent's runtime ID
        let my_agent_id = match self.session_agent_id.read().await.clone() {
            Some(id) => id,
            None => {
                return Ok(CallToolResult::error(vec![ContentBlock::text(
                    "Cannot register name: agent identity not established",
                )]));
            }
        };

        let runtime = get_agent_runtime();

        // Resolve MCP ID to runtime ID if needed
        let runtime_id = match runtime.resolve_agent_id(&my_agent_id).await {
            Some(id) => id,
            None => {
                return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "Cannot register name: agent '{}' not found in runtime",
                    my_agent_id
                ))]));
            }
        };

        // Set the display name
        match runtime
            .set_display_name(&runtime_id, display_name.clone())
            .await
        {
            Ok(()) => {
                let result = serde_json::json!({
                    "status": "registered",
                    "agent_id": runtime_id,
                    "display_name": display_name,
                    "message": format!(
                        "Display name '{}' registered. Other agents can now send messages to you using this name.",
                        display_name
                    )
                });

                info!(
                    agent_id = %runtime_id,
                    display_name = %display_name,
                    "Agent registered display name"
                );

                Ok(CallToolResult::success(vec![ContentBlock::text(
                    serde_json::to_string_pretty(&result).unwrap_or_default(),
                )]))
            }
            Err(e) => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "Failed to register display name: {}",
                e
            ))])),
        }
    }

    /// Send a message to another agent.
    ///
    /// Delivery order:
    /// 1. **NATS JetStream** (preferred) — message is persisted to `AGENT_MESSAGES` stream,
    ///    then delivered by the background `MessageDeliveryConsumer` via tmux injection.
    ///    Provides durability, retry on failure, and delivery confirmation.
    /// 2. **Direct tmux injection** (fallback) — when NATS is unavailable, falls back to
    ///    direct AgentRuntime injection. No persistence guarantee.
    #[tool(
        description = "Send a message to another agent (NATS JetStream for reliability, tmux fallback)"
    )]
    async fn send_message(
        &self,
        params: Parameters<SendMessageParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let target_agent_id = &params.0.target_agent_id;
        let message = &params.0.message;
        let message_type = params.0.message_type.as_deref().unwrap_or("request");

        info!(
            "Sending message to agent {}: {} (type: {})",
            target_agent_id, message, message_type
        );

        // Find the matching agent - support both exact ID and name prefix
        // Check both MCP registry and AgentRuntime
        let agents = self.registry.list_agents().await;
        let runtime = get_agent_runtime();
        let runtime_agents = runtime.list_agents().await;

        let matching_agent = agents
            .iter()
            .find(|a| {
                // Exact match
                a.agent_id == *target_agent_id
                // Or prefix match (e.g., "simple-agent" matches "simple-agent@ead00fad")
                || a.agent_id.starts_with(&format!("{}@", target_agent_id))
            })
            .map(|a| a.agent_id.clone())
            .or_else(|| {
                // Check runtime agents (by agent_id, task_id, or display_name)
                runtime_agents
                    .iter()
                    .find(|a| {
                        a.agent_id == *target_agent_id
                            || a.task_id.as_deref() == Some(target_agent_id)
                            || a.display_name.as_ref() == Some(target_agent_id)
                    })
                    .map(|a| a.agent_id.clone())
            });

        // Get the sender agent ID
        let from_agent = self
            .session_agent_id
            .read()
            .await
            .clone()
            .unwrap_or_else(|| "unknown-mcp-client".to_string());

        let resolved_agent_id = match matching_agent {
            Some(id) => {
                info!(
                    from_agent = %from_agent,
                    target_agent_id = %target_agent_id,
                    resolved_agent_id = %id,
                    "Message routing: resolved target agent ID"
                );
                id
            }
            None => {
                return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "Agent {} not found. Agent must connect via MCP or be running in tmux.",
                    target_agent_id
                ))]));
            }
        };

        // Server-side safety net: reject self-messages.
        // Both IDs must be resolved to runtime IDs before comparing —
        // from_agent is an MCP ID (e.g. "opencode@abcd") while resolved_agent_id
        // is a runtime ID (e.g. "%312"). Without resolution the check never fires.
        let from_runtime_id = runtime.resolve_agent_id(&from_agent).await;
        if from_runtime_id.as_deref() == Some(&resolved_agent_id) {
            return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "Cannot send message to yourself. Agent '{}' cannot target itself.",
                from_agent
            ))]));
        }

        // ── Collaboration session ACL (MeshPolicy enforcement) ──
        // Try plausible identifier pairs for sender/receiver against every active
        // DAG session. A session only "speaks up" when both endpoints are its
        // participants (Denied or Allowed); otherwise it returns NotApplicable
        // and we keep scanning. First Denied wins; if no scheduler denies, the
        // message is allowed.
        {
            let sender_ids = [
                from_agent.as_str(),
                from_runtime_id.as_deref().unwrap_or(""),
            ];
            let receiver_ids = [target_agent_id.as_str(), resolved_agent_id.as_str()];

            'scheduler_loop: for scheduler in ergatai_core::cross_agent::list_dag_schedulers() {
                for &s in &sender_ids {
                    if s.is_empty() {
                        continue;
                    }
                    for &r in &receiver_ids {
                        if r.is_empty() {
                            continue;
                        }
                        match scheduler.check_communication(s, r).await {
                            ergatai_core::cross_agent::CommunicationCheck::Denied(reason) => {
                                warn!(
                                    from = %s,
                                    to = %r,
                                    reason = %reason,
                                    "MeshPolicy denied message"
                                );
                                return Ok(CallToolResult::error(vec![ContentBlock::text(
                                    format!("Message blocked by collaboration policy: {}", reason),
                                )]));
                            }
                            ergatai_core::cross_agent::CommunicationCheck::Allowed => {
                                // This session covered both endpoints and permits
                                // the pair — skip to next scheduler.
                                continue 'scheduler_loop;
                            }
                            ergatai_core::cross_agent::CommunicationCheck::NotApplicable => {
                                // This session doesn't cover both endpoints; keep
                                // scanning other (sender, receiver) pairs.
                            }
                        }
                    }
                }
            }
        }

        // ── Check if this is a reply BEFORE check_and_record modifies token state ──
        // is_reply_message checks token_owner: if sender holds token, it means recipient sent last.
        // Must be called BEFORE check_and_record which transfers the token.
        let from_runtime_id_for_batch = from_runtime_id
            .clone()
            .unwrap_or_else(|| from_agent.clone());
        let is_reply = self
            .is_reply_message(&from_runtime_id_for_batch, &resolved_agent_id)
            .await;

        info!(
            from_agent = %from_agent,
            from_runtime_id = ?from_runtime_id,
            from_runtime_id_for_batch = %from_runtime_id_for_batch,
            resolved_agent_id = %resolved_agent_id,
            is_reply = is_reply,
            "is_reply_message check"
        );

        // ── Conversation loop prevention (AutoGen-style) ──
        // Use runtime IDs for consistency (from_runtime_id_for_batch and resolved_agent_id are both runtime IDs).
        // Check max_turns, max_consecutive_auto_reply, max_execution_time, and TERMINATE keyword.
        if let Err(e) = self
            .conversation_manager
            .check_and_record(&from_runtime_id_for_batch, &resolved_agent_id, message)
            .await
        {
            warn!(
                from = %from_agent,
                to = %resolved_agent_id,
                error = %e,
                "Conversation loop prevention blocked message"
            );
            return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "Message blocked by conversation loop prevention: {}",
                e
            ))]));
        }

        // ── Batch aggregator: record send for group message detection ──
        // Track this send to detect when agent sends to multiple targets in quick succession.
        // Skip recording if this is a reply (to avoid confusing batch detection).
        let batch_id = super::get_batch_aggregator()
            .record_send(&from_runtime_id_for_batch, &resolved_agent_id, is_reply)
            .await;

        if let Some(ref bid) = batch_id {
            info!(
                from = %from_agent,
                to = %resolved_agent_id,
                batch_id = %bid,
                "Message is part of a batch"
            );
        }

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // ── Primary path: publish to NATS JetStream (reliable) ──
        if let Some(conn) = ergatai_nats::get_nats_connection().await {
            // Resolve sender MCP ID → runtime ID for the reply target
            let sender_runtime_id = runtime
                .resolve_agent_id(&from_agent)
                .await
                .unwrap_or_else(|| from_agent.clone());

            // Get human-readable display name (e.g., "frontend-dev (%72)")
            let sender_display = Self::get_agent_display_name(&runtime, &sender_runtime_id).await;

            // Format message with contextual hint based on message type
            let formatted_content = Self::format_agent_message(&sender_display, message, is_reply);

            let bus = ergatai_nats::EventBus::new(conn);
            let mut metadata = std::collections::HashMap::new();
            if let Some(ref bid) = batch_id {
                metadata.insert("batch_id".to_string(), bid.clone());
            }

            // Get agent UUIDs for stable routing
            let from_uuid = runtime
                .get_agent(&from_runtime_id_for_batch)
                .await
                .map(|info| info.agent_uuid);
            let to_uuid = runtime
                .get_agent(&resolved_agent_id)
                .await
                .map(|info| info.agent_uuid);

            let payload = ergatai_nats::AgentMessagePayload {
                from_agent: from_agent.clone(),
                to_agent: resolved_agent_id.clone(),
                from_uuid,
                to_uuid,
                content: formatted_content,
                thread_id: None,
                timestamp,
                metadata,
            };

            match bus.publish_agent_message_reliable(&payload).await {
                Ok(ack) => {
                    let response_json = serde_json::json!({
                        "status": "queued",
                        "target_agent": resolved_agent_id,
                        "delivery_method": "nats_jetstream",
                        "stream": ack.stream,
                        "sequence": ack.sequence,
                        "note": "Message persisted to NATS JetStream. Background consumer will deliver via tmux injection."
                    });

                    return Ok(CallToolResult::success(vec![ContentBlock::text(
                        serde_json::to_string_pretty(&response_json).unwrap_or_default(),
                    )]));
                }
                Err(e) => {
                    warn!(
                        "NATS JetStream publish failed (falling back to direct delivery): {}",
                        e
                    );
                    // Fall through to direct delivery
                }
            }
        }

        // ── Fallback: direct tmux injection (no persistence) ──
        // is_reply was already computed before check_and_record
        match self
            .try_tmux_injection(&resolved_agent_id, &from_agent, message, is_reply)
            .await
        {
            Ok(result) => Ok(result),
            Err(e) => {
                // Both NATS and direct injection failed — return error to caller.
                // Caller can retry; NATS publish will be attempted again.
                Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "Failed to deliver message to {}: NATS publish failed and direct injection error: {}",
                    resolved_agent_id, e
                ))]))
            }
        }
    }

    /// Submit a DAG workflow for multi-agent collaboration
    #[tool(
        description = "Submit a DAG workflow for multi-agent collaboration. Accepts YAML format (recommended) or legacy Markdown format. The system auto-detects the format."
    )]
    async fn submit_orchestration(
        &self,
        params: Parameters<SubmitOrchestrationParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let dag_definition = &params.0.dag_definition;
        let context_value = &params.0.context;

        info!(
            "Submitting DAG orchestration ({} bytes)",
            dag_definition.len()
        );

        // Check if a DAG is already running
        if let Some(existing) = ergatai_core::cross_agent::get_dag_scheduler() {
            if !existing.is_complete().await {
                return Err(ErrorData::internal_error(
                    "A DAG is already running. Wait for it to complete or check its status.",
                    None,
                ));
            }
        }

        // Parse DAG definition (YAML or Markdown) → TaskGraph
        let graph =
            ergatai_core::orchestration::parse_dag_auto(dag_definition, None).map_err(|e| {
                ErrorData::invalid_params(format!("Failed to parse DAG definition: {}", e), None)
            })?;

        // Build DagContext from optional context parameter
        let mut dag_context = ergatai_core::orchestration::DagContext::empty();
        if let Some(ctx_val) = context_value {
            if let Some(vars) = ctx_val.as_object() {
                for (k, v) in vars {
                    dag_context.set_global(k.clone(), v.as_str().unwrap_or_default().to_string());
                }
            }
        }

        // Create DagScheduler
        let project_root = std::env::current_dir().map_err(|e| {
            ErrorData::internal_error(format!("Failed to get current directory: {}", e), None)
        })?;
        let scheduler =
            ergatai_core::cross_agent::DagScheduler::with_context(project_root, graph, dag_context);

        // Register globally + start NATS event listener
        ergatai_core::cross_agent::set_dag_scheduler(scheduler.clone());
        scheduler.clone().start_event_listener();

        // Submit the graph (dispatches ready nodes)
        let submitted = scheduler
            .submit_graph()
            .await
            .map_err(|e| ErrorData::internal_error(format!("Failed to submit DAG: {}", e), None))?;

        let progress = scheduler.progress().await;
        let status = scheduler.status_prompt().await;

        let result = serde_json::json!({
            "status": "submitted",
            "submitted_nodes": submitted.len(),
            "progress": progress,
            "graph_status": status,
        });

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&result).unwrap_or_default(),
        )]))
    }

    /// Check the status of a DAG execution
    #[tool(description = "Check the status of a DAG execution")]
    async fn check_dag_status(
        &self,
        params: Parameters<CheckDagStatusParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let _dag_id = &params.0.dag_id;

        info!("Checking DAG status");

        match ergatai_core::cross_agent::get_dag_scheduler() {
            None => {
                let result = serde_json::json!({
                    "status": "no_dag",
                    "message": "No DAG scheduler is active",
                });
                Ok(CallToolResult::success(vec![ContentBlock::text(
                    serde_json::to_string_pretty(&result).unwrap_or_default(),
                )]))
            }
            Some(scheduler) => {
                let progress = scheduler.progress().await;
                let is_complete = scheduler.is_complete().await;
                let status_text = scheduler.status_prompt().await;
                let snapshot = scheduler.graph_snapshot().await.ok();

                let status = if is_complete { "completed" } else { "running" };

                let result = serde_json::json!({
                    "status": status,
                    "progress": progress,
                    "is_complete": is_complete,
                    "graph_status": status_text,
                    "graph_snapshot": snapshot,
                });
                Ok(CallToolResult::success(vec![ContentBlock::text(
                    serde_json::to_string_pretty(&result).unwrap_or_default(),
                )]))
            }
        }
    }

    /// Get the current collaboration session (participants + communication policy)
    #[tool(
        description = "Get the current collaboration session status: participant agents, communication policy (open/adjacent/star), and DAG binding. Optional dag_id selects a specific session; otherwise returns the most recent."
    )]
    async fn get_collaboration_status(
        &self,
        params: Parameters<GetCollaborationStatusParams>,
    ) -> Result<CallToolResult, ErrorData> {
        // When dag_id is specified, look up that specific scheduler.
        // When dag_id is None, fall back to the most recent scheduler.
        let scheduler = if params.0.dag_id.is_some() {
            ergatai_core::cross_agent::get_dag_scheduler_by_id(params.0.dag_id.as_deref())
        } else {
            ergatai_core::cross_agent::get_dag_scheduler()
        };

        match (scheduler, params.0.dag_id) {
            (Some(s), _) => {
                let session = s.collaboration().await;
                Ok(CallToolResult::success(vec![ContentBlock::text(
                    serde_json::to_string_pretty(&session).unwrap_or_default(),
                )]))
            }
            (None, Some(dag_id)) => Ok(CallToolResult::success(vec![ContentBlock::text(
                serde_json::to_string_pretty(&serde_json::json!({
                    "status": "not_found",
                    "message": format!("No collaboration session found for dag_id: {}", dag_id),
                }))
                .unwrap_or_default(),
            )])),
            (None, None) => Ok(CallToolResult::success(vec![ContentBlock::text(
                serde_json::to_string_pretty(&serde_json::json!({
                    "status": "no_active_session",
                    "message": "No collaboration session is active. Submit a DAG orchestration first.",
                }))
                .unwrap_or_default(),
            )])),
        }
    }

    // ── File Access Control Tools ──

    /// Request file access lock for reading or writing
    #[tool(
        description = "Request file access lock. Use this before reading or writing files in multi-agent mode. Returns a lock token if approved."
    )]
    async fn request_file_access(
        &self,
        params: Parameters<RequestFileAccessParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let file_path = &params.0.file_path;
        let mode_str = params.0.mode.to_uppercase();
        let reason = params.0.reason.clone();
        let scope = params.0.scope.clone();

        // Get agent info from session
        let agent_id = self
            .session_agent_id
            .read()
            .await
            .clone()
            .unwrap_or_else(|| "unknown".to_string());

        info!(
            agent_id = %agent_id,
            file_path = %file_path,
            mode = %mode_str,
            "File access request via MCP"
        );

        // Parse mode
        let mode = match mode_str.as_str() {
            "READ" => ergatai_lock::FileMode::Read,
            "WRITE" => ergatai_lock::FileMode::Write,
            "ADMIN" => ergatai_lock::FileMode::Admin,
            _ => {
                return Err(ErrorData::invalid_params(
                    format!("Invalid mode '{}'. Must be READ, WRITE, or ADMIN", mode_str),
                    None,
                ));
            }
        };

        // Try to get lock manager (may not be initialized)
        let lock_manager = match ergatai_lock::get_lock_manager("default").await {
            Ok(lm) => lm,
            Err(e) => {
                // File lock not initialized - return success with note
                // In single-agent mode, file locks are not needed
                return Ok(CallToolResult::success(vec![ContentBlock::text(
                    serde_json::to_string_pretty(&serde_json::json!({
                        "status": "granted",
                        "file_path": file_path,
                        "mode": mode_str,
                        "note": "File lock system not active (single-agent mode). Access granted directly.",
                        "warning": format!("Lock manager not available: {}", e)
                    }))
                    .unwrap_or_default(),
                )]));
            }
        };

        // Create a file token for this request
        let session_id = format!("mcp-{}", agent_id);
        let system_token_id = ergatai_lock::TokenId::new();

        let file_token = ergatai_lock::FileToken::new(
            agent_id.clone(),
            session_id.clone(),
            system_token_id,
            scope.clone(),
            mode,
            reason.clone(),
            "mcp-request".to_string(),
            3600, // 1 hour TTL
            60,   // heartbeat every 60s
        );

        // Register the file token — fail early if registration fails so the lock
        // state stays consistent (otherwise acquire_lock would proceed without
        // a registered token, and subsequent release/list operations would be broken).
        if let Err(e) = lock_manager.register_file_token(&file_token) {
            warn!(
                agent_id = %agent_id,
                file_path = %file_path,
                error = %e,
                "Failed to register file token, denying access"
            );
            return Err(ErrorData::internal_error(
                format!("Failed to register file access token: {}", e),
                None,
            ));
        }

        // Try to acquire the lock
        match lock_manager.acquire_lock(&file_token, file_path).await {
            Ok(()) => {
                info!(
                    agent_id = %agent_id,
                    file_path = %file_path,
                    token_id = %file_token.id,
                    "File lock acquired successfully"
                );

                Ok(CallToolResult::success(vec![ContentBlock::text(
                    serde_json::to_string_pretty(&serde_json::json!({
                        "status": "granted",
                        "file_path": file_path,
                        "mode": mode_str,
                        "token_id": file_token.id.as_str(),
                        "scope": scope,
                        "expires_at": file_token.expires_at.to_rfc3339(),
                        "note": "File lock acquired. Remember to release when done."
                    }))
                    .unwrap_or_default(),
                )]))
            }
            Err(e) => {
                // Lock acquisition failed (conflict) - try MCP elicitation for user approval
                warn!(
                    agent_id = %agent_id,
                    file_path = %file_path,
                    error = %e,
                    "File lock conflict detected, requesting user approval via elicitation"
                );

                // Try to get the peer for this session and send elicitation
                {
                    if let Some(peer) = self.peer_registry.read().await.get(&agent_id).cloned() {
                        let approval_message = format!(
                            "🔒 File Access Conflict\n\n\
                             Agent wants to {} file: {}\n\
                             Reason: {}\n\
                             Conflict: {}\n\n\
                             Approve this access?",
                            mode_str,
                            file_path,
                            reason.as_deref().unwrap_or("not specified"),
                            e
                        );

                        match peer.elicit::<ApprovalResponse>(&approval_message).await {
                            Ok(Some(response)) if response.decision.to_lowercase() == "yes" => {
                                info!(
                                    agent_id = %agent_id,
                                    file_path = %file_path,
                                    "User approved file access via elicitation"
                                );
                                // User approved - grant access directly (bypass lock)
                                return Ok(CallToolResult::success(vec![ContentBlock::text(
                                    serde_json::to_string_pretty(&serde_json::json!({
                                        "status": "granted",
                                        "file_path": file_path,
                                        "mode": mode_str,
                                        "approval": "user_approved",
                                        "note": "Access granted by user approval despite conflict."
                                    }))
                                    .unwrap_or_default(),
                                )]));
                            }
                            Ok(Some(_)) => {
                                // User declined
                                info!(
                                    agent_id = %agent_id,
                                    file_path = %file_path,
                                    "User denied file access via elicitation"
                                );
                            }
                            Ok(None) => {
                                // No response (cancelled)
                                warn!(
                                    agent_id = %agent_id,
                                    file_path = %file_path,
                                    "User cancelled file access approval"
                                );
                            }
                            Err(ElicitationError::CapabilityNotSupported) => {
                                // Client doesn't support elicitation — deny rather than auto-approve.
                                // Auto-approving here would let any agent bypass file locks by connecting
                                // with a client that doesn't implement elicitation. The user/admin can
                                // manually grant access or upgrade the client.
                                warn!(
                                    agent_id = %agent_id,
                                    file_path = %file_path,
                                    "Client does not support elicitation, denying file access (conflict unresolved)"
                                );
                            }
                            Err(e) => {
                                // Elicitation failed — deny rather than auto-approve.
                                // Silently granting on failure defeats the purpose of the lock system.
                                warn!(
                                    agent_id = %agent_id,
                                    file_path = %file_path,
                                    error = %e,
                                    "Elicitation failed, denying file access (conflict unresolved)"
                                );
                            }
                        }
                    } else {
                        // No peer found — deny rather than auto-approve.
                        // A missing peer session is not a valid reason to bypass file locks.
                        warn!(
                            agent_id = %agent_id,
                            file_path = %file_path,
                            "No peer found in registry, denying file access (conflict unresolved)"
                        );
                    }
                }

                // No elicitation or user declined - return error
                Err(ErrorData::internal_error(
                    format!("File access denied: {}", e),
                    None,
                ))
            }
        }
    }

    /// Release a file access lock
    #[tool(
        description = "Release a file access lock when done reading/writing. Call this after completing file operations."
    )]
    async fn release_file_access(
        &self,
        params: Parameters<ReleaseFileAccessParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let file_path = &params.0.file_path;

        let agent_id = self
            .session_agent_id
            .read()
            .await
            .clone()
            .unwrap_or_else(|| "unknown".to_string());

        info!(
            agent_id = %agent_id,
            file_path = %file_path,
            "File lock release request via MCP"
        );

        let lock_manager = match ergatai_lock::get_lock_manager("default").await {
            Ok(lm) => lm,
            Err(_) => {
                return Ok(CallToolResult::success(vec![ContentBlock::text(
                    serde_json::to_string_pretty(&serde_json::json!({
                        "status": "released",
                        "file_path": file_path,
                        "note": "File lock system not active."
                    }))
                    .unwrap_or_default(),
                )]));
            }
        };

        // Find the lock for this agent and file
        let session_id = format!("mcp-{}", agent_id);
        let locks = match lock_manager.get_locks_by_session(&session_id) {
            Ok(locks) => locks,
            Err(e) => {
                return Err(ErrorData::internal_error(
                    format!("Failed to find lock: {}", e),
                    None,
                ));
            }
        };

        // Find the lock for the specific file
        let lock = locks.iter().find(|l| l.file_path == *file_path);
        match lock {
            Some(lock) => {
                match lock_manager
                    .release_lock(lock.token_id.as_str(), file_path)
                    .await
                {
                    Ok(()) => {
                        info!(
                            agent_id = %agent_id,
                            file_path = %file_path,
                            "File lock released successfully"
                        );

                        Ok(CallToolResult::success(vec![ContentBlock::text(
                            serde_json::to_string_pretty(&serde_json::json!({
                                "status": "released",
                                "file_path": file_path,
                                "token_id": &lock.token_id
                            }))
                            .unwrap_or_default(),
                        )]))
                    }
                    Err(e) => Err(ErrorData::internal_error(
                        format!("Failed to release lock: {}", e),
                        None,
                    )),
                }
            }
            None => Ok(CallToolResult::success(vec![ContentBlock::text(
                serde_json::to_string_pretty(&serde_json::json!({
                    "status": "no_lock",
                    "file_path": file_path,
                    "note": "No active lock found for this file."
                }))
                .unwrap_or_default(),
            )])),
        }
    }

    /// List all active file locks
    #[tool(description = "List all active file locks. Shows which agents hold which file locks.")]
    async fn list_active_locks(
        &self,
        params: Parameters<ListActiveLocksParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let agent_filter = params.0.agent_id.clone();

        let lock_manager = match ergatai_lock::get_lock_manager("default").await {
            Ok(lm) => lm,
            Err(_) => {
                return Ok(CallToolResult::success(vec![ContentBlock::text(
                    serde_json::to_string_pretty(&serde_json::json!({
                        "status": "not_active",
                        "locks": [],
                        "note": "File lock system not active."
                    }))
                    .unwrap_or_default(),
                )]));
            }
        };

        match lock_manager.get_all_active_locks() {
            Ok(locks) => {
                let filtered_locks: Vec<serde_json::Value> = locks
                    .iter()
                    .filter(|lock| {
                        agent_filter
                            .as_ref()
                            .is_none_or(|filter| &lock.agent_id == filter)
                    })
                    .map(|lock| {
                        serde_json::json!({
                            "file_path": lock.file_path,
                            "agent_id": lock.agent_id,
                            "session_id": lock.session_id,
                            "mode": format!("{:?}", lock.mode),
                            "token_id": lock.token_id,
                            "reason": lock.reason,
                            "created_at": lock.created_at.to_rfc3339(),
                            "expires_at": lock.expires_at.to_rfc3339()
                        })
                    })
                    .collect();

                Ok(CallToolResult::success(vec![ContentBlock::text(
                    serde_json::to_string_pretty(&serde_json::json!({
                        "status": "ok",
                        "total": filtered_locks.len(),
                        "locks": filtered_locks
                    }))
                    .unwrap_or_default(),
                )]))
            }
            Err(e) => Err(ErrorData::internal_error(
                format!("Failed to list locks: {}", e),
                None,
            )),
        }
    }

    // ── Private helpers for send_message ──

    /// Check if this message is a reply (i.e., the recipient previously sent to the sender)
    async fn is_reply_message(&self, from_agent: &str, to_agent: &str) -> bool {
        // Build conversation ID using same logic as Conversation::new (sorted alphabetically)
        let (a, b) = if from_agent < to_agent {
            (from_agent, to_agent)
        } else {
            (to_agent, from_agent)
        };
        let conversation_id = format!("conv-{}-{}", a, b);

        let conv_manager = &self.conversation_manager;

        // Check if this is a reply by looking at token ownership:
        // - If token is held by from_agent (sender), it means to_agent sent last
        // - So this message from from_agent is a reply to to_agent's previous message
        if let Some(conv) = conv_manager.get_conversation(&conversation_id).await {
            let result = match &conv.token_owner {
                TokenOwner::Held(holder) if holder == from_agent => {
                    // Sender holds the token = recipient sent last = this is a reply
                    true
                }
                _ => false,
            };
            info!(
                conversation_id = %conversation_id,
                from_agent = %from_agent,
                to_agent = %to_agent,
                token_owner = ?conv.token_owner,
                turn_count = conv.turn_count,
                is_reply = result,
                "is_reply_message: conversation found"
            );
            return result;
        }

        info!(
            conversation_id = %conversation_id,
            from_agent = %from_agent,
            to_agent = %to_agent,
            "is_reply_message: no conversation found, returning false"
        );
        false
    }

    /// Get human-readable agent display name.
    ///
    /// Returns the display_name if set (e.g., "frontend-dev"),
    /// otherwise falls back to the runtime ID (e.g., "%72").
    async fn get_agent_display_name(
        runtime: &ergatai_runtime::AgentRuntime,
        agent_id: &str,
    ) -> String {
        if let Some(info) = runtime.get_agent(agent_id).await {
            if let Some(ref display_name) = info.display_name {
                return display_name.clone();
            }
        }
        agent_id.to_string()
    }

    /// Format message as JSON payload with contextual hint.
    /// Protocol rules are sent once during MCP initialize (via ServerInfo.instructions),
    /// so we only send the message content here — saving ~800 tokens per message.
    ///
    /// Different hints are injected based on message type:
    /// - Initial send (A→B): `[用 send_message MCP 回复，不要提问]`
    /// - Reply (B→A): `[如无疑问题，展示"收到"即可]`
    fn format_agent_message(sender_display: &str, message: &str, is_reply: bool) -> String {
        let message_json = serde_json::json!({
            "from": sender_display,
            "message": message
        });

        let hint = if is_reply {
            "[If no questions, just show \"Received\"]\n"
        } else {
            "[Reply via send_message MCP. Answer as requested; if no request, do not ask questions]\n"
        };

        format!("{}{}", message_json, hint)
    }

    /// Inject message via AgentRuntime (fallback when NATS is unavailable).
    async fn try_tmux_injection(
        &self,
        resolved_agent_id: &str,
        from_agent: &str,
        message: &str,
        is_reply: bool,
    ) -> Result<CallToolResult, ErrorData> {
        // Resolve sender MCP ID → runtime ID so the receiver can reply via send_message.
        let runtime = get_agent_runtime();
        let sender_runtime_id = runtime
            .resolve_agent_id(from_agent)
            .await
            .unwrap_or_else(|| from_agent.to_string());

        // Get human-readable display name (e.g., "frontend-dev (%72)")
        let sender_display = Self::get_agent_display_name(&runtime, &sender_runtime_id).await;

        // Format message with contextual hint based on message type
        let formatted_message = Self::format_agent_message(&sender_display, message, is_reply);

        info!(
            "Attempting AgentRuntime injection to agent {}: {}",
            resolved_agent_id, formatted_message
        );

        // Try to inject via AgentRuntime (uses backend injection, e.g. rmux/tmux send_text)
        let runtime = get_agent_runtime();
        match runtime
            .inject_message(resolved_agent_id, &formatted_message)
            .await
        {
            Ok(()) => {
                info!("Message injected to {} via AgentRuntime", resolved_agent_id);
                Ok(CallToolResult::success(vec![ContentBlock::text(
                    serde_json::to_string_pretty(&serde_json::json!({
                        "status": "sent",
                        "target_agent": resolved_agent_id,
                        "delivery_method": "runtime_injection",
                        "note": "Message injected via AgentRuntime (backend tmux/rmux injection)."
                    }))
                    .unwrap_or_default(),
                )]))
            }
            Err(e) => {
                warn!(
                    "AgentRuntime injection to {} failed: {}",
                    resolved_agent_id, e
                );
                Err(ErrorData::internal_error(
                    format!("AgentRuntime injection failed: {}", e),
                    None,
                ))
            }
        }
    }
}

// ── ServerHandler implementation ──

#[tool_handler(router = self.tool_router)]
impl ServerHandler for ErgataiMcpServer {
    /// Handle initialize - auto-register the agent and save peer handle
    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<rmcp::RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        let agent_id = request.client_info.name.clone();
        let agent_version = request.client_info.version.clone();

        // Generate a connection ID - use as unique agent key to support
        // multiple instances of the same client (e.g. 3 OpenCode instances)
        let connection_id = uuid::Uuid::new_v4().to_string();
        // Take first 8 chars of UUID (safe: UUIDs are always 36 chars: 8-4-4-4-12)
        let id_prefix = connection_id.get(..8).unwrap_or(&connection_id);
        let unique_agent_id = format!("{}@{}", agent_id, id_prefix);

        info!(
            "Agent connecting: {} (version: {}, protocol: {}) → {}",
            agent_id, agent_version, request.protocol_version, unique_agent_id
        );

        // Store the agent ID for this session (used in send_message)
        *self.session_agent_id.write().await = Some(unique_agent_id.clone());

        // Register agent in registry
        if let Err(e) = self
            .registry
            .register_agent(unique_agent_id.clone(), connection_id.clone(), None)
            .await
        {
            return Err(ErrorData::invalid_params(
                format!("Failed to register agent: {}", e),
                None::<serde_json::Value>,
            ));
        }

        // Save the peer handle for pushing notifications to this agent
        self.peer_registry
            .write()
            .await
            .insert(unique_agent_id.clone(), context.peer.clone());

        info!(
            "Agent registered: {} (connection: {}, peer handle saved)",
            unique_agent_id, connection_id
        );

        // Try to bind this MCP agent to a runtime agent (rmux pane).
        // If agent_identifier is available (from URL path), use precise binding.
        // Otherwise, fall back to FIFO binding (legacy behavior).
        let runtime = get_agent_runtime();

        // Trigger immediate discovery to ensure runtime agents are available.
        // This handles the race condition where MCP connects before the periodic
        // discovery (30s interval) has run.
        if let Err(e) = runtime.discover_and_register_agents().await {
            warn!(error = %e, "Immediate discovery on MCP connect failed");
        }

        match &self.agent_identifier {
            Some(identifier) => {
                // Precise binding based on agent identifier from URL path
                match runtime
                    .try_bind_mcp_agent_with_identifier(&unique_agent_id, identifier)
                    .await
                {
                    Some(runtime_id) => {
                        info!(
                            mcp_agent_id = unique_agent_id,
                            runtime_id = runtime_id,
                            agent_identifier = identifier,
                            "MCP agent bound to runtime agent by identifier"
                        );

                        // Auto-set display_name from URL path identifier
                        // This allows agents to be addressed by their URL name (e.g., "agent-1")
                        if let Err(e) = runtime
                            .set_display_name(&runtime_id, identifier.clone())
                            .await
                        {
                            warn!(
                                runtime_id = runtime_id,
                                display_name = identifier,
                                error = %e,
                                "Failed to auto-set display_name from URL path"
                            );
                        } else {
                            info!(
                                runtime_id = runtime_id,
                                display_name = identifier,
                                "Auto-set display_name from URL path"
                            );
                        }
                    }
                    None => {
                        warn!(
                            mcp_agent_id = unique_agent_id,
                            agent_identifier = identifier,
                            "Failed to bind MCP agent: no runtime agent with matching identifier"
                        );
                    }
                }
            }
            None => {
                // Fallback to FIFO binding (legacy behavior)
                match runtime.try_bind_mcp_agent(&unique_agent_id).await {
                    Some(runtime_id) => {
                        info!(
                            mcp_agent_id = unique_agent_id,
                            runtime_id = runtime_id,
                            "MCP agent bound to runtime agent on connect"
                        );
                    }
                    None => {
                        info!(
                            mcp_agent_id = unique_agent_id,
                            "MCP agent queued for binding (no unmapped runtime agent yet)"
                        );
                    }
                }
            }
        }

        // Build the initialize result
        let mut server_info = self.get_info();
        // Negotiate: use client's version if we know it, otherwise our latest
        let client_version = &request.protocol_version;
        let known = rmcp::model::ProtocolVersion::KNOWN_VERSIONS
            .iter()
            .any(|v| v.as_str() == client_version.as_str());
        server_info.protocol_version = if known {
            client_version.clone()
        } else {
            rmcp::model::ProtocolVersion::default()
        };

        // Store peer info in context
        context.peer.set_peer_info(request);

        Ok(server_info)
    }

    /// Return server info with tools capability
    fn get_info(&self) -> ServerInfo {
        let instructions = r#"# CRITICAL: Multi-Agent Communication Protocol

## When you receive a message from another agent:
**You MUST reply using the `send_message` MCP tool, NOT by typing in the terminal.**

Example: If you receive "你好！" from agent %45, you must call:
```
send_message(target_agent_id="%45", message="你的回复")
```

## ONE-QUESTION-ONE-ANSWER Protocol
1. Receive one message → Reply exactly once via `send_message` → TERMINATE
2. Do NOT reply again after TERMINATE
3. Do NOT ask follow-up questions

## Reply Format
- Max 150 Chinese characters
- Direct, no pleasantries
- End with "TERMINATE" on a new line

## Exception Handling
- Vague question → Point out what's missing, then TERMINATE
- Harmful content → Decline, then TERMINATE
- Need tools → Call tool once, integrate result, then TERMINATE

# DO NOT:
- Reply in terminal (use send_message MCP tool instead)
- Ask "Anything else I can help?"
- Continue conversation after TERMINATE"#;

        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(rmcp::model::Implementation::new(
                "ergatai",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(instructions)
    }
}

// ── Public API for creating the Streamable HTTP service ──

use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};

/// Create the MCP Streamable HTTP service for mounting in axum.
///
/// Returns a `StreamableHttpService` that handles POST/GET/DELETE /mcp
/// with proper MCP 2025-06-18 protocol support.
///
/// # Arguments
/// * `registry` - Agent registry for tracking connected agents
/// * `peer_registry` - Shared registry of MCP peer handles for pushing notifications
/// * `conversation_manager` - Conversation manager for loop prevention
/// * `cancellation_token` - Token for graceful shutdown
/// * `sse_keep_alive_secs` - SSE keep-alive interval in seconds (default 15)
pub fn create_mcp_service(
    registry: Arc<AgentRegistry>,
    peer_registry: PeerRegistry,
    conversation_manager: Arc<ConversationManager>,
    cancellation_token: CancellationToken,
    sse_keep_alive_secs: u64,
    agent_identifier: Option<String>,
) -> StreamableHttpService<ErgataiMcpServer, LocalSessionManager> {
    let config = StreamableHttpServerConfig::default()
        .with_sse_keep_alive(Some(std::time::Duration::from_secs(sse_keep_alive_secs)))
        .with_sse_retry(Some(std::time::Duration::from_secs(3)))
        .with_json_response(true)
        .with_cancellation_token(cancellation_token)
        .with_allowed_hosts(["localhost", "127.0.0.1", "::1", "0.0.0.0"]);

    // Session keep_alive: auto-close sessions after this duration of inactivity.
    // This catches dead clients (kill, network drop) within 2 minutes.
    // Default is 300s (5 min). Agents that call tools periodically stay alive.
    let mut session_manager = LocalSessionManager::default();
    session_manager.session_config.keep_alive = Some(std::time::Duration::from_secs(120));

    StreamableHttpService::new(
        move || {
            Ok(ErgataiMcpServer::new(
                registry.clone(),
                peer_registry.clone(),
                conversation_manager.clone(),
                agent_identifier.clone(),
            ))
        },
        std::sync::Arc::new(session_manager),
        config,
    )
}

/// Start a background task that periodically checks all peer connections
/// and removes agents whose MCP transport has been closed (e.g. abrupt disconnect).
///
/// This complements the `Drop`-based cleanup which only fires on graceful session close.
/// When a client is killed (SIGKILL, network drop), the SSE session may linger until
/// the transport detects the broken connection. The reaper proactively cleans these up.
pub fn start_peer_reaper(
    registry: Arc<AgentRegistry>,
    peer_registry: PeerRegistry,
    cancellation_token: CancellationToken,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
        loop {
            tokio::select! {
                _ = cancellation_token.cancelled() => {
                    info!("Peer reaper shutting down");
                    break;
                }
                _ = interval.tick() => {
                    let stale_peers: Vec<String> = {
                        let peers = peer_registry.read().await;
                        peers.iter()
                            .filter(|(_, peer)| peer.is_transport_closed())
                            .map(|(id, _)| id.clone())
                            .collect()
                    };

                    for agent_id in stale_peers {
                        warn!("Peer reaper: detected dead transport for {}, cleaning up", agent_id);
                        do_unregister_agent(
                            &registry, &peer_registry, &agent_id, "dead transport (reaper)",
                        ).await;
                    }
                }
            }
        }
    });
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::conversation::ConversationConfig;
    use ergatai_core::agent_registry::AgentRegistry;
    use serde_json::json;

    // ── PeerRegistry tests ──

    #[test]
    fn test_new_peer_registry_is_empty() {
        let registry = new_peer_registry();
        // Block on async read to check emptiness
        let map = registry.blocking_read();
        assert!(map.is_empty(), "new_peer_registry should start empty");
    }

    #[tokio::test]
    async fn test_peer_registry_insert_and_read() {
        let registry = new_peer_registry();
        // We can't easily construct a Peer<RoleServer> here, but we can verify
        // the registry's type and that write().await works.
        let map = registry.write().await;
        assert_eq!(map.len(), 0);
        // Insert/remove a dummy key (peer is opaque, we just test the HashMap mechanics)
        // Since Peer is not constructible without an MCP connection, we only verify
        // that the registry operations don't panic on an empty registry.
        drop(map);
        let map = registry.read().await;
        assert!(map.is_empty());
    }

    #[tokio::test]
    async fn test_peer_registry_remove_missing_key_is_noop() {
        let registry = new_peer_registry();
        let mut map = registry.write().await;
        let removed = map.remove("nonexistent-agent");
        assert!(
            removed.is_none(),
            "removing a missing key should return None"
        );
    }

    // ── ErgataiMcpServer::new() tests ──

    fn make_test_server() -> ErgataiMcpServer {
        let registry = Arc::new(AgentRegistry::new());
        let peer_registry = new_peer_registry();
        let conversation_manager =
            Arc::new(ConversationManager::new(ConversationConfig::default()));
        ErgataiMcpServer::new(registry, peer_registry, conversation_manager, None)
    }

    #[test]
    fn test_mcp_server_new_creates_instance() {
        let server = make_test_server();
        // Verify the server was created successfully (Debug impl works, no panics)
        let debug_str = format!("{:?}", server);
        assert!(
            debug_str.contains("ErgataiMcpServer"),
            "Debug output should contain struct name"
        );
    }

    #[tokio::test]
    async fn test_mcp_server_new_initial_session_agent_id_is_none() {
        let server = make_test_server();
        let agent_id = server.session_agent_id.read().await.clone();
        assert!(
            agent_id.is_none(),
            "session_agent_id should be None before initialize"
        );
    }

    #[test]
    fn test_mcp_server_clone() {
        // ErgataiMcpServer derives Clone; verify clone doesn't panic
        let server = make_test_server();
        let _cloned = server.clone();
    }

    // ── get_info tests ──

    #[test]
    fn test_get_info_returns_server_info() {
        let server = make_test_server();
        let info = server.get_info();
        // Verify the server name is "ergatai"
        assert_eq!(info.server_info.name, "ergatai");
        // Version comes from CARGO_PKG_VERSION
        assert!(!info.server_info.version.is_empty());
    }

    #[test]
    fn test_get_info_has_tools_capability() {
        let server = make_test_server();
        let info = server.get_info();
        // The capabilities should have tools enabled
        let caps_json = serde_json::to_value(&info.capabilities).unwrap();
        assert!(
            caps_json.get("tools").is_some(),
            "Server capabilities should include 'tools'"
        );
    }

    // ── Parameter deserialization tests ──

    #[test]
    fn test_list_agents_params_empty_object() {
        let params: ListAgentsParams = serde_json::from_value(json!({})).unwrap();
        assert_eq!(params.include_capabilities, None);
    }

    #[test]
    fn test_list_agents_params_with_true() {
        let params: ListAgentsParams =
            serde_json::from_value(json!({"include_capabilities": true})).unwrap();
        assert_eq!(params.include_capabilities, Some(true));
    }

    #[test]
    fn test_list_agents_params_with_false() {
        let params: ListAgentsParams =
            serde_json::from_value(json!({"include_capabilities": false})).unwrap();
        assert_eq!(params.include_capabilities, Some(false));
    }

    #[test]
    fn test_list_agents_params_ignores_extra_fields() {
        let params: ListAgentsParams =
            serde_json::from_value(json!({"include_capabilities": true, "unknown": 123})).unwrap();
        assert_eq!(params.include_capabilities, Some(true));
    }

    #[test]
    fn test_send_message_params_required_fields() {
        let params: SendMessageParams = serde_json::from_value(json!({
            "target_agent_id": "agent-1",
            "message": "hello"
        }))
        .unwrap();
        assert_eq!(params.target_agent_id, "agent-1");
        assert_eq!(params.message, "hello");
        assert_eq!(params.message_type, None);
    }

    #[test]
    fn test_send_message_params_with_message_type() {
        let params: SendMessageParams = serde_json::from_value(json!({
            "target_agent_id": "agent-1",
            "message": "hello",
            "message_type": "broadcast"
        }))
        .unwrap();
        assert_eq!(params.message_type.as_deref(), Some("broadcast"));
    }

    #[test]
    fn test_send_message_params_missing_target_fails() {
        let result: Result<SendMessageParams, _> =
            serde_json::from_value(json!({"message": "hello"}));
        assert!(
            result.is_err(),
            "missing target_agent_id should fail deserialization"
        );
    }

    #[test]
    fn test_send_message_params_missing_message_fails() {
        let result: Result<SendMessageParams, _> =
            serde_json::from_value(json!({"target_agent_id": "a"}));
        assert!(
            result.is_err(),
            "missing message should fail deserialization"
        );
    }

    #[test]
    fn test_submit_orchestration_params_with_dag_only() {
        let params: SubmitOrchestrationParams = serde_json::from_value(json!({
            "dag_definition": "## Task A\n- agent: a\n"
        }))
        .unwrap();
        assert!(params.dag_definition.contains("Task A"));
        assert!(params.context.is_none());
    }

    #[test]
    fn test_submit_orchestration_params_with_context() {
        let params: SubmitOrchestrationParams = serde_json::from_value(json!({
            "dag_definition": "dag",
            "context": {"key": "value", "num": 42}
        }))
        .unwrap();
        let ctx = params.context.unwrap();
        assert_eq!(ctx["key"].as_str(), Some("value"));
        assert_eq!(ctx["num"].as_i64(), Some(42));
    }

    #[test]
    fn test_submit_orchestration_params_missing_dag_fails() {
        let result: Result<SubmitOrchestrationParams, _> =
            serde_json::from_value(json!({"context": {}}));
        assert!(result.is_err());
    }

    #[test]
    fn test_check_dag_status_params_valid() {
        let params: CheckDagStatusParams =
            serde_json::from_value(json!({"dag_id": "abc-123"})).unwrap();
        assert_eq!(params.dag_id, "abc-123");
    }

    #[test]
    fn test_check_dag_status_params_empty_string() {
        let params: CheckDagStatusParams = serde_json::from_value(json!({"dag_id": ""})).unwrap();
        assert_eq!(params.dag_id, "");
    }

    #[test]
    fn test_check_dag_status_params_missing_dag_id_fails() {
        let result: Result<CheckDagStatusParams, _> = serde_json::from_value(json!({}));
        assert!(result.is_err());
    }

    // ── Message formatting helper tests ──

    #[test]
    fn test_message_formatting_prefix() {
        // The formatted message in try_tmux_injection is:
        // format!("Message from {}: {}", from_agent, message)
        let from = "agent-A";
        let message = "please review";
        let formatted = format!("Message from {}: {}", from, message);
        assert_eq!(formatted, "Message from agent-A: please review");
    }

    #[test]
    fn test_message_formatting_empty_message() {
        let formatted = format!("Message from {}: {}", "sender", "");
        assert_eq!(formatted, "Message from sender: ");
    }

    // ── Agent ID prefix matching logic tests ──

    #[test]
    fn test_agent_id_exact_match() {
        let target = "simple-agent@ead00fad";
        let candidate = "simple-agent@ead00fad";
        assert_eq!(candidate, target);
    }

    #[test]
    fn test_agent_id_prefix_match_logic() {
        // The send_message code uses:
        // a.agent_id.starts_with(&format!("{}@", target_agent_id))
        let target = "simple-agent";
        let agent_id = "simple-agent@ead00fad";
        assert!(agent_id.starts_with(&format!("{}@", target)));
    }

    #[test]
    fn test_agent_id_prefix_no_false_positive() {
        // "simple" should NOT match "simple-agent@xxx"
        let target = "simple";
        let agent_id = "simple-agent@ead00fad";
        assert!(!agent_id.starts_with(&format!("{}@", target)));
    }

    #[test]
    fn test_agent_id_prefix_empty_target_does_not_match() {
        // Edge case: empty target produces "@", which does NOT match "agent@abc"
        // (because "agent@abc" starts with 'a', not '@'). This confirms the prefix
        // match is safe against empty/missing target_agent_id.
        let target = "";
        let agent_id = "agent@abc";
        assert!(!agent_id.starts_with(&format!("{}@", target)));
    }

    // ── do_unregister_agent tests ──

    #[tokio::test]
    async fn test_do_unregister_agent_removes_from_registry() {
        let registry = AgentRegistry::new();
        let peer_registry = new_peer_registry();

        // Register an agent first
        registry
            .register_agent("agent-1".to_string(), "conn-1".to_string(), None)
            .await
            .unwrap();

        // Verify it's registered
        let agents = registry.list_agents().await;
        assert_eq!(agents.len(), 1);

        // Unregister
        do_unregister_agent(&registry, &peer_registry, "agent-1", "test").await;

        // Verify it's gone
        let agents = registry.list_agents().await;
        assert_eq!(agents.len(), 0);
    }

    #[tokio::test]
    async fn test_do_unregister_agent_removes_from_peer_registry() {
        let registry = AgentRegistry::new();
        let peer_registry = new_peer_registry();

        // Manually insert a dummy entry (we can't create a real Peer, so we test
        // the mechanics by inserting then checking removal logic via another path).
        // Since Peer is opaque, we just verify that removing from an empty registry
        // doesn't panic.
        do_unregister_agent(&registry, &peer_registry, "nonexistent", "test").await;

        let map = peer_registry.read().await;
        assert!(map.is_empty());
    }

    #[tokio::test]
    async fn test_do_unregister_agent_is_idempotent() {
        let registry = AgentRegistry::new();
        let peer_registry = new_peer_registry();

        registry
            .register_agent("agent-1".to_string(), "conn-1".to_string(), None)
            .await
            .unwrap();

        // Call unregister twice — second call should be a no-op
        do_unregister_agent(&registry, &peer_registry, "agent-1", "test1").await;
        do_unregister_agent(&registry, &peer_registry, "agent-1", "test2").await;

        let agents = registry.list_agents().await;
        assert_eq!(agents.len(), 0);
    }

    // ── Drop impl tests ──

    #[tokio::test]
    async fn test_drop_unregisters_agent_when_session_id_set() {
        let registry = Arc::new(AgentRegistry::new());
        let peer_registry = new_peer_registry();

        // Register agent manually
        registry
            .register_agent("drop-agent".to_string(), "conn".to_string(), None)
            .await
            .unwrap();

        {
            let conversation_manager =
                Arc::new(ConversationManager::new(ConversationConfig::default()));
            let server = ErgataiMcpServer::new(
                registry.clone(),
                peer_registry.clone(),
                conversation_manager,
                None,
            );
            // Simulate initialize having set the session agent ID
            *server.session_agent_id.write().await = Some("drop-agent".to_string());
            // server is dropped here
        }

        // Give the spawned task time to run
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let agents = registry.list_agents().await;
        assert!(agents.is_empty(), "Drop should have unregistered the agent");
    }

    #[tokio::test]
    async fn test_drop_is_noop_when_session_id_not_set() {
        let registry = Arc::new(AgentRegistry::new());
        let peer_registry = new_peer_registry();

        // Register a different agent (not the one tied to this session)
        registry
            .register_agent("other-agent".to_string(), "conn".to_string(), None)
            .await
            .unwrap();

        {
            let conversation_manager =
                Arc::new(ConversationManager::new(ConversationConfig::default()));
            let _server = ErgataiMcpServer::new(
                registry.clone(),
                peer_registry.clone(),
                conversation_manager,
                None,
            );
            // session_agent_id is None (not initialized), so drop should not unregister anything
        }

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let agents = registry.list_agents().await;
        assert_eq!(
            agents.len(),
            1,
            "Drop without session_agent_id should not unregister any agent"
        );
    }
}

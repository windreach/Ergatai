# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is this?

**Ergatai** - A multi-agent collaboration middleware for AI-assisted software engineering. Transforms individual AI coding assistants into a coordinated engineering team with parallel task execution, safe concurrent file access, and DAG-based workflow orchestration.

**Pure Rust implementation** using **MCP (Model Context Protocol)** for all agent communication — agents connect as MCP clients, call tools, and receive messages via **MCP custom notifications** (no HTTP server required), embedded **NATS** event bus for reliable internal messaging, and **DAG-based orchestration engine** with template-driven data flow.

### Middleware Architecture (Important!)

Ergatai runs as a **middleware** - it does NOT spawn or manage agent processes. Instead:

1. **Agents run independently** and manage their own lifecycle
2. **Agents connect to Ergatai via MCP** (Streamable HTTP) as MCP clients
3. **Agents call tools** (send_message, list_agents, etc.) via MCP tool calls
4. **Agents receive messages** via MCP custom notifications (`ergatai/message`)
5. **No HTTP server needed** — agents never bind ports or expose endpoints

```
┌─────────────────────────────────────────────────────────────────┐
│                      Agent (runs independently)                  │
│  ┌──────────────┐                                               │
│  │   LLM/API    │                                               │
│  │              │                                               │
│  └──────────────┘                                               │
└─────────────────────────────────────────────────────────────────┘
         │ MCP (tools/call)                    ▲ MCP notifications
         ▼                                     │ (ergatai/message)
┌─────────────────────────────────────────────────────────────────┐
│                        Ergatai (Middleware)                      │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────────┐  │
│  │  MCP Server  │  │ PeerRegistry │  │   NATS Event Bus     │  │
│  │  (tools)     │  │ (push)       │  │   (JetStream)        │  │
│  └──────────────┘  └──────────────┘  └──────────────────────┘  │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────────┐  │
│  │  Agent       │  │  DAG         │  │  File Access         │  │
│  │  Registry    │  │  Scheduler   │  │  Control             │  │
│  └──────────────┘  └──────────────┘  └──────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

## Commands

### Build and Test

```bash
# Build all crates
cargo build --workspace

# Build release version
cargo build --release --workspace

# Build a single crate
cargo build -p ergatai-api

# Run all tests
cargo test --workspace

# Run tests for a specific crate
cargo test -p ergatai-core
cargo test -p ergatai-api

# Run a single test file
cargo test -p ergatai-core --test integration_test

# Run a single test function
cargo test -p ergatai-core test_lock_acquire_release
cargo test -p ergatai-core -- --exact test_lock_acquire_release

# Run tests with filter
cargo test -p ergatai-core lock

# Lint
cargo clippy --workspace -- -D warnings

# Format
cargo fmt --all
```

### Run

```bash
# Run API server (Ergatai middleware)
cargo run --bin ergatai-api -- --port 3000

# Run with debug logging
RUST_LOG=debug cargo run --bin ergatai-api -- --port 3000

# Run with API token authentication
ERGATAI_API_TOKEN=secret cargo run --bin ergatai-api -- --port 3000

# Run example agent (in another terminal)
cargo run -p simple-agent -- --port 8080 --agent-id my-agent --ergatai http://localhost:3000

# Run integration test
./tests/integration_test.sh
```

### Environment Variables

- `RUST_LOG` - Log level (error, warn, info, debug, trace)
- `ERGATAI_API_TOKEN` - Bearer token for API authentication (optional)

## Architecture

### Layer Responsibilities

| Layer | Crate | Responsibility |
|-------|-------|----------------|
| **Core** | `ergatai-core` | Business logic facade, re-exports from sub-crates |
| **ACP** | `ergatai-acp` | ACP protocol layer (HTTP client for middleware mode) |
| **Collab** | `ergatai-collab` | Multi-agent collaboration (DAG scheduler, task coordinator) |
| **DAG** | `ergatai-dag` | DAG parsing, template engine, context management |
| **NATS** | `ergatai-nats` | Embedded NATS server and event bus |
| **Lock** | `ergatai-lock` | File access control and token-based locking |
| **Agent** | `ergatai-agent` | Agent config, discovery, hosted agents |
| **Error** | `ergatai-error` | Shared error types |
| **API** | `ergatai-api` | HTTP server with MCP endpoints |

### Workspace Structure

```
crates/
├── ergatai-core/    # Core library facade
├── ergatai-acp/     # ACP protocol (HTTP client)
├── ergatai-collab/  # Multi-agent collaboration
├── ergatai-dag/     # DAG parsing and orchestration
├── ergatai-nats/    # NATS messaging
├── ergatai-lock/    # File access control
├── ergatai-agent/   # Agent config and discovery
├── ergatai-error/   # Error types
└── ergatai-api/     # HTTP/MCP server
    └── src/
        └── mcp/
            ├── agent_registry.rs  # Tracks connected agents
            ├── message_relay.rs   # MCP notification push
            ├── tools.rs           # MCP tool implementations
            └── server.rs          # MCP JSON-RPC handler
examples/
└── simple-agent/    # Example agent demonstrating middleware usage
```

### Communication Architecture (Critical!)

**Two independent layers:**

| Layer | Protocol | Direction | Purpose |
|-------|----------|-----------|---------|
| **Agent ↔ Ergatai** | MCP (Streamable HTTP) | Bidirectional | Agents call tools; Ergatai pushes messages via custom notifications |
| **Ergatai Internal** | NATS (JetStream) | Event stream | Task routing, completion events, file notifications |

**Key point**: Agents never communicate directly. All inter-agent messaging is relayed through Ergatai. Ergatai uses the MCP peer handle to push `ergatai/message` notifications to the target agent's SSE stream.

```
User request: "Refactor this module with 3 agents"
    ↓
CLI generates DAG definition
    ↓
DagScheduler parses → NATS distributes tasks → Sub-agents A/B/C (MCP execution)
                   ↑ NATS events relay completion
```

### Core Modules

**ACP Protocol (`acp/`):**
- `manager.rs` - Session lifecycle management
- `sdk_session.rs` - ACP SDK-based sessions
- `sdk_pool.rs` - Session pooling for efficiency
- `persistence.rs` - Session state persistence

**NATS Messaging (`nats/`):**
- `manager.rs` - NATS server management
- `streams.rs` - JetStream stream definitions
- `event_bus.rs` - Typed event publishing

**DAG Orchestration (`orchestration/`):**
- `task_graph.rs` - DAG parsing and validation
- `template.rs` - Template engine ({{var}} rendering)
- `dag_context.rs` - Context management for data flow

**Multi-Agent Collaboration (`cross_agent/`):**
- `dag_scheduler.rs` - DAG execution scheduling
- `task_scheduler.rs` - Individual task scheduling
- `agent_launcher.rs` - Agent process management
- `message_router.rs` - Inter-agent message routing

**File Access Control (`file_access/`):**
- `lock_manager.rs` - Token-based locking (READ/WRITE/ADMIN)
- `token.rs` - Two-level token system (SystemToken + FileToken)
- `watchdog.rs` - Heartbeat monitoring and lock reclamation
- `snapshot.rs` - Git snapshot creation before writes
- `audit.rs` - Security audit logging

**Agent Management (`agent/`):**
- `config.rs` - Agent configuration structures
- `discovery.rs` - Agent discovery (built-in + custom)
- `hosted_config.rs` - User-defined agent configurations

### File Access Control (Multi-Agent Safety)

Token-based locking prevents conflicting edits:

```rust
// Agent A acquires WRITE lock
let token = lock_manager.acquire_write("src/foo.rs").await?;

// Modify file
fs::write("src/foo.rs", new_content)?;

// Automatic git snapshot
snapshot_manager.create_snapshot("src/foo.rs")?;

// Release lock (Agent B can now acquire)
lock_manager.release(token).await?;
```

**Two-level Token System:**
- `SystemToken` - Session-level admission (binds agent_id + session_id)
- `FileToken` - Operation-level (READ/WRITE/ADMIN scope)

**Database**: `{project_root}/.ergatai/locks.db` (SQLite)

**Single Agent Mode**: When only one agent is active, automatically bypasses approval flow and conflict arbitration (5-second hysteresis debounce).

### DAG Orchestration

```markdown
## Task A (Analyze code)
- **agent**: claude-code
- **task**: tasks/analyze.md

## Task B (Write tests)
- **agent**: cursor
- **task**: tasks/test.md
- **depends_on**: [Task A]
- **input**: "Analysis: {{TaskA.review_result}}"
- **output**: test_result, coverage
- **retry**: 3
- **timeout**: 300
```

**Template Variables:**
- `{{global.*}}` - Global variables (DagContext.global_vars)
- `{{node_id.*}}` - Upstream node outputs (DagContext.node_outputs)

### NATS Subject Naming

```
ergatai.
├── task.submit.{agent}              # DagScheduler → TaskScheduler
├── task.complete.{task_id}          # Agent completion notification
├── dag.node_complete.{node}         # AgentLauncher → DagScheduler
├── dag.complete.{dag_id}            # All tasks complete
├── agent.message.{agent_id}         # Inter-agent messages (@mention)
├── file.access.request              # File lock requests (JetStream)
├── file.ready.{md5_hash}            # File WRITE complete notification
└── file.error.{md5_hash}            # File WRITE failed notification
```

**JetStream Streams:**
- `TASK_QUEUE` - Task distribution (WorkQueue retention)
- `FILE_ACCESS_REQUESTS/GRANTS/ESCALATIONS` - File access control
- `FILE_EVENTS` - File ready/error notifications
- `LOCK_WAITERS` - Lock waiting queue

## Database

**Location**: `{project_root}/.ergatai/ergatai.db` (SQLite)

**Key Tables:**
- `projects` - Project information
- `agents` - Agent configurations
- `sessions` - Session records
- `tasks` - Task execution records

**Lock Database**: `{project_root}/.ergatai/locks.db` (SQLite with 5 tables)

## Important Files

### Core Library
- `crates/ergatai-core/src/lib.rs` - Library entry point
- `crates/ergatai-core/src/acp/manager.rs` - ACP session management
- `crates/ergatai-core/src/nats/manager.rs` - NATS global state
- `crates/ergatai-core/src/file_access/lock_manager.rs` - Lock management (largest file)
- `crates/ergatai-core/src/cross_agent/dag_scheduler.rs` - DAG scheduling

### API Server
- `crates/ergatai-api/src/main.rs` - HTTP/MCP server entry point
- `crates/ergatai-api/src/mcp/` - MCP protocol implementation

### Configuration
- `Cargo.toml` - Workspace configuration
- `docs/dev/ARCHITECTURE.md` - Detailed architecture documentation

## MCP Tools (Agent Interface)

Agents connect to Ergatai via MCP and can call these tools:

### `list_agents`
List all connected agents and their status.

```json
{
  "name": "list_agents",
  "arguments": {
    "include_capabilities": true
  }
}
```

### `send_message`
Send a message to another agent via MCP notification.

```json
{
  "name": "send_message",
  "arguments": {
    "target_agent_id": "other-agent",
    "message": "Please review this code",
    "message_type": "request"
  }
}
```

### `submit_orchestration`
Submit a DAG workflow for multi-agent collaboration.

```json
{
  "name": "submit_orchestration",
  "arguments": {
    "dag_definition": "## Task A\n- agent: agent-1\n..."
  }
}
```

### `check_dag_status`
Check the status of a DAG execution.

```json
{
  "name": "check_dag_status",
  "arguments": {
    "dag_id": "uuid-of-dag"
  }
}
```

## Agent Developer Guide

To create an agent that works with Ergatai:

### 1. Connect to Ergatai as an MCP client

Use any MCP SDK (e.g. `rmcp` for Rust) to connect to `http://localhost:3000/mcp`:

```rust
use rmcp::{ClientHandler, ServiceExt, transport::StreamableHttpClientTransport, RoleClient};

struct MyAgent { agent_id: String }

impl ClientHandler for MyAgent {
    // Override get_info so Ergatai registers you under your actual name
    fn get_info(&self) -> ClientInfo {
        ClientInfo::new(
            ClientCapabilities::default(),
            Implementation::new(self.agent_id.clone(), "1.0.0"),
        )
    }

    // Receive messages from other agents via MCP custom notifications
    async fn on_custom_notification(
        &self, notification: CustomNotification, _ctx: NotificationContext<RoleClient>,
    ) {
        if notification.method == "ergatai/message" {
            let payload = notification.params.unwrap_or_default();
            let from = payload["from_agent"].as_str().unwrap_or("unknown");
            let content = payload["content"].as_str().unwrap_or("(empty)");
            info!("📩 Message from {}: {}", from, content);
        }
    }
}

// Connect
let transport = StreamableHttpClientTransport::from_uri("http://localhost:3000/mcp");
let client = MyAgent::new("my-agent").serve(transport).await?;
```

**That's it!** Ergatai will automatically:
- Register your agent identity (from `clientInfo.name`)
- Enable message delivery via MCP notifications (no HTTP server needed)

### 2. Use Ergatai tools

Your agent can call `list_agents`, `send_message`, `submit_orchestration`, etc.

See `examples/simple-agent/` for a complete working implementation.

## Debugging Tips

### Build Issues

```bash
# Clean build
cargo clean
cargo build --workspace

# Check dependencies
cargo tree | grep <crate-name>

# Verbose build
cargo build --workspace --verbose
```

### Runtime Issues

```bash
# Enable debug logging
RUST_LOG=debug cargo run --bin ergatai-api -- --port 3000

# Enable trace logging
RUST_LOG=trace cargo run --bin ergatai-api -- --port 3000
```

### Test Issues

```bash
# Some tests may fail intermittently when run together (shared global state)
# Run tests individually if you encounter failures
cargo test -p ergatai-core --test <test_name>

# Run with single thread to avoid race conditions
cargo test --workspace -- --test-threads=1
```

## Tech Stack

| Layer | Technology |
|-------|-----------|
| Language | Rust (100%) |
| Agent→Ergatai | MCP (JSON-RPC over Streamable HTTP) |
| MCP Spec | 2025-11-25 |
| Messaging | NATS (async-nats 0.38) + JetStream |
| Database | SQLite (rusqlite 0.31) |
| CLI Framework | clap 4.5 |
| HTTP Server | axum 0.7 |
| Async Runtime | tokio 1.36 |

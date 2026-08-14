# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is this?

**Ergatai** - A multi-agent collaboration middleware for AI-assisted software engineering. Transforms individual AI coding assistants into a coordinated engineering team with parallel task execution, safe concurrent file access, and DAG-based workflow orchestration.

**Pure Rust implementation** using **MCP (Model Context Protocol)** for agent-to-Ergatai communication (tool calls), **ACP (Agent Client Protocol) HTTP** for Ergatai-to-agent push messages, embedded **NATS** event bus for reliable messaging, and **DAG-based orchestration engine** with template-driven data flow.

### Middleware Architecture (Important!)

Ergatai runs as a **middleware** - it does NOT spawn or manage agent processes. Instead:

1. **Agents run independently** and manage their own lifecycle
2. **Agents connect to Ergatai via MCP** to call tools (send_message, list_agents, etc.)
3. **Agents expose ACP HTTP endpoints** so Ergatai can push tasks to them
4. **Agents register their ACP endpoints** via the `set_acp_endpoint` MCP tool

```
┌─────────────────────────────────────────────────────────────────┐
│                      Agent (runs independently)                  │
│  ┌──────────────┐         ┌──────────────────────────────────┐  │
│  │   LLM/API    │         │     ACP HTTP Server              │  │
│  │              │◄────────│  POST /acp/session/:id/prompt    │  │
│  └──────────────┘         └──────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
         │ MCP (tools/call)                    ▲ ACP HTTP
         ▼                                     │
┌─────────────────────────────────────────────────────────────────┐
│                        Ergatai (Middleware)                      │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────────┐  │
│  │  MCP Server  │  │ HTTP Client  │  │   NATS Event Bus     │  │
│  │  (tools)     │  │ (ACP push)   │  │   (JetStream)        │  │
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

# Run tests
cargo test --workspace

# Run specific crate tests
cargo test -p ergatai-core
cargo test -p ergatai-api

# Lint
cargo clippy --workspace -- -D warnings

# Format
cargo fmt --all
```

### Run

```bash
# Run API server (Ergatai middleware)
cargo run --bin ergatai-api -- --port 3000

# Run example agent (in another terminal)
cargo run -p simple-agent -- --port 8080 --agent-id my-agent --ergatai http://localhost:3000

# Run integration test
./tests/integration_test.sh
```

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
├── ergatai-agent/   # Placeholder (was: agent hosting)
├── ergatai-error/   # Error types
└── ergatai-api/     # HTTP/MCP server
    └── src/
        └── mcp/
            ├── agent_registry.rs  # Tracks connected agents
            ├── message_relay.rs   # HTTP ACP client for push
            ├── tools.rs           # MCP tool implementations
            └── server.rs          # MCP JSON-RPC handler
examples/
└── simple-agent/    # Example agent demonstrating middleware usage
```

### Communication Architecture (Critical!)

**Two independent layers:**

| Layer | Protocol | Direction | Purpose |
|-------|----------|-----------|---------|
| **Agent ↔ Ergatai** | ACP (JSON-RPC over stdio) | Bidirectional | Prompts, responses, tool calls, approvals |
| **Ergatai Internal** | NATS (JetStream) | Event stream | Task routing, completion events, file notifications |

**Key point**: Agents never communicate directly. All inter-agent messaging is relayed through Ergatai via NATS.

```
User request: "Refactor this module with 3 agents"
    ↓
CLI generates DAG definition
    ↓
DagScheduler parses → NATS distributes tasks → Sub-agents A/B/C (ACP execution)
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

## Current Status

### ✅ Completed

**Phase 1: Architecture Migration**
- Migrated from Electron/React/TypeScript to pure Rust
- Created workspace structure (core + cli + api)
- Removed all NAPI bindings
- Updated all dependencies

**Phase 2: Core Infrastructure**
- NATS infrastructure + JetStream streams
- ACP protocol integration + session pool management
- DAG orchestration engine + template system
- File access control + token-based locking
- Agent discovery + configuration management

### 🚧 In Progress

**CLI Implementation:**
- Interactive chat interface
- Agent selection and configuration
- Real-time progress display

**Integration Testing:**
- End-to-end multi-agent collaboration scenarios
- CLI → Backend → Agent complete flow

### ❌ Known Issues

**Test Isolation:**
- Some tests may fail intermittently when run together (shared global state)
- Pass when run individually
- Pre-existing issue from original codebase

## Code Statistics

- **Rust**: ~30,000 lines across all crates
- **Tests**: 418+ unit tests
- **Crates**: 3 (ergatai-core, ergatai-cli, ergatai-api)

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
RUST_LOG=debug cargo run --bin ergatai -- chat

# Enable trace logging
RUST_LOG=trace cargo run --bin ergatai -- chat
```

### NATS Issues

```bash
# Check NATS server status
# NATS is embedded, auto-started by ergatai-core

# View NATS logs
# Logs are in {project_root}/.ergatai/logs/
```

## Important Files

### Core Library
- `crates/ergatai-core/src/lib.rs` - Library entry point
- `crates/ergatai-core/src/acp/manager.rs` - ACP session management
- `crates/ergatai-core/src/nats/manager.rs` - NATS global state
- `crates/ergatai-core/src/file_access/lock_manager.rs` - Lock management (largest file)
- `crates/ergatai-core/src/cross_agent/dag_scheduler.rs` - DAG scheduling

### CLI
- `crates/ergatai-cli/src/main.rs` - CLI entry point, command parsing
- `crates/ergatai-cli/src/commands/` - Command implementations

### API Server
- `crates/ergatai-api/src/main.rs` - HTTP/WebSocket server

### Configuration
- `Cargo.toml` - Workspace configuration
- `ARCHITECTURE.md` - Detailed architecture documentation

## Building & Releasing

### Development Build

```bash
cargo build --workspace
```

### Release Build

```bash
cargo build --release --workspace
```

Binaries will be in `target/release/`:
- `ergatai` - CLI binary
- `ergatai-api` - API server binary

### Future: Packaging

```bash
# Will be added for distribution
# cargo install --path crates/ergatai-cli
# cargo install --path crates/ergatai-api
```

## Tech Stack Summary

| Layer | Technology |
|-------|-----------|
| Language | Rust (100%) |
| Agent→Ergatai | MCP (JSON-RPC over HTTP) |
| Ergatai→Agent | ACP HTTP (agent-client-protocol 2.x) |
| Messaging | NATS (async-nats 0.38) + JetStream |
| Database | SQLite (rusqlite 0.31) |
| CLI Framework | clap 4.5 |
| HTTP Server | axum 0.7 |
| Async Runtime | tokio 1.36 |

## Project History

This project underwent a major architecture migration from Electron/React/TypeScript to pure Rust CLI-first architecture. The old version used:
- React frontend (forked from 21st Agents)
- Electron main process
- NAPI-RS bindings for Rust ↔ TypeScript FFI

The new pure Rust architecture provides:
- Better performance (no FFI overhead)
- Simpler deployment (single binary)
- Easier maintenance (one language)
- Better type safety (Rust's type system throughout)

## Current Development Direction

**CLI-First Strategy (v0.x)**

The project is currently focused on developing the **CLI conversational version**. The desktop version is planned for v1.0.0.

**Architecture Design:**
- **ergatai-core**: Core library (shared by all clients)
- **ergatai-cli**: CLI client (current focus)
- **ergatai-api**: API layer (GUI-ready with authentication)
- **Desktop/Web**: v1.0.0 planned clients

**Key priorities for v0.x:**
1. Complete CLI chat interface with permission confirmation UI
2. Improve TUI components (ratatui-based)
3. Enhance agent discovery and configuration
4. Add real-time progress display for DAG execution
5. Stabilize core features for production use

**v1.0.0 Features (planned):**
- Desktop GUI application (Tauri/Electron)
- Visual DAG editor
- Real-time agent collaboration monitoring
- User management and permission system
- Enterprise features

## Recent Security Improvements (2026-08-13)

Comprehensive code security audit and fixes completed:

**Security fixes:**
- API path traversal protection + Bearer token authentication
- Sensitive file detection enhanced (`*.env` patterns + path validation)
- Configuration file permission protection (`0o600`)
- Install command whitelist hardening (shell injection prevention)

**Correctness fixes:**
- NATS zombie process reaping
- Signal handler improvements (proper exit codes)
- Lock manager correctness (new file support, conflict handling)
- CLI command conflicts resolved

**Code quality:**
- Removed hardcoded agent lists (now dynamic)
- Fixed byte slicing panic in skills.rs
- Improved error handling throughout

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

### `set_acp_endpoint`
Register the agent's ACP HTTP endpoint so Ergatai can push tasks to it.

```json
{
  "name": "set_acp_endpoint",
  "arguments": {
    "agent_id": "my-agent",
    "endpoint": "http://localhost:8080"
  }
}
```

### `send_message`
Send a message to another agent via ACP HTTP.

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

### 1. Start an ACP HTTP server

Your agent should expose these endpoints:

```
POST /acp/session/new        → Create a new session, returns {session_id}
POST /acp/session/:id/prompt → Handle a prompt, returns {content: [{type, text}]}
POST /acp/session/:id/close  → Close a session
GET  /health                  → Health check
```

### 2. Connect to Ergatai MCP

Send a JSON-RPC initialize request to Ergatai:

```bash
curl -X POST http://localhost:3000/mcp \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "initialize",
    "params": {
      "protocol_version": "2024-11-05",
      "client_info": {"name": "my-agent", "version": "1.0.0"},
      "capabilities": {}
    }
  }'
```

### 3. Register your ACP endpoint

```bash
curl -X POST http://localhost:3000/mcp \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 2,
    "method": "tools/call",
    "params": {
      "name": "set_acp_endpoint",
      "arguments": {
        "agent_id": "my-agent",
        "endpoint": "http://localhost:8080"
      }
    }
  }'
```

### 4. Use Ergatai tools

Now your agent can call `list_agents`, `send_message`, etc. to collaborate with other agents.

See `examples/simple-agent/` for a complete implementation.

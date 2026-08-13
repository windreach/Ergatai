# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is this?

**Ergatai** - A multi-agent collaboration platform for AI-assisted software engineering. Transforms individual AI coding assistants into a coordinated engineering team with parallel task execution, safe concurrent file access, and DAG-based workflow orchestration.

**Pure Rust implementation** using **ACP (Agent Client Protocol)** for standardized agent communication, embedded **NATS** event bus for reliable messaging, and **DAG-based orchestration engine** with template-driven data flow.

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
cargo test -p ergatai-cli
cargo test -p ergatai-api

# Lint
cargo clippy --workspace -- -D warnings

# Format
cargo fmt --all
```

### Run

```bash
# Run CLI
cargo run --bin ergatai -- chat
cargo run --bin ergatai -- chat --agent claude-code
cargo run --bin ergatai -- agents list
cargo run --bin ergatai -- dag submit workflow.md

# Run API server
cargo run --bin ergatai-api -- --port 3000
```

## Architecture

### Layer Responsibilities

| Layer | Crate | Responsibility |
|-------|-------|----------------|
| **CLI** | `ergatai-cli` | Interactive terminal interface, user interactions |
| **Core** | `ergatai-core` | All business logic, ACP, NATS, file access control |
| **API** | `ergatai-api` | HTTP/WebSocket server for future GUI |

### Workspace Structure

```
crates/
├── ergatai-core/    # Core library (~30,000 lines)
│   ├── src/
│   │   ├── acp/          # ACP protocol layer
│   │   ├── nats/         # NATS messaging system
│   │   ├── orchestration/# DAG orchestration engine
│   │   ├── cross_agent/  # Multi-agent collaboration
│   │   ├── file_access/  # File locking and access control
│   │   └── agent/        # Agent discovery and configuration
│   └── Cargo.toml
├── ergatai-cli/     # CLI binary
│   ├── src/
│   │   ├── main.rs       # Entry point, command parsing
│   │   ├── commands/     # Command handlers
│   │   └── ui/           # TUI components
│   └── Cargo.toml
└── ergatai-api/     # API server
    ├── src/
    │   └── main.rs       # HTTP/WebSocket endpoints
    └── Cargo.toml
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
| Agent Protocol | ACP (agent-client-protocol 2.x) |
| Messaging | NATS (async-nats 0.38) + JetStream |
| Database | SQLite (rusqlite 0.31) |
| CLI Framework | clap 4.5 |
| TUI | ratatui 0.26 + crossterm 0.27 |
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

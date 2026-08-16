# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-08-16

### Added

- **MCP Server** — Streamable HTTP MCP server for agent connections (`ergatai-api`)
  - Agent auto-registration on MCP initialize
  - Tool implementations: `list_agents`, `send_message`, `submit_orchestration`, `check_dag_status`
  - MCP custom notification push (`ergatai/message`) for agent messaging
- **tmux Injection** — Direct message delivery by writing into agent tmux panes (`ergatai-collab::tmux`)
  - Preferred delivery method over MCP notifications
  - Fallback to MCP notifications when tmux unavailable
- **NATS Event Bus** — Embedded NATS server with JetStream (`ergatai-nats`)
  - Typed event publishing: task submit/complete, DAG node/dag complete
  - Agent message routing via NATS subjects
  - WorkQueue retention for task distribution
- **DAG Orchestration** — Template-driven workflow engine (`ergatai-dag`)
  - Markdown-based DAG definition parsing
  - Template variable rendering (`{{global.*}}`, `{{node_id.*}}`)
  - Topology validation and cycle detection
- **File Access Control** — Token-based locking system (`ergatai-lock`)
  - Two-level tokens: `SystemToken` (session) + `FileToken` (operation)
  - READ/WRITE/ADMIN lock modes
  - Watchdog heartbeat monitoring with progressive timeout
  - Automatic git snapshot before file writes
  - Conflict arbitration for concurrent access
  - Audit logging for all lock operations
- **Multi-Agent Collaboration** — Task coordination and scheduling (`ergatai-collab`)
  - `DagScheduler` — Orchestrates DAG workflow execution
  - `TaskScheduler` — Manages individual task lifecycle
  - `AgentLauncher` — Agent process management via pluggable backends
  - `TaskCoordinator` — Cross-agent task dependency tracking
- **Pluggable Runtime Backends** (`ergatai-runtime`)
  - `direct_process` — Spawn agents as child processes
  - `local_pty` — Local PTY-based agent management
  - `rmux` — tmux-based agent backend
- **Example Agent** — Working MCP client example (`examples/simple-agent`)
  - Demonstrates agent registration, messaging, and orchestration submission

### Architecture

- Pure Rust workspace with 9 modular crates
- Middleware design: agents manage own lifecycle, connect via MCP
- No HTTP server needed on agent side
- NATS JetStream for reliable internal messaging
- SQLite for lock persistence (`.ergatai/locks.db`)

### Infrastructure

- Apache-2.0 license
- Rust 2021 edition
- async-nats 0.38, axum 0.7, tokio 1.36, rmcp 3.1.2

[0.1.0]: https://github.com/windreach/Ergatai/releases/tag/v0.1.0

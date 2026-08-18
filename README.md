# Ergatai

<div align="center">

<img src="assets/logo.jpeg" alt="Ergatai Logo" width="200">

**Multi-Agent Collaboration Middleware**

*Tell your AI agents what to do — together.*

</div>

<div align="center">

[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
&ensp;
[![Rust](https://img.shields.io/badge/rust-2021-orange.svg)](https://www.rust-lang.org)
&ensp;
[![Docs](https://img.shields.io/badge/docs-CLAUDE.md-blue.svg)](CLAUDE.md)
&ensp;
[![Contributing](https://img.shields.io/badge/contributions-welcome-brightgreen.svg)](#contributing)

</div>

<br/>

# What can Ergatai do?

Ergatai connects AI agents so they can work together on complex tasks — like a team of specialists coordinating on a project. You describe what needs to be done, and Ergatai routes the work across agents.

### 🔄 Agent-to-Agent Messaging

Agents send and receive messages through Ergatai's relay. Claude Code can ask Cursor to refactor a module, and Codex can report results back.

```
User: "Claude, ask Cursor to write unit tests for src/auth.rs"
Claude: [calls send_message → target: cursor]
Cursor: [runs tests, reports back]
Claude: "Tests written. All 12 passing."
```

### 📋 DAG-Based Task Orchestration

Submit multi-step workflows where different agents handle different phases — with dependencies between tasks.

```markdown
## Task A — Code Analysis
- agent: claude-code
- task: Review all changes in PR #42 and identify risky modules

## Task B — Test Writing
- agent: cursor
- task: Write unit tests for modules identified in Task A
- depends_on: [Task A]

## Task C — Security Review
- agent: codex
- task: Run security audit on changed files
- depends_on: [Task A]
```

###  Safe Concurrent File Access

When multiple agents edit the same codebase, Ergatai prevents conflicts with token-based locking, automatic git snapshots, and heartbeat monitoring.

<br/>

# Quickstart

**1. Build and start the server:**

```bash
cargo build --release -p ergatai-api
./target/release/ergatai-api --port 3000
```

**2. Add Ergatai to your agent's MCP config:**

Each agent gets its own URL path so Ergatai can bind MCP connections to specific rmux panes:

```json
{
  "mcpServers": {
    "ergatai": {
      "url": "http://localhost:3000/mcp/agent-1"
    }
  }
}
```

Available paths: `/mcp/agent-1`, `/mcp/agent-2`, `/mcp/agent-3`, plus `/mcp` as a shared fallback.

**3. Start collaborating** — agents auto-register on connect and can immediately call `list_agents`, `register_agent_name`, `send_message`, `submit_orchestration`, and `check_dag_status`.

📖 See [MCP Configuration Guide](docs/MCP_CONFIG_GUIDE.md) for full details.

<br/>

# Architecture

```
  ┌──────────────────────────────────────────────────────────────┐
  │                       Agents                                 │
  │                                                              │
  │     Claude Code   │   Cursor   │   Codex   │    ...         │
  └────────┬──────────────────┬─────────────┬───────────────────┘
           │                  │             │
           │ MCP              │ MCP         │ MCP
           │ tools/call       │ tools/call  │ tools/call
           ▼                  ▼             ▼
  ┌──────────────────────────────────────────────────────────────┐
  │                    Ergatai Middleware                        │
  │                                                              │
  │  ┌────────────────────────────────────────────────────────┐ │
  │  │                 Protocol Layer                          │ │
  │  │                                                         │ │
  │  │    ┌───────────────┐                ┌───────────────┐  │ │
  │  │    │  MCP Server   │                │   rmux        │  │ │
  │  │    │               │   notify       │   injector    │  │ │
  │  │    │  • JSON-RPC   │◄──────────────►│               │  │ │
  │  │    │  • SSE stream │                │  • send_text  │  │ │
  │  │    │  • Tools API  │                │  • Pane write │  │ │
  │  │    └───────┬───────┘                └───────┬───────┘  │ │
  │  └────────────┼────────────────────────────────┼──────────┘ │
  │               │                                │            │
  │  ┌────────────┴────────────────────────────────┴──────────┐ │
  │  │                 Application Layer                       │ │
  │  │                                                         │ │
  │  │    ┌──────────────┐ ┌──────────────┐ ┌──────────────┐  │ │
  │  │    │Agent Registry│ │ DAG Scheduler│ │ File Access  │  │ │
  │  │    │              │ │              │ │  Control     │  │ │
  │  │    │ • Discover   │ │ • Parse DAG  │ │              │  │ │
  │  │    │ • Heartbeat  │ │ • Resolve    │ │ • Locks      │  │ │
  │  │    │ • Reap stale │ │ • Schedule   │ │ • Snapshot   │  │ │
  │  │    │              │ │ • Template   │ │ • Conflict   │  │ │
  │  │    └──────────────┘ └──────────────┘ └──────────────┘  │ │
  │  └──────────────────────────┬─────────────────────────────┘ │
  │                             │                               │
  │  ┌──────────────────────────┴─────────────────────────────┐ │
  │  │                  Event Bus Layer                        │ │
  │  │                                                         │ │
  │  │         ┌──────────────────────────────────┐           │ │
  │  │         │      NATS  +  JetStream           │           │ │
  │  │         │                                    │           │ │
  │  │         │  TASK_QUEUE │ FILE_ACCESS          │           │ │
  │  │         │  FILE_EVENTS│ LOCK_WAITERS         │           │ │
  │  │         └──────────────────────────────────┘           │ │
  │  └────────────────────────────────────────────────────────┘ │
  └──────────────────────────┬──────────────────────────────────┘
                             │
               ┌─────────────┴─────────────┐
               ▼                           ▼
     ┌───────────────────┐      ┌─────────────────────┐
     │  Shared Codebase  │      │  SQLite Database    │
     │  (with file lock) │      │  .ergatai/*.db      │
     └───────────────────┘      └─────────────────────┘
```

### Dual Protocol Stack

Ergatai uses two independent protocols for bidirectional communication:

| Direction | Protocol | Purpose |
|-----------|----------|---------|
| **Agent → Ergatai** | MCP (Streamable HTTP) | Agents call tools: `list_agents`, `send_message`, `submit_orchestration`, `check_dag_status` |
| **Ergatai → Agent** | rmux pane injection | Ergatai delivers messages by injecting text directly into the agent's rmux pane via `pane.send_text()` |

**Agent registration flow:**
1. Agent connects to Ergatai via MCP (as MCP client)
2. Agent calls tools (e.g. `list_agents`, `send_message`) to interact with Ergatai
3. Ergatai pushes tasks/messages back by injecting text into the agent's rmux pane

**Key point**: Agents do NOT need to expose any incoming endpoint. Ergatai delivers messages by injecting text into the agent's rmux pane, simulating keyboard input. Agent identity is deterministically bound to panes via the `RMUX_PANE` environment variable.

### DAG Orchestration Flow

The core value of Ergatai — parallel multi-agent workflows with dependencies:

```
  ① Submit                 ② Parse & Resolve           ③ Parallel Execution
  ────────                 ─────────────────           ────────────────────

  ┌──────────────┐         ┌──────────────┐
  │  Markdown    │         │   DAG        │
  │  Definition  │────────▶│   Engine     │
  │              │         │              │
  │  ## Task A   │         │  validate    │
  │  ## Task B   │         │  resolve     │
  │  ## Task C   │         │  deps        │
  │  depends_on  │         └──────┬───────┘
  └──────────────┘                │
                                  ▼
                         ┌────────────────┐
                         │   Scheduler    │
                         │                │
                         │  A ──┬──▶ B    │
                         │   └───▶ C      │
                         └────────────────┘

  ─────────────────────────────────────────────────────────────────────

  Task Dependency Graph                    Execution Timeline
  ─────────────────────                    ──────────────────

       ┌──────────┐                        Time ──────────────────────▶
       │  Task A  │
       │ (Claude) │                        A: ████████  done
       └────┬─────┘
            │                               B: ·······██████  done
       ┌────┴─────┐
       │          │                         C: ·······██████████  done
       ▼          ▼
  ┌─────────┐ ┌─────────┐
  │ Task B  │ │ Task C  │                  Template Data Flow
  │(Cursor) │ │(Codex)  │                  ──────────────────
  └─────────┘ └─────────┘
                                        TaskA.output ──▶ TaskB.input
                                        TaskA.output ──▶ TaskC.input
                                        {{TaskA.review_result}} rendered
                                        at schedule time
```

<br/>

# Features

| Capability | Description |
|------------|-------------|
| **Agent-to-Agent Messaging** | Agents send and receive messages through Ergatai's relay |
| **DAG Orchestration** | Submit markdown-formatted workflows with task dependencies |
| **Safe Concurrency** | Token-based READ/WRITE/ADMIN locks with automatic reclamation |
| **Agent Discovery** | Automatic registration when agents connect via MCP |
| **Agent Agnostic** | Works with any MCP-compatible agent — Claude, Cursor, Codex, and more |
| **Local First** | All execution runs locally; no data leaves your machine |
| **Crash Recovery** | Heartbeat monitoring reclaims stale locks automatically |
| **TLS Support** | Optional HTTPS with rustls for secure deployments |
| **Rate Limiting** | Per-agent rate limits prevent spam and runaway loops |
| **Prometheus Metrics** | `/metrics` endpoint for observability |

<br/>

# MCP Tools

Ergatai exposes five tools to connected agents:

### `list_agents`

List all connected agents and their current status.

**Input:**
```json
{ "include_capabilities": true }
```

**Response:**
```json
{
  "agents": [
    {
      "agent_id": "%15",
      "display_name": "alice",
      "mcp_agent_id": "opencode@a1b2c3d4",
      "workspace_id": "workspace-1",
      "status": "active",
      "is_self": false
    }
  ],
  "total": 1
}
```

### `register_agent_name`

Register a human-readable display name for the calling agent. Once registered, other agents can target this name in `send_message` instead of the auto-generated runtime ID (e.g., `%15`). Names must be unique across all connected agents.

**Input:**
```json
{ "display_name": "alice" }
```

**Validation:** non-empty, max 64 characters, alphanumeric plus `-`, `_`, `/`.

### `send_message`

Send a message to another registered agent.

**Input:**
```json
{
  "target_agent_id": "cursor",
  "message": "Please refactor src/auth.rs to use the new middleware pattern",
  "message_type": "request"
}
```

**Response:**
```json
{
  "status": "queued",
  "target_agent": "cursor",
  "delivery_method": "nats_jetstream",
  "stream": "AGENT_MESSAGES",
  "sequence": 42,
  "note": "Message persisted to NATS JetStream. Background consumer will deliver via rmux injection."
}
```

### `submit_orchestration`

Submit a DAG workflow for multi-agent parallel execution.

**Input:**
```json
{
  "dag_definition": "## Task A\n- agent: claude-code\n- task: Analyze codebase structure\n\n## Task B\n- agent: cursor\n- task: Write unit tests\n- depends_on: [Task A]",
  "context": {
    "project": "my-app",
    "branch": "main"
  }
}
```

### `check_dag_status`

Check the execution progress of a submitted DAG.

**Input:**
```json
{ "dag_id": "dag-abc123" }
```

<br/>

# Supported Agents

Ergatai works with any agent that implements the MCP protocol:

| Agent | Status | Notes |
|-------|--------|-------|
| Claude Code | ✅ Verified | Native MCP support |
| Cursor | ✅ Verified | IDE-integrated agent |
| Codex | ✅ Supported | OpenAI CLI agent |
| Goose | ✅ Supported | Block's AI assistant |
| Cline | ✅ Supported | VS Code extension |
| Aider | ✅ Supported | Terminal coding agent |
| Custom Agents | ✅ Supported | Any MCP-compatible runtime |

<br/>

# Project Structure

```
ergatai/
├── crates/
│   ├── ergatai-api/       # MCP server + REST API (main entry point)
│   ├── ergatai-runtime/   # Agent runtime (discovery, injection, lifecycle)
│   ├── ergatai-nats/      # Embedded NATS server + JetStream streams
│   ├── ergatai-collab/    # Multi-agent collaboration (DAG scheduling)
│   ├── ergatai-dag/       # DAG parser, scheduler, dependency resolution
│   ├── ergatai-lock/      # Token-based file access control
│   ├── ergatai-agent/     # Agent config, discovery, hosted agents
│   ├── ergatai-core/      # Core library — business logic facade
│   ├── ergatai-error/     # Shared error types
│   ├── ergatai-binary/    # Binary resources (rmux, nats-server)
│   └── ergatai-cli/       # CLI tool
├── examples/
│   └── simple-agent/      # Minimal MCP agent example
├── Cargo.toml
├── CLAUDE.md
└── README.md
```

<br/>

# Development

### Build

```bash
cargo build --workspace
cargo build --release --workspace
cargo build -p ergatai-api          # single crate
```

### Run

```bash
cargo run -p ergatai-api -- --port 3000
RUST_LOG=debug cargo run -p ergatai-api -- --port 3000
ERGATAI_API_TOKEN=secret cargo run -p ergatai-api -- --port 3000
```

### Tests & Lint

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all
```

<br/>

# Tech Stack

| Component | Technology | Version |
|-----------|-----------|---------|
| Language | Rust | 2021 edition |
| MCP Protocol | Streamable HTTP | 2025-06-18 |
| Messaging | async-nats + JetStream | 0.38 |
| Database | rusqlite (embedded SQLite) | 0.31 |
| HTTP Server | axum | 0.7 |
| TLS | rustls (via axum-server) | — |
| Async Runtime | tokio | 1.36 |
| Serialization | serde + serde_json | 1.0 |
| CLI | clap | 4.5 |
| Observability | Prometheus (metrics crate) | — |

<br/>

# File Access Control

When multiple agents collaborate on the same repository, Ergatai ensures safe concurrent writes:

- **Token-based locking** — READ / WRITE / ADMIN modes with explicit acquire/release
- **Heartbeat monitoring** — Stale locks reclaimed after 90s of inactivity
- **Git snapshots** — Automatic pre-write snapshots enable rollback on conflict
- **Conflict arbitration** — Priority-based resolution when two agents contend for the same file
- **Audit logging** — Every lock acquire, release, and conflict is logged

<br/>

# FAQ

<details>
<summary><b>Should I use Ergatai or wire agents directly?</b></summary>

Use Ergatai when you have 2+ agents that need to coordinate — it removes the need for each agent to know about every other agent. Use direct wiring only for simple point-to-point communication.
</details>

<details>
<summary><b>Can I use this with my own custom agent?</b></summary>

Yes. As long as your agent implements the MCP protocol (can connect as an MCP client), Ergatai will discover it automatically and enable messaging. See `examples/simple-agent/` for a complete working implementation.
</details>

<details>
<summary><b>Does Ergatai send data to the cloud?</b></summary>

No. Ergatai runs entirely locally. NATS, the MCP server, and all message forwarding happen on your machine. No telemetry, no phoning home.
</details>

<details>
<summary><b>How does Ergatai handle agent crashes?</b></summary>

The agent reaper checks heartbeats every 30s. If an agent goes silent for 90s, it is marked disconnected and its locks are released. Pending messages to that agent are queued until it reconnects.
</details>

<details>
<summary><b>What's the difference between the API server and the core library?</b></summary>

`ergatai-api` is the standalone MCP server (the entry point you run). `ergatai-core` is the reusable library — if you want to embed Ergatai's collaboration logic inside another application, depend on `ergatai-core`.
</details>

<br/>

# Security

Security audit completed 2026-08-19 (v0.1.0):

- ✅ API path traversal protection
- ✅ Bearer token authentication (`--api-token`)
- ✅ Sensitive file detection enhanced
- ✅ Configuration file permission hardening (`0o600`)
- ✅ Install command whitelist (shell injection prevention)
- ✅ NATS zombie process cleanup
- ✅ Signal handler for graceful shutdown
- ✅ Lock manager correctness fixes
- ✅ Rate limiting per agent (1 req/s, burst 20)

<br/>

# Contributing

Contributions are welcome! Please read [CONTRIBUTING.md](CONTRIBUTING.md) before submitting a pull request.

<br/>

# License

This project is licensed under the Apache License 2.0 — see [LICENSE](LICENSE) for details.

<br/>

<div align="center">

**Tell your AI agents what to do, and they get it done — together.**

</div>

<div align="center"> Made with ❤️ by the Ergatai team </div>

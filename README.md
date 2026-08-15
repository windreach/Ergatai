# Ergatai

> Tell your AI agents what to do — together.

Ergatai is a **multi-agent collaboration middleware** that lets AI agents communicate and coordinate on tasks. It acts as a message broker, relaying messages between agents via MCP (Model Context Protocol) — agents connect as MCP clients and receive messages via custom notifications.

Pure Rust. Local-first. No cloud dependencies. No HTTP server needed on the agent side.

[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2021-orange.svg)](https://www.rust-lang.org)
[![Docs](https://img.shields.io/badge/docs-latest-blue.svg)](#architecture)
[![Contributing](https://img.shields.io/badge/contributions-welcome-brightgreen.svg)](#contributing)

---

## What can Ergatai do?

Ergatai connects AI agents so they can work together on complex tasks — like a team of specialists coordinating on a project. You describe what needs to be done, and Ergatai routes the work across agents.

### 🔄 Agent-to-Agent Communication

Agents can send messages to each other seamlessly. Claude Code can ask Cursor to refactor a module, and Codex can report results back.

```
User: "Claude, ask Cursor to write unit tests for src/auth.rs"
Claude: [forwards to Cursor via Ergatai]
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

### 🔒 Safe Concurrent File Access

When multiple agents edit the same codebase, Ergatai prevents conflicts with token-based locking, automatic git snapshots, and heartbeat monitoring.

---

## Quickstart

### 1. Build and Start the Server

```bash
cargo build --release -p ergatai-api
./target/release/ergatai-api --port 3000
```

MCP endpoint: `http://localhost:3000/mcp`

### 2. Configure Your Agent

**Claude Code** — add to `~/.claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "ergatai": {
      "url": "http://localhost:3000/mcp"
    }
  }
}
```

**Cursor** — add to `.cursor/mcp.json`:

```json
{
  "mcpServers": {
    "ergatai": {
      "url": "http://localhost:3000/mcp"
    }
  }
}
```

📖 See [MCP Configuration Guide](docs/MCP_CONFIG_GUIDE.md) for full details.

### 3. Start Collaborating

Once connected, agents can use Ergatai tools:

```
User: List all connected agents
Claude: [calls list_agents]
Claude: Connected: claude-code (active), cursor (active)

User: Send a message to cursor
Claude: [calls send_message → target: cursor]
Claude: Message delivered. Session reused from previous exchange.
```

---

## Architecture

```
┌──────────────┐    MCP     ┌──────────────┐    MCP      ┌──────────────┐
│   Agent A    │ ←────────→ │   Ergatai    │ ←─────────→ │   Agent B    │
│  (Claude)    │  tools +   │  MCP Server  │  tools +    │   (Cursor)   │
│              │  notify    │              │  notify     │              │
└──────────────┘            └──────┬───────┘             └──────────────┘
                                   │
                              ┌────▼─────┐
                              │  NATS    │
                              │ JetStream│
                              └────┬─────┘
                                   │
                              ┌────▼─────┐
                              │  Agent C  │
                              │ (Codex)  │
                              └──────────┘
```

### Protocol Stack

| Layer | Protocol | Direction | Purpose |
|-------|----------|-----------|---------|
| **Agent → Ergatai** | MCP (Streamable HTTP) | Inbound | Agents connect as MCP clients; call tools (send_message, list_agents, etc.) |
| **Ergatai → Agent** | MCP Custom Notification | Outbound | Ergatai pushes messages via `ergatai/message` notification over the existing MCP connection |
| **Internal** | NATS + JetStream | Event bus | Task routing, completion events, file change notifications |

### How It Works

1. **Agent Registration** — When an agent connects via MCP, it is automatically discovered and registered
2. **Message Relay** — Agent A calls `send_message` via MCP; Ergatai pushes an `ergatai/message` notification to Agent B over its existing MCP connection
3. **Orchestration** — Submit a DAG definition; Ergatai parses dependencies and schedules tasks across agents
4. **Conflict Prevention** — Token-based file locks ensure concurrent edits never corrupt the codebase

---

## Features

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

---

## MCP Tools

Ergatai exposes four tools to connected agents:

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
      "agent_id": "claude-code",
      "status": "active",
      "capabilities": ["chat", "code", "tools"],
      "connected_at": "2026-08-14T10:00:00Z",
      "last_heartbeat": "2026-08-14T10:05:00Z"
    }
  ],
  "total": 1
}
```

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
  "message_id": "msg-a1b2c3",
  "status": "delivered",
  "delivery_method": "mcp_notification",
  "target_agent_id": "cursor",
  "message_type": "request"
}
```

### `submit_orchestration`

Submit a DAG workflow for multi-agent parallel execution.

**Input:**
```json
{
  "dag_definition": "## Task A\n- agent: claude-code\n- task: Analyze codebase structure\n\n## Task B\n- agent: cursor\n- task: Write unit tests\n- depends_on: [Task A]\n\n## Task C\n- agent: codex\n- task: Run security scan\n- depends_on: [Task A]",
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

---

## Supported Agents

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

---

## Project Structure

```
ergatai/
├── crates/
│   ├── ergatai-core/      # Core library — business logic, agents, NATS, DAG
│   ├── ergatai-api/       # MCP server + REST API (main entry point)
│   │   └── src/
│   │       ├── main.rs             # Server bootstrap, routes, middleware
│   │       └── mcp/
│   │           ├── server.rs       # MCP Streamable HTTP server
│   │           ├── tools.rs        # MCP tool handlers (list/send/submit/check)
│   │           ├── agent_registry.rs # Agent discovery and tracking
│   │           └── message_relay.rs  # NATS → ACP forwarding
│   ├── ergatai-acp/       # Agent Client Protocol implementation
│   ├── ergatai-dag/       # DAG parser, scheduler, and dependency resolution
│   ├── ergatai-lock/      # Token-based file access control
│   ├── ergatai-nats/      # Embedded NATS server + JetStream streams
│   ├── ergatai-agent/     # Agent config, discovery, and hosted agents
│   ├── ergatai-collab/    # Cross-agent collaboration primitives
│   └── ergatai-error/     # Shared error types
├── docs/
│   ├── MCP_CONFIG_GUIDE.md
│   └── ACP_SDK_GUIDE.md
├── examples/
│   └── simple-agent/      # Minimal MCP agent example
├── Cargo.toml
└── README.md
```

---

## Development

### Build

```bash
# Build everything
cargo build --workspace

# Release build
cargo build --release --workspace

# Single crate
cargo build -p ergatai-api
```

### Run

```bash
# Development
cargo run -p ergatai-api -- --port 3000

# Verbose logging
RUST_LOG=debug cargo run -p ergatai-api -- --port 3000

# With authentication token
ERGATAI_API_TOKEN=secret cargo run -p ergatai-api -- --port 3000
```

### Tests

```bash
cargo test --workspace
cargo test --workspace -- --nocapture   # with output
```

### Code Quality

```bash
cargo clippy --workspace -- -D warnings
cargo fmt --all
cargo fmt --all -- --check
```

---

## Tech Stack

| Component | Technology | Version |
|-----------|-----------|---------|
| Language | Rust | 2021 edition |
| MCP Protocol | Streamable HTTP | 2025-11-25 |
| Messaging | async-nats + JetStream | 0.38 |
| Database | rusqlite (embedded SQLite) | 0.31 |
| HTTP Server | axum | 0.7 |
| TLS | rustls (via axum-server) | — |
| Async Runtime | tokio | 1.36 |
| Serialization | serde + serde_json | 1.0 |
| CLI | clap | 4.5 |
| Observability | Prometheus (metrics crate) | — |

---

## File Access Control

When multiple agents collaborate on the same repository, Ergatai ensures safe concurrent writes:

- **Token-based locking** — READ / WRITE / ADMIN modes with explicit acquire/release
- **Heartbeat monitoring** — Stale locks are reclaimed after 90s of inactivity
- **Git snapshots** — Automatic pre-write snapshots enable rollback on conflict
- **Conflict arbitration** — Priority-based resolution when two agents contend for the same file
- **Audit logging** — Every lock acquire, release, and conflict is logged for traceability

---

## FAQ

**Should I use Ergatai or wire agents directly?**

Use Ergatai when you have 2+ agents that need to coordinate — it removes the need for each agent to know about every other agent. Use direct wiring only for simple point-to-point communication.

**Can I use this with my own custom agent?**

Yes. As long as your agent implements the MCP protocol (can connect as an MCP client), Ergatai will discover it automatically and enable messaging.

**Does Ergatai send data to the cloud?**

No. Ergatai runs entirely locally. NATS, the MCP server, and all message forwarding happen on your machine. No telemetry, no phoning home.

**How does Ergatai handle agent crashes?**

The agent reaper checks heartbeats every 30s. If an agent goes silent for 90s, it is marked disconnected and its locks are released. Pending messages to that agent are queued until it reconnects.

**Can I add custom MCP tools?**

Yes. Ergatai's tool handlers live in `crates/ergatai-api/src/mcp/tools.rs`. You can extend them with your own logic, or fork the server and add new tools following the same pattern.

**What's the difference between the API server and the core library?**

`ergatai-api` is the standalone MCP server (the entry point you run). `ergatai-core` is the reusable library — if you want to embed Ergatai's collaboration logic inside another application, you depend on `ergatai-core`.

---

## Roadmap

### v0.1.0 — Current

- [x] MCP server with Streamable HTTP transport
- [x] Agent auto-registration on connect
- [x] Message relay via MCP custom notifications
- [x] Agent discovery and heartbeat
- [x] Drop-based cleanup (stale agent removal)
- [x] DAG orchestration engine
- [x] Token-based file locking
- [x] Prometheus metrics
- [x] TLS support
- [ ] End-to-end integration tests
- [ ] Web dashboard

### Near-term

- [ ] Enhanced error reporting across agent boundaries
- [ ] Persistent message queue (retry on reconnect)
- [ ] Plugin system for custom tool handlers
- [ ] Multi-workspace support

### Future

- [ ] GUI dashboard for monitoring agent activity
- [ ] Agent performance analytics
- [ ] Cloud-hosted mode (optional remote NATS)
- [ ] Enterprise features (SAML, audit export)

---

## Security

Security improvements (2026-08-14):

- ✅ API path traversal protection
- ✅ Bearer token authentication (`--api-token`)
- ✅ Sensitive file detection
- ✅ Configuration file permission hardening (`0o600`)
- ✅ Install command whitelist
- ✅ NATS zombie process cleanup
- ✅ Signal handler for graceful shutdown
- ✅ Lock manager correctness fixes
- ✅ Rate limiting per agent (1 req/s, burst 20)

---

## Contributing

Contributions are welcome! Please read [CONTRIBUTING.md](CONTRIBUTING.md) before submitting a pull request.

---

## License

This project is licensed under the Apache License 2.0 — see [LICENSE](LICENSE) for details.

---

## Citation

If you use Ergatai in your research or project, please cite:

```bibtex
@software{ergatai2026,
  author = {Ergatai Team},
  title = {Ergatai: Multi-Agent Collaboration Middleware},
  year = {2026},
  publisher = {GitHub},
  url = {https://github.com/ergatai/ergatai}
}
```

---

Made with ❤️ by the Ergatai team

# 🚀 Ergatai v0.1.0 — Initial Release

**Multi-agent collaboration middleware** that organizes independent AI coding assistants (Claude Code, Cursor, Codex, OpenCode, etc.) into collaborative teams via rmux pane injection.

## ✨ Core Features

### Agent-to-Agent Messaging
Agents send and receive messages through Ergatai's relay. Messages are persisted to NATS JetStream for reliable delivery, then injected into the target agent's rmux pane as if typed by a human.

### DAG-Based Task Orchestration
Submit markdown-formatted workflows with dependencies — Ergatai parses them, resolves the DAG, schedules tasks in parallel, and propagates outputs between tasks.

### Safe Concurrent File Access
Token-based file locking (READ/WRITE/ADMIN modes) with kernel-level enforcement via Linux fanotify. Automatic git snapshots enable rollback on conflict.

### Agent Discovery
Automatic registration via rmux pane scan. Agents are deterministically identified by the `RMUX_PANE` environment variable (e.g., `%15`, `%16`).

### Human-Readable Agent Names
New `register_agent_name` MCP tool lets agents claim a display name (e.g., `alice`) — other agents can target this name in `send_message` instead of the auto-generated ID.

### Conversation Loop Prevention
AutoGen-style token-based turn-taking: one-question-one-answer between agent pairs, with automatic restart after `max_turns` and `TERMINATE` keyword support.

## 🔧 MCP Tools

| Tool | Purpose |
|------|---------|
| `list_agents` | List connected agents and their status |
| `register_agent_name` | Claim a human-readable display name |
| `send_message` | Send a message to another agent |
| `submit_orchestration` | Submit a DAG workflow |
| `check_dag_status` | Check DAG execution progress |

## 🏗️ Architecture

- **11 Rust crates**, ~45,000 lines of code
- **MCP protocol 2025-06-18** (Streamable HTTP transport)
- **async-nats 0.38** + JetStream (7 streams: AGENT_MESSAGES, FILE_ACCESS_*, FILE_EVENTS, DAG_EVENTS, LOCK_WAITERS)
- **axum 0.7** HTTP server with optional TLS (rustls)
- **rusqlite 0.31** embedded SQLite for state
- **rmux** tmux-compatible daemon for pane management

## 🔐 File Locking Permissions (IMPORTANT)

> ⚠️ **Kernel-level mandatory file locking requires `CAP_SYS_ADMIN`** (Linux fanotify with `FAN_OPEN_PERM` events).

Without this capability, Ergatai falls back to **advisory-only mode** — agent locks are cooperative and can be bypassed by direct shell access.

### Install (recommended)

```bash
curl -sSL https://raw.githubusercontent.com/windreach/Ergatai/main/install.sh | bash
```

The install script:
- Downloads the binary from this release
- Installs to `/usr/local/bin`
- Grants `CAP_SYS_ADMIN` via `setcap`
- Verifies the capability is set

### Manual install

```bash
curl -L -o ergatai-api https://github.com/windreach/Ergatai/releases/download/v0.1.0/ergatai-api-x86_64
chmod +x ergatai-api
sudo mv ergatai-api /usr/local/bin/
sudo setcap 'cap_sys_admin+ep' /usr/local/bin/ergatai-api
```

See the [File Locking Permissions](https://github.com/windreach/Ergatai#file-locking-permissions) section in the README for details.

## 🔌 MCP Configuration

Each agent connects to Ergatai via its own URL path. The **path suffix is the agent's conversation name** — used for messaging, discovery, and rmux binding. Names must be **unique** per agent.

```json
{
  "mcpServers": {
    "ergatai": {
      "url": "http://localhost:3000/mcp/alice"
    }
  }
}
```

> ⚠️ **Do not reuse agent names.** If two agents connect to the same name, they share session state and their messages will collide.

## ✅ Verification

- **L1 tests**: `cargo build`, 1007 tests, `clippy -D warnings` — all green
- **L2 smoke test**: end-to-end with 3 simple-agent instances
  - MCP connect → rmux bind → list_agents → send_message → delivery
- **Code review**:
  - 0 CRITICAL findings
  - 0 HIGH findings (1 memory leak fixed before release)
  - 3 MEDIUM findings (defense-in-depth, deferred to v0.1.1)
- **Security audit**: all unsafe blocks have SAFETY comments, SQL queries parameterized, path traversal prevented

## 📦 What's in the box

| File | Description |
|------|-------------|
| `ergatai-x86_64` | Linux x86_64 CLI binary (manages workspaces/agents, wraps rmux) |
| `ergatai-api-x86_64` | Linux x86_64 server binary (MCP/HTTP API, needs `CAP_SYS_ADMIN` for file locking) |
| `ergatai-aarch64` | Linux ARM64 CLI binary |
| `ergatai-api-aarch64` | Linux ARM64 server binary |
| `install.sh` | One-liner installer — downloads both binaries and sets capabilities |

> **Note:** The install script installs both `ergatai` (CLI) and `ergatai-api` (server). Only the server needs `CAP_SYS_ADMIN` (for fanotify file locking); the CLI runs without special privileges.

## 🚦 Quickstart

```bash
# 1. Install (auto-grants CAP_SYS_ADMIN)
curl -sSL https://raw.githubusercontent.com/windreach/Ergatai/main/install.sh | bash

# 2. Start the server
ergatai-api --port 3000

# 3. Configure agents to connect to /mcp/<agent-name>
# 4. Agents can now call: list_agents, register_agent_name, send_message, etc.
```

## 📖 Documentation

- [README](https://github.com/windreach/Ergatai#readme)
- [CLAUDE.md](https://github.com/windreach/Ergatai/blob/main/CLAUDE.md) — agent-facing docs
- [examples/simple-agent/](https://github.com/windreach/Ergatai/tree/main/examples/simple-agent) — minimal MCP agent example

## 🙏 Acknowledgments

Built with Rust, async-nats, axum, rmux, and the MCP protocol.

---

**Full changelog**: https://github.com/windreach/Ergatai/commits/v0.1.0

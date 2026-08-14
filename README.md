# Ergatai

> ⚠️ **Project under active development**

Ergatai is a **multi-agent collaboration middleware** that enables AI agents to communicate and work together seamlessly. It acts as a message broker, relaying messages between agents via MCP (Model Context Protocol) and ACP (Agent Client Protocol).

**Pure Rust implementation** focused on performance and security. Agents connect via **MCP** to send messages, and Ergatai uses **ACP** to forward messages to other agents, enabling seamless agent-to-agent collaboration without direct dependencies.

## 🚀 Quick Start

### 1. Start Ergatai MCP Server

```bash
# Build
cargo build --release -p ergatai-api

# Start the server
./target/release/ergatai-api --port 3000
```

MCP endpoint: `http://localhost:3000/mcp`

### 2. Configure Your Agent

**Claude Code** (`~/.claude/claude_desktop_config.json`):
```json
{
  "mcpServers": {
    "ergatai": {
      "url": "http://localhost:3000/mcp"
    }
  }
}
```

**Cursor** (`.cursor/mcp.json`):
```json
{
  "mcpServers": {
    "ergatai": {
      "url": "http://localhost:3000/mcp"
    }
  }
}
```

📖 **See [MCP Configuration Guide](docs/MCP_CONFIG_GUIDE.md) for detailed setup instructions.**

### 3. Start Collaborating

Once configured, your agent can use Ergatai's MCP tools:

```
User: List all connected agents
Claude: [calls list_agents tool]
Claude: Currently connected agents:
  - claude-code (active)
  - cursor (active)

User: Send a message to cursor agent
Claude: [calls send_message tool]
Claude: Message sent to cursor agent
```

## Architecture

```
┌─────────────┐         ┌─────────────┐         ┌─────────────┐
│  Agent A    │ ←─MCP─→ │  Ergatai    │ ←─ACP─→ │  Agent B    │
│ (Claude)    │         │  MCP Server │         │ (Cursor)    │
└─────────────┘         └─────────────┘         └─────────────┘
                               ↓
                        ┌─────────────┐
                        │  Agent C    │
                        │ (Codex)     │
                        └─────────────┘
```

### Two-Layer Communication

**MCP (Model Context Protocol)** - Agent → Ergatai
- Agents connect to Ergatai as MCP clients
- Ergatai acts as MCP server
- Agents send messages and orchestration requests via MCP

**ACP (Agent Client Protocol)** - Ergatai → Agent
- Ergatai connects to agents as ACP client
- Agents act as ACP servers
- Ergatai forwards messages to target agents via ACP

### How It Works

1. **Agent Registration**: When an agent connects via MCP, it's automatically registered
2. **Message Sending**: Agent A calls `send_message` via MCP
3. **Message Relay**: Ergatai receives the message and forwards it to Agent B via ACP
4. **Response**: Agent B processes and responds through the same path

## Features

| Capability | Description |
|------------|-------------|
| **Agent-to-Agent Communication** | Agents can send messages to each other through Ergatai |
| **Multi-Agent Orchestration** | Submit DAG workflows for coordinated task execution |
| **Safe Concurrency** | Token-based file locking prevents conflicting edits |
| **Agent Discovery** | Automatic agent registration via MCP connection |
| **Agent Agnostic** | Supports any MCP-compatible agent — Claude Code, Cursor, Codex, and more |
| **Local First** | All execution happens on your machine, no cloud dependencies |
| **Crash Recovery** | Heartbeat monitoring and automatic lock reclamation |

## MCP Tools

Ergatai exposes the following MCP tools:

### `list_agents`

List all connected agents and their status.

```json
{
  "include_capabilities": true
}
```

**Response:**
```json
{
  "agents": [
    {
      "agent_id": "claude-code",
      "status": "active",
      "capabilities": ["chat", "code"],
      "connected_at": "2026-08-14T10:00:00Z",
      "last_heartbeat": "2026-08-14T10:05:00Z"
    }
  ],
  "total": 1
}
```

### `send_message`

Send a message to another agent.

```json
{
  "target_agent_id": "cursor",
  "message": "Please help me refactor src/auth.rs",
  "message_type": "request"
}
```

**Response:**
```json
{
  "message_id": "msg-123",
  "status": "sent",
  "target_agent_id": "cursor",
  "message_type": "request",
  "session_id": "session-456",
  "session_reused": true
}
```

### `submit_orchestration`

Submit a DAG workflow for multi-agent collaboration.

```json
{
  "dag_definition": "## Task A\n- agent: claude-code\n- task: analyze code\n\n## Task B\n- agent: cursor\n- task: write tests\n- depends_on: [Task A]",
  "context": {
    "project": "ergatai"
  }
}
```

### `check_dag_status`

Check the status of a DAG execution.

```json
{
  "dag_id": "dag-123"
}
```

## Supported Agents

Ergatai supports any agent that implements the MCP protocol:

| Agent | Status | Description |
|-------|--------|-------------|
| Claude Code | ✅ Verified | Anthropic's official CLI |
| Cursor | ✅ Verified | Cursor IDE Agent |
| Codex | ✅ Supported | OpenAI Codex CLI |
| Goose | ✅ Supported | Block's AI assistant |
| Cline | ✅ Supported | VS Code plugin |
| Aider | ✅ Supported | Terminal AI coding tool |
| Custom Agents | ✅ Supported | Any MCP-compatible agent |

## Project Structure

```
ergatai/
├── crates/
│   ├── ergatai-core/      # Core library (business logic)
│   ├── ergatai-api/       # MCP Server (main entry point)
│   │   └── src/
│   │       ├── main.rs
│   │       └── mcp/
│   │           ├── server.rs         # MCP server implementation
│   │           ├── tools.rs          # MCP tool handlers
│   │           ├── agent_registry.rs # Agent tracking
│   │           └── message_relay.rs  # ACP message forwarding
│   ├── ergatai-acp/       # ACP protocol implementation
│   ├── ergatai-dag/       # DAG orchestration engine
│   ├── ergatai-lock/      # File access control
│   ├── ergatai-nats/      # NATS messaging
│   ├── ergatai-agent/     # Agent management
│   ├── ergatai-collab/    # Collaboration logic
│   └── ergatai-error/     # Error handling
├── docs/
│   ├── MCP_CONFIG_GUIDE.md
│   └── superpowers/specs/
├── Cargo.toml
└── README.md
```

## Development

### Build

```bash
# Build all crates
cargo build --workspace

# Build release version
cargo build --release --workspace

# Build specific crate
cargo build -p ergatai-api
```

### Run Tests

```bash
# Run all tests
cargo test --workspace

# Run tests for a specific crate
cargo test -p ergatai-api

# Run with output
cargo test --workspace -- --nocapture
```

### Code Quality

```bash
# Lint
cargo clippy --workspace -- -D warnings

# Format
cargo fmt --all

# Check formatting
cargo fmt --all -- --check
```

### Run MCP Server

```bash
# Development mode
cargo run -p ergatai-api -- --port 3000

# With verbose logging
RUST_LOG=debug cargo run -p ergatai-api -- --port 3000

# With authentication
ERGATAI_API_TOKEN=your-token cargo run -p ergatai-api -- --port 3000
```

## Tech Stack

| Component | Technology | Version |
|-----------|-----------|---------|
| Language | Rust | 2021 edition |
| MCP Protocol | Custom JSON-RPC | 2024-11-05 |
| Agent Protocol | agent-client-protocol | 2.x |
| Messaging | async-nats + JetStream | 0.38 |
| Database | rusqlite (SQLite) | 0.31 |
| HTTP Server | axum | 0.7 |
| Async Runtime | tokio | 1.36 |
| Serialization | serde + serde_json | 1.0 |

## File Access Control

When multiple agents collaborate on the same codebase, Ergatai provides safe concurrent file access:

- **Token-based locking**: READ/WRITE/ADMIN modes
- **Heartbeat monitoring**: Automatic timeout and lock reclamation
- **Git snapshots**: Automatic snapshots before writes for rollback
- **Conflict arbitration**: Priority-based conflict resolution
- **Audit logging**: Complete security audit trail

## Roadmap

### Current Phase (v0.1.0)

- [x] MCP Server implementation
- [x] Agent auto-registration
- [x] Message relay infrastructure
- [x] Agent discovery
- [ ] Complete ACP message forwarding
- [ ] DAG orchestration integration
- [ ] End-to-end testing

### Near-term Plans

- [ ] On-demand ACP session spawning
- [ ] Agent health checks and heartbeat
- [ ] Enhanced error handling
- [ ] Performance optimization
- [ ] Documentation improvements

### Future Plans

- [ ] Web dashboard for monitoring
- [ ] Plugin system for custom tools
- [ ] Agent performance analytics
- [ ] Multi-workspace support
- [ ] Enterprise features

## Security

Recent security improvements (2026-08-13):

- ✅ API path traversal protection + Bearer token authentication
- ✅ Sensitive file detection enhanced
- ✅ Configuration file permission protection (`0o600`)
- ✅ Install command whitelist hardening
- ✅ NATS zombie process fix
- ✅ Signal handler improvements
- ✅ Lock manager correctness enhancements

## Contributing

Contributions are welcome! Please feel free to submit issues and pull requests.

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## Acknowledgments

- [agent-client-protocol](https://github.com/anthropics/agent-client-protocol) - ACP protocol implementation
- [async-nats](https://github.com/nats-io/nats.rs) - NATS client
- [axum](https://github.com/tokio-rs/axum) - Web framework
- [tokio](https://tokio.rs/) - Async runtime
- [Model Context Protocol](https://modelcontextprotocol.io/) - MCP specification

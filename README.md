# Ergatai

> ⚠️ **Project under active development**

Ergatai is a **multi-agent collaboration middleware** that enables AI agents to communicate and work together. It acts as a message broker, relaying messages between agents via MCP (Model Context Protocol) and ACP (Agent Client Protocol).

**Pure Rust implementation** focused on performance and security. Agents connect via **MCP** to send messages, and Ergatai uses **ACP** to forward messages to other agents, enabling seamless agent-to-agent collaboration.

## 🚀 Quick Start

### 1. Start Ergatai MCP Server

```bash
cargo build --release -p ergatai-api
./target/release/ergatai-api --port 3000
```

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

## Features

| Capability | Description |
|------------|-------------|
| **Agent-to-Agent Communication** | Agents can send messages to each other through Ergatai |
| **Multi-Agent Orchestration** | Submit DAG workflows for coordinated task execution |
| **Safe Concurrency** | Token-based file locking prevents conflicting edits |
| **Agent Discovery** | Automatic agent registration via MCP connection |
| **Agent Agnostic** | Supports any MCP-compatible agent — Claude Code, Cursor, Codex, and more |
| **Local First** | All execution happens on your machine, no cloud dependencies |

## Architecture

```
Agent A ←→ MCP ←→ Ergatai ←→ ACP ←→ Agent B
```

**Two-layer communication**:
- **MCP** (Agent → Ergatai): Agents send messages to Ergatai
- **ACP** (Ergatai → Agent): Ergatai forwards messages to target agents

Ergatai acts as a **message relay** in the middle, enabling agents to communicate without knowing about each other directly.

---

# Ergatai

> ⚠️ **Project under active development**

Ergatai is a **multi-agent collaboration platform** for AI-assisted software engineering. It transforms individual AI coding assistants into a coordinated engineering team with parallel task execution, safe concurrent file access, and structured workflow orchestration.

**Pure Rust implementation** focused on performance and security. Uses **ACP (Agent Client Protocol)** for standardized agent communication, embedded **NATS** event bus for reliable messaging, and a **DAG-based** orchestration engine with template-driven data flow.

## Features

| Capability | Description |
|------------|-------------|
| **Parallel Execution** | Multiple agents work concurrently on different parts of the task graph |
| **Safe Concurrency** | Token-based file locking prevents conflicting edits between agents |
| **Workflow Orchestration** | Declarative DAG definitions with dependency tracking and automatic scheduling |
| **Data Flow** | Template engine passes outputs between tasks (`{{node.output}}`) |
| **Crash Recovery** | Heartbeat monitoring and automatic lock reclamation; git snapshots for rollback |
| **Agent Agnostic** | Supports 13+ agents via ACP — Claude Code, Codex, Goose, and more |
| **Local First** | All execution happens on your machine, no cloud dependencies |

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│  CLI (Rust + ratatui)                                               │
│  Interactive chat · Agent selection · DAG management · Progress     │
└──────────────────────────────┬──────────────────────────────────────┘
                               │
┌──────────────────────────────▼──────────────────────────────────────┐
│  ergatai-core (Rust library)                                        │
│                                                                     │
│  ┌─────────────────┐  ┌──────────────────┐  ┌───────────────────┐  │
│  │  ACP Protocol   │  │  NATS Event Bus  │  │  File Access      │  │
│  │                  │  │                  │  │  Control          │  │
│  │  Bidirectional   │  │  ┌────────────┐  │  │  Tokens (R/W/A)   │  │
│  │  JSON-RPC over   │  │  │ DAG        │  │  │  Lock manager     │  │
│  │  stdin/stdout    │  │  │ Scheduler  │  │  │  Watchdog         │  │
│  │                  │  │  └─────┬──────┘  │  │  Snapshots (git)  │  │
│  │  Session mgmt    │  │  ┌─────▼──────┐  │  │  Audit log        │  │
│  │  Pool manager    │  │  │ Task       │  │  │  Arbitration      │  │
│  │  Approval flow   │  │  │ Scheduler  │  │  │                   │  │
│  └────────┬────────┘  │  └────────────┘  │  └───────────────────┘  │
│           │            │                  │                         │
│           └────────────┴──────────────────┴─────────────────────────│
│                     Cross-Agent Collaboration Engine                │
└─────────────────────────────────────────────────────────────────────┘
```

### Core Components

| Component | Description | Status |
|-----------|-------------|--------|
| **ergatai-core** | Core library: all business logic, ACP, NATS, file access control | ✅ Complete |
| **ergatai-cli** | CLI binary: interactive chat interface | 🚧 In development |
| **ergatai-api** | API server: HTTP/WebSocket interface (GUI-ready) | ✅ Basics complete |

## Quick Start

### Build

```bash
# Build all crates
cargo build --workspace

# Build release version
cargo build --release --workspace
```

### Using the CLI

```bash
# Interactive chat (select agent)
cargo run --bin ergatai -- chat

# Chat with a specific agent
cargo run --bin ergatai -- chat --agent claude-code

# One-off message
cargo run --bin ergatai -- chat "Help me refactor this module"

# List available agents
cargo run --bin ergatai -- agents list

# Submit a DAG workflow
cargo run --bin ergatai -- dag submit workflow.md

# Check DAG status
cargo run --bin ergatai -- dag status <dag-id>
```

### Running the API Server

```bash
# Start API server
cargo run --bin ergatai-api -- --port 3000

# With authentication
ERGATAI_API_TOKEN=your-token cargo run --bin ergatai-api -- --port 3000
```

## Agent Support

Ergatai supports various AI coding assistants via the ACP protocol:

| Agent | Status | Description |
|-------|--------|-------------|
| Claude Code | ✅ Verified | Anthropic's official CLI |
| Cursor | ✅ Verified | Cursor IDE Agent |
| Codex | ✅ Supported | OpenAI Codex CLI |
| Goose | ✅ Supported | Block's AI assistant |
| Cline | ✅ Supported | VS Code plugin |
| Aider | ✅ Supported | Terminal AI coding tool |
| Custom Agents | ✅ Supported | Any ACP-compatible agent |

## Multi-Agent Collaboration Example

### 1. Define a DAG Workflow

Create `refactor-workflow.md`:

```markdown
## Task A (Code Analysis)
- **agent**: claude-code
- **task**: Analyze code structure and dependencies in src/ directory
- **output**: analysis_result

## Task B (Write Tests)
- **agent**: cursor
- **task**: Write unit tests for critical modules
- **depends_on**: [Task A]
- **input**: "Based on analysis: {{TaskA.analysis_result}}"
- **output**: test_files

## Task C (Refactor Implementation)
- **agent**: claude-code
- **task**: Refactor based on analysis and tests
- **depends_on**: [Task A, Task B]
- **input**: "Analysis: {{TaskA.analysis_result}}, Tests: {{TaskB.test_files}}"
- **retry**: 3
- **timeout**: 300
```

### 2. Execute the Workflow

```bash
$ cargo run --bin ergatai -- dag submit refactor-workflow.md

✓ DAG submitted: dag-12345

Task scheduling:
  [A] Code analysis (claude-code) .............. ✓ Done (23s)
  [B] Write tests (cursor) ..................... ✓ Done (45s)
  [C] Refactor (claude-code) ................... ✓ Done (67s)

✓ DAG complete: dag-12345
Total time: 135s
```

## File Access Control

Safety guarantees when multiple agents edit files concurrently:

```rust
// Agent A acquires write lock
let token = lock_manager.acquire_write("src/main.rs").await?;

// Modify file
fs::write("src/main.rs", new_content)?;

// Automatic git snapshot
snapshot_manager.create_snapshot("src/main.rs")?;

// Release lock (other agents can now acquire it)
lock_manager.release(token).await?;
```

**Features:**
- Token-level locking (READ/WRITE/ADMIN)
- Heartbeat monitoring and automatic timeout reclamation
- Git snapshots for rollback support
- Conflict detection and arbitration
- Security audit logging

## Development

### Running Tests

```bash
# Run all tests
cargo test --workspace

# Run tests for a specific crate
cargo test -p ergatai-core

# Run integration tests
cargo test --test '*'
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

### Project Structure

```
ergatai/
├── crates/
│   ├── ergatai-core/     # Core library
│   │   ├── src/
│   │   │   ├── acp/          # ACP protocol
│   │   │   ├── nats/         # NATS messaging
│   │   │   ├── orchestration/# DAG orchestration
│   │   │   ├── cross_agent/  # Multi-agent collaboration
│   │   │   ├── file_access/  # File access control
│   │   │   └── agent/        # Agent management
│   │   └── Cargo.toml
│   ├── ergatai-cli/      # CLI binary
│   │   ├── src/
│   │   │   ├── main.rs       # Entry point
│   │   │   ├── commands/     # Command handlers
│   │   │   └── ui/           # TUI components
│   │   └── Cargo.toml
│   └── ergatai-api/      # API server
│       ├── src/
│       │   └── main.rs
│       └── Cargo.toml
├── Cargo.toml           # Workspace configuration
└── README.md            # Project documentation
```

## Tech Stack

| Component | Technology | Version |
|-----------|-----------|---------|
| Language | Rust | 2021 edition |
| Agent Protocol | agent-client-protocol | 2.x |
| Messaging | async-nats + JetStream | 0.38 |
| Database | rusqlite (SQLite) | 0.31 |
| CLI Framework | clap | 4.5 |
| TUI | ratatui + crossterm | 0.26 / 0.27 |
| HTTP Server | axum | 0.7 |
| Async Runtime | tokio | 1.36 |

## Roadmap

### Current Release (v0.1.0)

- [x] Rust workspace architecture
- [x] ACP protocol integration
- [x] NATS messaging system
- [x] DAG orchestration engine
- [x] File access control
- [x] CLI basic framework

### Near-term Plans

- [ ] Complete CLI chat interface
- [ ] Agent selection and configuration UI
- [ ] Real-time DAG progress display
- [ ] Session persistence
- [ ] Integration test suite

### Future Plans

**v0.x (Current Phase) - CLI Conversational Version**
- [ ] CLI chat interface refinement (permission confirmation UI, real-time progress)
- [ ] DAG visualization (terminal UI)
- [ ] Agent performance statistics and analysis
- [ ] Plugin system
- [ ] More agent support

**Note**: Current development focus is on the **CLI conversational version**.

## Recent Security Improvements

2026-08-13: Completed comprehensive code security audit and fixes:

- ✅ API path traversal protection + Bearer token authentication
- ✅ Sensitive file detection enhanced (`*.env` patterns + path validation)
- ✅ Configuration file permission protection (`0o600`)
- ✅ Install command whitelist hardening (shell injection prevention)
- ✅ NATS zombie process fix
- ✅ Signal handler improvements
- ✅ Lock manager correctness enhancements
- ✅ CLI command conflict resolution

## Contributing

Contributions are welcome! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for more information.

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## Acknowledgments

- [agent-client-protocol](https://github.com/anthropics/agent-client-protocol) - ACP protocol implementation
- [async-nats](https://github.com/nats-io/nats.rs) - NATS client
- [ratatui](https://github.com/ratatui-org/ratatui) - Terminal UI library
- [clap](https://github.com/clap-rs/clap) - Command-line argument parsing

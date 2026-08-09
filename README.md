# Ergatai

> ⚠️ **This project is under active development.** Building a multi-agent collaboration platform with Rust backend and ACP protocol standardization.

A **multi-agent collaboration platform** that turns AI coding assistants into a coordinated team. Built with a **Rust backend**, **ACP (Agent Client Protocol)** for agent communication, **NATS event bus** for reliable internal messaging, and **DAG-based orchestration** with template-driven data flow — enabling multiple agents to collaborate, communicate, and coordinate tasks.

## What is Ergatai?

Ergatai is not just another coding assistant — it's a **platform for agent collaboration**. While traditional tools focus on single-agent interactions, Ergatai enables:

- **Multi-Agent Teams** — PM → Dev → QA pipelines that work together via DAG orchestration
- **ACP Standardization** — Plug in any ACP-compliant agent (Claude, Codex, Goose, 13+ supported)
- **Rust Performance** — Native backend with NAPI bindings for speed and safety
- **NATS Event Bus** — Embedded NATS server for reliable task routing, crash recovery, and event-driven scheduling
- **Template Data Flow** — `{{var}}` templates pass data between DAG nodes automatically
- **Local-First** — Your code, your agents, your machine (no cloud lock-in)

## Architecture

```
┌────────────────────────────────────────────────────────────────┐
│  Frontend (React + Electron)                                   │
│  - Chat UI, diff preview, tool display                         │
│  - Plan mode, file viewer, terminal                            │
└──────────────────────────┬─────────────────────────────────────┘
                           │ tRPC
┌──────────────────────────▼─────────────────────────────────────┐
│  Main Process (TypeScript)                                     │
│  - tRPC routers, NAPI bindings → Rust                          │
└──────────────────────────┬─────────────────────────────────────┘
                           │ NAPI
┌──────────────────────────▼─────────────────────────────────────┐
│  Rust Backend (src-rust/)                                      │
│                                                                │
│  ┌──────────────────┐   ┌──────────────────────────────────┐  │
│  │  ACP Protocol     │   │  NATS Event Bus                  │  │
│  │  (Client↔Agent)   │   │  (Internal component signaling)  │  │
│  │                   │   │                                  │  │
│  │  Bidirectional    │   │  ┌─────────┐  ┌─────────────┐  │  │
│  │  JSON-RPC over    │   │  │ Dag     │  │ Task        │  │  │
│  │  stdin/stdout     │   │  │Scheduler│←→│ Scheduler   │  │  │
│  │                   │   │  └────┬────┘  └──────┬──────┘  │  │
│  └────────┬─────────┘   │       │   events.rs   │         │  │
│           │              │       │   event_bus.rs│         │  │
│           │              │  ┌────▼────────────▼──────┐   │  │
│  ┌────────▼─────────┐   │  │ Agent Launcher          │   │  │
│  │  Session Mgmt    │   │  │ (spawn + ACP connect)   │   │  │
│  │  Pool Manager    │   │  └─────────────────────────┘   │  │
│  └──────────────────┘   └──────────────────────────────────┘  │
│                                                                │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  DAG Orchestration                                       │  │
│  │  - Markdown → TaskGraph parser                           │  │
│  │  - {{var}} template engine (DagContext)                  │  │
│  │  - Event-driven scheduling (NATS)                        │  │
│  └──────────────────────────────────────────────────────────┘  │
└────────────────────────────────────────────────────────────────┘
                           │ ACP (stdin/stdout)
              ┌────────────┼────────────┐
              ▼            ▼            ▼
         ┌─────────┐ ┌─────────┐ ┌─────────┐
         │ Claude  │ │ Codex   │ │ Goose   │  ...
         │ Code    │ │         │ │         │
         └─────────┘ └─────────┘ └─────────┘
```

### Communication Layers

| Layer | Technology | Direction | Purpose |
|-------|-----------|-----------|---------|
| **Agent ↔ Ergatai** | ACP (JSON-RPC) | Bidirectional | Prompts, responses, tool calls |
| **Ergatai internal** | NATS (JetStream) | Event stream | Task routing, completion events |

- **ACP** = Client(Ergatai) ↔ Agent protocol. Bidirectional, per-session.
- **NATS** = Ergatai's internal event bus. Reliable delivery, crash recovery.
- NATS does NOT talk to agents directly. Agents only communicate via ACP.
- Agent-to-agent conversation requires Ergatai as relay (planned Phase 5).

## Supported Agents

Ergatai supports **13 builtin agents** with automatic discovery, plus unlimited custom agents:

### Tier 1 Agents (Full Metadata)
| Agent | Protocol | Status |
|-------|----------|--------|
| **Goose** | ACP native | ✅ Supported |
| **Claude Code** | ACP via `claude-agent-sdk` | ✅ Supported |
| **Codex** | ACP via `@zed-industries/codex-acp` | ✅ Supported |
| **Hermes** | ACP | ✅ Supported |

### Tier 2 Agents (Preset)
| Agent | Protocol | Status |
|-------|----------|--------|
| **Devin** | ACP | ✅ Supported |
| **Cursor** | ACP | ✅ Supported |
| **Oh My Pi** | ACP | ✅ Supported |
| **Grok Build** | ACP | ✅ Supported |
| **OpenCode** | ACP | ✅ Supported |
| **Kimi Code** | ACP | ✅ Supported |
| **Amp** | ACP | ✅ Supported |
| **OpenClaw** | ACP | ✅ Supported |

### Custom Agents
Register any ACP-compliant agent via custom harness definitions in `~/.config/ergatai/custom_harnesses/`.

## Key Features

### DAG Orchestration
- **Markdown-based DAGs** — Define task graphs in markdown with `depends_on`, `input`, `output`, `priority`, `timeout`, `retry`
- **Template Engine** — `{{global.var}}` and `{{node_id.key}}` for data flow between nodes
- **Event-Driven Scheduling** — NATS pub/sub replaces file polling (with direct-call fallback)
- **DagContext** — Tracks global variables + per-node outputs, renders templates automatically

### NATS Event Bus
- **Embedded nats-server** — Bundled as child process (~15MB), auto-started on app launch
- **JetStream Persistence** — WorkQueue for tasks, Limits stream for DAG events, crash recovery
- **Dual-Mode** — Every component checks `is_nats_initialized()`, falls back to direct calls if unavailable
- **Subject Naming** — `ergatai.task.submit.{pool}`, `ergatai.dag.node_complete.{node}`, etc.

### Multi-Agent Collaboration
- **ACP Protocol** — Bidirectional JSON-RPC over stdin/stdout between Ergatai (client) and Agents (server)
- **Agent Network** — Register, discover, and communicate with 13+ supported agents
- **Task Distribution** — Send tasks between agents, track progress, collect results
- **Agent Relay** — Agent-to-agent communication routed through Ergatai (planned Phase 5)

### ACP Agent Management
- **Automatic Discovery** — Detects installed agents, probes authentication status
- **Global Configuration** — Unified provider/model/env_vars configuration
- **Custom Harness** — Define custom agents via JSON files
- **Environment Injection** — Automatic env var injection based on runtime metadata

### Visual Collaboration UI
- **Chat Interface** — Familiar chat UX for each agent
- **Tool Execution** — Watch agents run bash, edit files, search web
- **Diff Preview** — See code changes before they land
- **Agent Status** — Monitor which agents are active, idle, or busy

### Additional Features
- **Git Worktree Isolation** — Each chat runs in its own isolated worktree
- **Background Agents** — Cloud sandboxes that run when your laptop sleeps
- **Live Browser Previews** — Preview dev branches in a real browser
- **Kanban Board** — Visualize agent sessions
- **Built-in Git Client** — Visual staging, diffs, PR creation
- **MCP & Plugins** — Server management, plugin marketplace
- **Voice Input** — Hold-to-talk dictation
- **Plan Mode** — Structured plans with markdown preview
- **Extended Thinking** — Enabled by default with visual UX

## Development

```bash
# Prerequisites: Bun, Python 3.11, Rust toolchain
bun install
bun run dev
```

### Build from Source

```bash
bun install
bun run build:rust        # Build Rust backend
bun run build:napi        # Generate NAPI bindings
bun run build             # Build Electron app
bun run package:linux     # or package:mac, package:win
```

## Installation

### macOS
```bash
# Download from releases (coming soon)
brew install ergatai
```

### Linux
```bash
# AppImage or DEB package (coming soon)
```

### Windows
```bash
# NSIS installer (coming soon)
```

## Development Status

**Phase 1 — NATS Infrastructure** ✅ Complete
- nats-server child process management, async-nats connection, JetStream task queue
- sdk_pool_manager migrated from VecDeque to NATS dual-mode

**Phase 2 — Template Engine + Data Flow** ✅ Complete
- `{{global.var}}` and `{{node_id.key}}` template rendering
- DagContext for tracking global vars + per-node outputs
- DAG parser extended with `output`, `priority`, `timeout`, `retry` fields

**Phase 3 — DAG Event-Driven Architecture** ✅ Complete
- NATS event bus replacing direct function calls and file polling
- EventBus with typed payloads (TaskSubmit, NodeComplete, NodeFailed, DagComplete)
- Fallback paths: every component checks `is_nats_initialized()` before using NATS

**Phase 4 — Markdown Orchestration Enhancement** ✅ Complete (merged into Phase 2)

**Phase 5 — Agent Message Routing** ✅ Complete
- Agent-to-agent messaging via NATS relay (Ergatai as middleman)
- Message router detects `@agent_name` mentions and routes automatically
- NAPI bindings: `nats_route_agent_message` / `nats_scan_and_route_mentions`
- Subject: `ergatai.agent.message.{agent_id}`

## Community

- **Discord**: Join our [Discord server](https://discord.gg/8ektTZGnj4) for support and discussions
- **Issues**: Report bugs or request features via [GitHub Issues](https://github.com/windreach/ergatai/issues)
- **Contributions**: PRs welcome! See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines

## Acknowledgments

Ergatai is built with inspiration and code from two excellent projects:

### Frontend Architecture
The frontend is based on [1Code](https://1code.dev) by 21st.dev team (React + Electron + TypeScript). We thank the original contributors for their work on the UI components, Electron integration, and initial agent support.

**Note**: 1Code only has a TypeScript backend. The **Rust backend in Ergatai is original work**, built from scratch for native performance and ACP protocol support.

### Backend Design
The ACP agent management system (runtime metadata, discovery mechanism, global configuration) is inspired by [Buzz](https://github.com/nicepkg/buzz), a pioneering multi-agent desktop application.

Both projects are licensed under Apache 2.0. See [NOTICE](NOTICE) for detailed attribution information.

## License

Apache License 2.0 — see [LICENSE](LICENSE) for details.

This project includes code derived from 1Code and Buzz, both licensed under Apache 2.0. Original copyright notices are preserved as required.

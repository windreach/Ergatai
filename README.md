# Ergatai

> ⚠️ **This project is under active development.**

Ergatai is a **multi-agent collaboration platform** for AI-assisted software engineering. It transforms individual AI coding assistants into a coordinated engineering team — enabling parallel task execution, safe concurrent file access, and structured workflow orchestration.

Built with a **Rust** core for performance and safety, **ACP (Agent Client Protocol)** for standardized agent communication, an embedded **NATS** event bus for reliable message delivery, and a **DAG-based orchestration engine** with template-driven data flow.

## Overview

Traditional AI coding tools operate in isolation: one agent, one conversation, one task at a time. Ergatai introduces the infrastructure required for **multi-agent collaboration at scale**:

| Capability | Description |
|---|---|
| **Parallel Execution** | Multiple agents work concurrently on different parts of a task graph |
| **Safe Concurrency** | Token-based file locking prevents conflicting edits across agents |
| **Workflow Orchestration** | Declarative DAG definitions with dependency tracking and automatic scheduling |
| **Data Flow** | Template engine passes outputs between tasks (`{{node.output}}`) |
| **Crash Recovery** | Heartbeat monitoring with automatic lock reclamation; git snapshots for rollback |
| **Agent Agnostic** | 13+ supported agents via ACP — Claude Code, Codex, Goose, and more |
| **Local-First** | All execution happens on your machine. No cloud dependency. |

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│  Frontend (React 19 + Electron)                                     │
│  Chat UI · Diff Preview · Tool Execution · Agent Status             │
└──────────────────────────────┬──────────────────────────────────────┘
                               │ tRPC (trpc-electron)
┌──────────────────────────────▼──────────────────────────────────────┐
│  Main Process (TypeScript)                                          │
│  tRPC Routers · Auth Manager · Drizzle ORM · NAPI Bindings          │
└──────────────────────────────┬──────────────────────────────────────┘
                               │ NAPI-RS
┌──────────────────────────────▼──────────────────────────────────────┐
│  Rust Backend                                                       │
│                                                                     │
│  ┌─────────────────┐  ┌──────────────────┐  ┌───────────────────┐  │
│  │  ACP Protocol    │  │  NATS Event Bus  │  │  File Access      │  │
│  │                  │  │                  │  │  Control          │  │
│  │  Bidirectional   │  │  ┌────────────┐  │  │                   │  │
│  │  JSON-RPC over   │  │  │ DAG        │  │  │  Token (R/W/Admin)│  │
│  │  stdin/stdout    │  │  │ Scheduler  │  │  │  Lock Manager     │  │
│  │                  │  │  └─────┬──────┘  │  │  Watchdog         │  │
│  │  Session Mgmt   │  │  ┌─────▼──────┐  │  │  Snapshot (git)   │  │
│  │  Pool Manager   │  │  │ Task       │  │  │  Audit Log        │  │
│  │  Approval Flow  │  │  │ Scheduler  │  │  │  Conflict Arb.    │  │
│  └────────┬────────┘  │  └────────────┘  │  └───────────────────┘  │
│           │            │                  │                         │
│  ┌────────▼──────────────────────────────────────────────────────┐  │
│  │  DAG Orchestration                                            │  │
│  │  Markdown → TaskGraph · {{var}} Templates · DagContext        │  │
│  └───────────────────────────────────────────────────────────────┘  │
└──────────────────────────────┬──────────────────────────────────────┘
                               │ ACP (stdin/stdout)
                  ┌─────────────┼─────────────┐
                  ▼             ▼             ▼
             ┌─────────┐  ┌─────────┐  ┌─────────┐
             │ Claude  │  │  Codex  │  │  Goose  │  ...
             │  Code   │  │         │  │         │
             └─────────┘  └─────────┘  └─────────┘
```

### Communication Model

The system operates on two independent communication layers:

| Layer | Protocol | Direction | Purpose |
|---|---|---|---|
| **Agent ↔ Ergatai** | ACP (JSON-RPC over stdio) | Bidirectional | Prompts, responses, tool calls, approval requests |
| **Ergatai Internal** | NATS (JetStream) | Event stream | Task routing, completion events, file notifications |

Agents never communicate directly with each other. All inter-agent messaging is relayed through Ergatai via NATS, ensuring centralized control and observability.

## Multi-Agent Infrastructure

### DAG Orchestration

Tasks are defined in Markdown and parsed into a directed acyclic graph:

```markdown
## Task A — Analyze Codebase
- **agent**: agent-a
- **task**: tasks/analyze.md

## Task B — Write Tests
- **agent**: agent-b
- **task**: tasks/test.md
- **depends_on**: [Task A]
- **input**: "Analysis: {{TaskA.review_result}}"
- **output**: test_result, coverage
- **retry**: 3
- **timeout**: 300
```

The orchestration engine handles:
- **Dependency resolution** — Topological scheduling with parallel execution where possible
- **Template rendering** — `{{global.*}}` for global variables, `{{node_id.*}}` for upstream outputs
- **Event-driven scheduling** — NATS pub/sub triggers downstream tasks on completion
- **Retry and timeout** — Per-node configurable resilience

### File Access Control

When multiple agents operate on the same project, file conflicts are the primary risk. Ergatai provides a comprehensive file access control system:

**Token-Based Permissions**

| Mode | Scope | Use Case |
|---|---|---|
| `READ` | Shared — multiple readers allowed | Code analysis, review |
| `WRITE` | Exclusive — one writer per file | Code modification |
| `ADMIN` | Full access including sensitive paths | Configuration, credentials |

**Safety Mechanisms**

- **SQLite-persisted locks** — Atomic transactions prevent race conditions
- **Heartbeat monitoring** — Watchdog detects unresponsive agents with progressive timeout (30s → 60s → reclaim)
- **Git snapshots** — Every WRITE creates a git blob snapshot before modification, enabling rollback
- **Path traversal protection** — Canonicalization checks prevent directory escape
- **Sensitive path detection** — `.env`, `.key`, `credentials` files require ADMIN permission
- **Conflict arbitration** — Priority-based resolution when WRITE locks collide
- **Audit logging** — All lock operations are recorded for security review

### Agent Communication

Agents can message each other through Ergatai's relay system:

```
Agent A: "@agent-b please review this implementation"
    ↓ (message_router detects @mention)
NATS → ergatai.agent.message.{agent-b}
    ↓
Agent B receives message via ACP
```

### NATS Event Bus

An embedded `nats-server` subprocess provides reliable internal messaging:

**JetStream Streams**

| Stream | Subjects | Retention | Purpose |
|---|---|---|---|
| `TASK_QUEUE` | `ergatai.task.submit.*` | WorkQueue | Task distribution to agents |
| `FILE_ACCESS_REQUESTS` | `ergatai.file.access.request` | WorkQueue | File access request persistence |
| `FILE_ACCESS_GRANTS` | `ergatai.file.access.grant.*` | WorkQueue | Token grant delivery guarantee |
| `FILE_ACCESS_ESCALATIONS` | `ergatai.file.access.escalate.*` | WorkQueue | Approval escalation persistence |
| `FILE_EVENTS` | `ergatai.file.ready.*`, `ergatai.file.error.*` | WorkQueue | File completion/error notifications |

DAG events (`ergatai.dag.*`) use core NATS pub/sub without JetStream persistence.

**Subject Naming Convention**

```
ergatai.
├── task.submit.{agent}              # Task submission
├── task.complete.{task_id}          # Task completion
├── task.fail.{task_id}              # Task failure
├── dag.node_complete.{node}         # DAG node finished
├── dag.node_failed.{node}           # DAG node failed
├── dag.complete.{dag_id}            # Entire DAG finished
├── agent.message.{agent_id}         # Inter-agent messaging (@mention relay)
├── file.access.request              # File access request
├── file.access.grant.{agent}        # File access granted
├── file.access.deny.{agent}         # File access denied
├── file.access.escalate.{agent}     # Escalated to main agent for approval
├── file.access.revoke.{agent}       # File access revoked
├── file.conflict.arbitrate.{agent}  # WRITE conflict arbitration result
├── file.ready.{hash}                # File WRITE completed (JetStream)
├── file.error.{hash}                # File WRITE failed (JetStream)
└── system.token.{agent_id}          # System token registration/renewal
```

Every component implements dual-mode operation: NATS when available, direct function calls as fallback.

## Supported Agents

Ergatai supports **13 built-in agents** with automatic discovery, plus unlimited custom agents via ACP:

### Tier 1 — Full Integration
| Agent | Protocol | Notes |
|---|---|---|
| **Goose** | ACP native | Full metadata |
| **Claude Code** | ACP via SDK | Full metadata |
| **Codex** | ACP via Zed | Full metadata |
| **Hermes** | ACP | Full metadata |

### Tier 2 — Preset Configuration
| Agent | Protocol |
|---|---|
| Devin | ACP |
| Cursor | ACP |
| Oh My Pi | ACP |
| Grok Build | ACP |
| OpenCode | ACP |
| Kimi Code | ACP |
| Amp | ACP |
| OpenClaw | ACP |

Custom agents can be registered via JSON configuration in `~/.config/ergatai/custom_harnesses/`.

## Development

### Prerequisites

- **Bun** (package manager)
- **Rust toolchain** (cargo, rustc)
- **Node.js 18+** (for Electron)

### Quick Start

```bash
bun install          # Install dependencies
bun run dev          # Start Electron with hot reload
```

### Build

```bash
bun run build:rust   # Compile Rust backend
bun run build:napi   # Generate NAPI bindings
bun run build        # Build Electron app
bun run package:mac  # Package for macOS (DMG + ZIP)
bun run package:win  # Package for Windows (NSIS)
bun run package:linux # Package for Linux (AppImage + DEB)
```

### Testing

```bash
# Rust backend
cargo test --lib -- --skip agent::discovery
cargo test --lib file_access    # File access control (20+ tests)
cargo test --lib orchestration  # DAG parser + templates (37 tests)

# Database
bun run db:generate  # Generate Drizzle migrations
bun run db:push      # Push schema (dev only)
```

## Technology Stack

| Layer | Technology |
|---|---|
| Desktop | Electron 33, electron-vite, electron-builder |
| UI | React 19, TypeScript 5.4, Tailwind CSS |
| Components | Radix UI, Lucide Icons, Motion, Sonner |
| State | Jotai (UI), Zustand (tabs), React Query (server state) |
| Backend | tRPC, Drizzle ORM, SQLite |
| Core | Rust, NAPI-RS, async-nats 0.38, rusqlite |
| AI | ACP Protocol (agent-client-protocol SDK) |
| Messaging | Embedded nats-server (JetStream) |
| Orchestration | Custom TaskGraph + template engine |

## Project Status

| Phase | Description | Key Deliverables | Status |
|---|---|---|---|
| **Phase 1** | NATS infrastructure + Pool task queue | Embedded nats-server, JetStream TASK_QUEUE, dual-mode (VecDeque + JetStream) | ✅ Complete |
| **Phase 2** | Template engine + data flow pipeline | `{{global.*}}` / `{{node.*}}` rendering, DagContext | ✅ Complete |
| **Phase 3** | DAG event-driven scheduling | NATS pub/sub triggers, direct-call fallback | ✅ Complete |
| **Phase 4** | Markdown orchestration enhancement | input/output/retry/timeout/priority fields | ✅ Complete |
| **Phase 5** | Agent message routing | `message_router.rs`, @mention detection, `ergatai.agent.message.*` | ✅ Complete |
| **Phase 6** | File access control | Two-level tokens, SQLite locks, watchdog, snapshots, conflict arbitration, single-agent bypass, approval flow, READ_LATEST | ✅ Complete |
| **Phase 7** | NATS file event streams | 4 JetStream streams, FileEventsConsumer, typed EventBus publish | ✅ Complete |

## Community

- **Discord**: [Join our server](https://discord.gg/8ektTZGnj4) for support and discussion
- **Issues**: [GitHub Issues](https://github.com/windreach/ergatai/issues) for bug reports and feature requests
- **Contributions**: See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines

## Acknowledgments

Ergatai builds upon the work of two excellent open-source projects:

- **Frontend**: Based on [1Code](https://1code.dev) by 21st.dev (React + Electron + TypeScript). The Rust backend is original work.
- **Agent Management**: Inspired by [Buzz](https://github.com/nicepkg/buzz), a pioneering multi-agent desktop application.

Both projects are licensed under Apache 2.0. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for detailed attribution.

## License

Apache License 2.0 — see [LICENSE](LICENSE) for details.

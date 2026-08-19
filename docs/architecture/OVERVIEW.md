# Architecture Overview

## System Architecture

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

## Dual Protocol Stack

Ergatai uses two independent protocols for bidirectional communication:

| Direction | Protocol | Purpose |
|-----------|----------|---------|
| **Agent → Ergatai** | MCP (Streamable HTTP) | Agents call tools: `list_agents`, `send_message`, `submit_orchestration` |
| **Ergatai → Agent** | rmux pane injection | Ergatai delivers messages by injecting text into the agent's rmux pane |

### Agent Registration Flow

1. Agent connects to Ergatai via MCP (as MCP client)
2. Agent calls tools (e.g. `list_agents`, `send_message`) to interact with Ergatai
3. Ergatai pushes tasks/messages back by injecting text into the agent's rmux pane

**Key point**: Agents do NOT need to expose any incoming endpoint. Ergatai delivers messages by injecting text into the agent's rmux pane, simulating keyboard input. Agent identity is deterministically bound to panes via the `RMUX_PANE` environment variable.

## DAG Orchestration Flow

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

## Module Architecture

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
```

### Key Modules

| Module | Responsibility |
|--------|---------------|
| `ergatai-api` | HTTP/MCP server, request routing, agent endpoints |
| `ergatai-runtime` | Agent lifecycle, rmux backend, pane injection |
| `ergatai-nats` | Embedded NATS, JetStream streams, event bus |
| `ergatai-collab` | DAG scheduling, task orchestration |
| `ergatai-lock` | File locking, fanotify enforcement, conflict resolution |
| `ergatai-cli` | User-facing CLI (`ergatai`, `ega`) |

## Data Flow

### Message Delivery

```
Agent A (rmux pane)
  │  MCP tools/call: send_message
  ▼
MCP Server (ergatai-api)
  │  Publish to NATS JetStream
  ▼
NATS JetStream (AGENT_MESSAGES stream)
  │  Background consumer pulls message
  ▼
Message Delivery Service
  │  Inject text into target pane
  ▼
Agent B's rmux pane (receives message)
```

### File Locking

```
Agent requests file access
  │
  ▼
File Access Control (ergatai-lock)
  │  Check token (READ/WRITE/ADMIN)
  │
  ├─ If authorized → fanotify allows open()
  │
  └─ If denied → fanotify blocks open()
       │
       ▼
     Returns EPERM to application
```

## Tech Stack

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

## Related Docs

- [Installation Guide](../getting-started/INSTALL.md)
- [CLI Guide](../guide/CLI.md)
- [MCP Configuration](../guide/MCP.md)
- Internal dev docs: `docs/dev/`

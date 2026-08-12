# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is this?

**Ergatai** - A multi-agent collaboration platform for AI-assisted software engineering. Transforms individual AI coding assistants into a coordinated engineering team with parallel task execution, safe concurrent file access, and DAG-based workflow orchestration.

Built with **Rust** core (performance + safety), **ACP** (Agent Client Protocol) for standardized agent communication, embedded **NATS** event bus for reliable messaging, and **Electron + React** frontend.

## Commands

### Development

```bash
# Frontend (Electron + React)
bun run dev              # Start Electron with hot reload

# Rust backend
cd src-rust
cargo build              # Build Rust library
cargo test --lib         # Run all tests (418 tests)
cargo test --lib <module_name>  # Run specific module tests
cargo clippy -- -D warnings     # Lint (treats warnings as errors)
cargo fmt              # Format code

# Build everything
bun run build            # Compile TypeScript + Rust
bun run build:rust       # Build Rust release binary
bun run build:napi       # Build NAPI bindings

# Package for distribution
bun run package          # Package for current platform
bun run package:mac      # Build macOS (DMG + ZIP)
bun run package:win      # Build Windows (NSIS + portable)
bun run package:linux    # Build Linux (AppImage + DEB)
```

### Database (Drizzle + SQLite)

```bash
bun run db:generate      # Generate migrations from schema
bun run db:push          # Push schema directly (dev only)
```

## Architecture

### Layer Responsibilities

| Layer | Language | Responsibility |
|-------|----------|----------------|
| **Frontend** (`src/renderer/`) | TypeScript/React | UI layer, user interactions |
| **Main Process** (`src/main/`) | TypeScript | Electron main, tRPC routers, DB access |
| **Rust Backend** (`src-rust/src/`) | Rust | Core logic, ACP protocol, NATS, file access control |

**Fork Principle**: Frontend + Main TS are forked from 21st Agents. When inconsistencies arise, **Rust backend is the source of truth**.

### Call Chain

```
Frontend (React) → tRPC → Main (TypeScript) → NAPI-RS → Rust (core logic)
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
Main Agent (claude-code) outputs DAG markdown
    ↓
Ergatai parses DAG → NATS distributes tasks → Sub-agents A/B/C (ACP execution)
                   ↑ NATS events relay completion
```

### Rust Backend Modules

**Core Infrastructure:**
- `acp/` - ACP protocol layer (session management, agent pool, SDK integration)
- `nats/` - NATS server management, JetStream streams, event bus, task queues
- `orchestration/` - DAG parser, template engine ({{var}}), context management

**Multi-Agent Collaboration:**
- `cross_agent/` - DAG scheduler, task scheduler, agent launcher, message router
- `file_access/` - Token-based file locking, lock manager, watchdog, snapshots, audit
- `agent/` - Agent discovery, configuration (13 built-in + hosted agents)

**Integration:**
- `napi/` - NAPI bindings (Rust → TypeScript FFI)
- `error/` - Error types (ErgataiError enum)

### File Access Control (Multi-Agent Safety)

Token-based locking prevents conflicting edits:

```
Agent A (WRITE lock on src/foo.rs)
    ↓ holds token
    ↓ modifies file
    ↓ creates git snapshot
    ↓ releases lock
Agent B (waits for WRITE lock → acquires → continues)
```

**Two-level Token System:**
- `SystemToken` - Session-level admission (binds agent_id + session_id)
- `FileToken` - Operation-level (READ/WRITE/ADMIN scope)

**Database**: `{project_root}/.ergatai/locks.db` (SQLite with 5 tables)

**Single Agent Mode**: When only one agent is active, automatically bypasses approval flow and conflict arbitration (5-second hysteresis debounce).

### DAG Orchestration

```markdown
## Task A (Analyze code)
- **agent**: agent-a
- **task**: tasks/analyze.md

## Task B (Write tests)
- **agent**: agent-b
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

## Database (Drizzle ORM)

**Location**: `{userData}/data/agents.db` (SQLite)

**Schema**: `src/main/lib/db/schema/index.ts`

```typescript
// Three main tables:
projects    → id, name, path (local folder), timestamps
chats       → id, name, projectId, worktree fields, timestamps
sub_chats   → id, name, chatId, sessionId, mode, messages (JSON)
```

**Auto-migration**: On app start, `initDatabase()` runs migrations from `drizzle/` folder.

## Frontend Stack

| Technology | Purpose |
|------------|---------|
| Electron 33.4.5 | Desktop app shell |
| React 19 | UI framework |
| TypeScript 5.4.5 | Type safety |
| Tailwind CSS | Styling |
| Radix UI | Component library |
| Jotai | UI state (selected chat, sidebar) |
| Zustand | Sub-chat tabs (persisted to localStorage) |
| React Query | Server state via tRPC |
| tRPC + trpc-electron | Type-safe IPC |

## Current Status

### ✅ Completed

**Phase 1-4: Core Infrastructure**
- NATS infrastructure + Pool task queue (VecDeque → JetStream dual mode)
- Template engine + data flow pipeline (DagContext + {{var}} rendering)
- DAG event-driven scheduling (NATS pub/sub with fallback)
- Markdown orchestration (input/output/retry/timeout/priority)

**Phase 5: Inter-Agent Communication**
- Message router (detect @mentions, route via NATS)
- AgentMessagePayload for agent-to-agent messages
- NAPI bindings: `nats_route_agent_message` / `nats_scan_and_route_mentions`

**Phase 6: File Access Control**
- Token permission model (READ/WRITE/ADMIN) + path scope matching
- Two-level Token system (SystemToken admission + FileToken operations)
- SQLite persistent lock manager (transaction-safe, 5 tables)
- Lock upgrade/downgrade + renewal + heartbeat
- Watchdog progressive timeout + automatic reclamation
- Git snapshot (auto-create before WRITE, for rollback)
- Security audit log + sensitive path detection
- Conflict arbitration (WRITE conflict priority)
- Performance optimization (lock cache + BinaryHeap priority queue)
- Single agent mode auto-bypass (5s hysteresis, skip approval + conflict check)
- Approval flow (escalate → respond_approval interaction)
- READ_LATEST waiter model (notify readers after WRITE completes)
- Multi-project support (per-project lock_manager / snapshot_manager / watchdog)

**Phase 7: NATS File Event Streams**
- FILE_EVENTS JetStream stream (WorkQueue retention)
- FileEventsConsumer background processing (file.ready / file.error)
- Event notification API (notify_file_ready / notify_file_error)
- EventBus typed publishing (publish_file_ready / publish_file_error)

**Phase 8: Hosted Agent Configuration** (Latest)
- `hosted_config.rs` module for user-defined agent configs
- MCP server injection (system tools for agents)
- Security: path traversal protection, command injection prevention
- Avatar path validation, agent_base whitelist

### 🚧 In Progress

**Frontend Integration:**
- Replacing `mock-api.ts` with real tRPC calls
- Implementing DAG submission UI
- Real-time progress display (listen to NATS events)
- File conflict approval UI

**End-to-End Testing:**
- Complete multi-agent collaboration scenario tests
- Frontend → Backend → Agent integration tests
- Real DAG execution flow verification

### ❌ Known Issues

**Test Isolation:**
- `test_global_dag_scheduler_lifecycle` fails intermittently when run with other tests (shared global state issue)
- Passes when run individually
- Not caused by recent changes, pre-existing issue

## Code Statistics

- **Rust**: ~30,000 lines across 92 files
- **Tests**: 418 passing (unit tests, exclude `agent::discovery` which hangs)
- **TypeScript**: Frontend + Main process

## Debugging Tips

### First Install Issues

When testing auth flows or fresh install behavior:

```bash
# Clear all app data
rm -rf ~/Library/Application\ Support/Agents\ Dev/

# Clear preferences
defaults delete dev.21st.agents.dev  # Dev mode
defaults delete dev.21st.agents      # Production

# Run in dev mode
bun run dev
```

**Common bugs:**
- OAuth deep link not working on first launch (macOS Launch Services delay)
- Folder dialog not appearing (window focus timing)

### Debug Mode

```bash
# Start debug server
bun packages/debug/src/server.ts &

# Instrument renderer code (no import needed)
fetch('http://localhost:7799/log',{
  method:'POST',
  headers:{'Content-Type':'application/json'},
  body:JSON.stringify({tag:'TAG',msg:'MESSAGE',data:{},ts:Date.now()})
}).catch(()=>{});

# Read logs
cat .debug/logs.ndjson

# Clear logs
curl -X DELETE http://localhost:7799/logs
```

See `packages/debug/INSTRUCTIONS.md` for full protocol.

## File Naming Conventions

- **Components**: PascalCase (`ActiveChat.tsx`, `AgentsSidebar.tsx`)
- **Utilities/hooks**: camelCase (`useFileUpload.ts`, `formatters.ts`)
- **Stores**: kebab-case (`sub-chat-store.ts`, `agent-chat-store.ts`)
- **Atoms**: camelCase with `Atom` suffix (`selectedAgentChatIdAtom`)

## Important Files

### Frontend
- `src/renderer/App.tsx` - Root with providers
- `src/renderer/features/agents/main/active-chat.tsx` - Main chat component
- `src/renderer/features/agents/atoms/index.ts` - Agent UI state atoms
- `src/main/lib/trpc/routers/claude.ts` - Claude SDK integration

### Backend
- `src/main/lib/db/schema/index.ts` - Drizzle schema (source of truth)
- `src/main/lib/db/index.ts` - DB initialization + auto-migrate

### Rust
- `src-rust/src/lib.rs` - Library entry point
- `src-rust/src/file_access/lock_manager.rs` - Core lock management (largest file)
- `src-rust/src/acp/sdk_session.rs` - ACP session lifecycle
- `src-rust/src/nats/manager.rs` - Global NATS state

## Building & Releasing

### Prerequisites (macOS notarization)

```bash
# Create keychain profile
xcrun notarytool store-credentials "21st-notarize" \
  --apple-id YOUR_APPLE_ID \
  --team-id YOUR_TEAM_ID
```

### Release Flow

```bash
# Full release
bun run release

# Or step by step
bun run build
bun run package:mac
bun run dist:manifest
./scripts/upload-release-wrangler.sh

# After release
# 1. Wait for notarization (2-5 min)
# 2. Staple DMGs: cd release && xcrun stapler staple *.dmg
# 3. Re-upload stapled DMGs
# 4. Update changelog: gh release edit v0.0.X --notes "..."
# 5. Upload manifests (triggers auto-updates)
# 6. Sync to public: ./scripts/sync-to-public.sh
```

### Version Bump

```bash
npm version patch --no-git-tag-version  # 0.0.27 → 0.0.28
```

## Tech Stack Summary

| Layer | Technology |
|-------|-----------|
| Desktop | Electron 33.4.5, electron-vite, electron-builder |
| UI | React 19, TypeScript 5.4.5, Tailwind CSS |
| Components | Radix UI, Lucide icons, Motion, Sonner |
| State | Jotai, Zustand, React Query |
| Backend | tRPC, Drizzle ORM, better-sqlite3 |
| AI | ACP Protocol (agent-client-protocol SDK) |
| Agent Communication | NATS (async-nats 0.38) + nats-server subprocess |
| DAG Orchestration | Custom TaskGraph + template engine + DagContext |
| Package Manager | bun |

---
name: ergatai-patterns
description: "Use when working in ergatai, especially before editing Rust crates (ergatai-core, ergatai-api, ergatai-collab, ergatai-dag, ergatai-lock, ergatai-nats, ergatai-agent, ergatai-error, ergatai-runtime), writing commit messages, adding tests, or updating architecture docs — conventions measured from 200 commits of git history"
metadata:
  version: "1.0.0"
  source: local-git-analysis
  analyzed_commits: "200"
---

# Ergatai Patterns

## Commit Conventions

Use **Conventional Commits** with lowercase type prefix. Follow the pattern:

```
<type>: <short description>
```

### Type Distribution (from 200 commits)

| Type | Frequency | Usage |
|------|-----------|-------|
| `feat:` | 33 | New features (MCP server, NATS streams, DAG scheduler) |
| `refactor:` | 16 | Architecture changes, crate extraction |
| `fix:` | 10 | Bug fixes (watchdog, lock reclamation, security) |
| `docs:` | 8 | CLAUDE.md, README, architecture docs |
| `style:` | 5 | Formatting, clippy warnings |
| `test:` | 2 | Adding test coverage |
| `chore:` | 2 | Dependency updates, cleanup |

### Commit Rules

- **Lowercase prefix**: `feat:`, not `Feat:` or `FEAT:`
- **Scope optional**: `fix(file_access):` is valid but `fix:` alone is preferred
- **Description**: Imperative mood, no period at end
- **Language**: English preferred (some commits in Chinese accepted historically)

### Examples

```
feat: add ergatai-runtime crate with pluggable backends
fix: auto-cleanup dead agents via session timeout
refactor: remove ACP crate and add tmux injection support
docs: update CLAUDE.md for MCP notification architecture
```

## Code Architecture

### Workspace Structure

```
crates/
├── ergatai-core/       # Business logic facade, re-exports from sub-crates
├── ergatai-api/        # HTTP server with MCP endpoints
├── ergatai-collab/     # Multi-agent collaboration (DAG scheduler, task coordinator)
├── ergatai-dag/        # DAG parsing, template engine, context management
├── ergatai-nats/       # Embedded NATS server and event bus (JetStream)
├── ergatai-lock/       # File access control, token-based locking
├── ergatai-agent/      # Agent config, discovery, hosted agents
├── ergatai-error/      # Shared error types
└── ergatai-runtime/    # Pluggable backends (direct_process, local_pty, rmux)

examples/
└── simple-agent/       # Example agent demonstrating middleware usage

docs/
├── dev/                # Development docs (ARCHITECTURE.md, CODE_REVIEW_*.md)
└── *.md                # User-facing guides (ACP_SDK_GUIDE.md, etc.)
```

### Crate Dependency Rules

- **ergatai-error** is the leaf crate — no internal dependencies
- **ergatai-core** is the facade — re-exports from sub-crates, never duplicate logic
- **ergatai-api** depends on all other crates (HTTP server is the integration point)
- **ergatai-runtime** is independent — pluggable backends, no MCP/NATS coupling

### File Co-Change Patterns

Files that change together (update them atomically):

| Trigger | Files to Update |
|---------|-----------------|
| Lock manager changes | `lock_manager.rs`, `watchdog.rs`, `manager.rs`, `token.rs` |
| DAG scheduler changes | `dag_scheduler.rs`, `task_scheduler.rs`, `agent_launcher.rs` |
| MCP protocol changes | `server.rs`, `message_relay.rs`, `mod.rs` |
| Architecture change | `CLAUDE.md`, `README.md`, `docs/dev/ARCHITECTURE.md` |
| Dependency update | `Cargo.toml`, `Cargo.lock` |

## Testing Patterns

### Test Location

- **Integration tests**: `tests/integration_test.sh` (shell-based)
- **Inline tests**: `#[cfg(test)] mod tests` in source files (e.g., `multi_agent_tests.rs`)
- **Binary test utilities**: `crates/ergatai-core/src/bin/tmux-test.rs`

### Test Coverage Expectations

- Unit tests for critical logic (lock_manager, dag_scheduler) are expected
- Integration tests are shell-based (see `tests/integration_test.sh`)
- Test coverage is currently sparse — new code should include unit tests

### Running Tests

```bash
# All tests
cargo test --workspace

# Specific crate
cargo test -p ergatai-lock

# Single-threaded (avoids race conditions in shared global state)
cargo test --workspace -- --test-threads=1
```

## Documentation Patterns

### CLAUDE.md (Critical!)

- **Update on architecture changes** — this is the primary onboarding doc
- Include: architecture diagrams, NATS subject naming, crate responsibilities
- Keep in sync with actual code (stale CLAUDE.md is worse than none)

### docs/dev/

- **Architecture docs**: `ARCHITECTURE.md`, `ARCHITECTURE_DIAGRAM.md`
- **Code review records**: `CODE_REVIEW_2026-08-14.md`, `CODE_REVIEW_FIXES_COMPLETE.md`
- **Implementation summaries**: `IMPLEMENTATION_COMPLETE.md`, `PHASE2_IMPLEMENTATION.md`

### README.md

- User-facing, focuses on quick start and examples
- Update when MCP interface changes (tool signatures, agent setup)
- Keep examples minimal and copy-pasteable

## Development Workflow

### Adding a New Crate

1. Create `crates/ergatai-<name>/` with `Cargo.toml` and `src/lib.rs`
2. Add to workspace `Cargo.toml` under `members`
3. Add re-export in `ergatai-core/src/lib.rs` if it's part of the facade
4. Update `CLAUDE.md` workspace structure section
5. Commit with `feat: add ergatai-<name> crate`

### Modifying Lock Manager

Always update these files together:
- `crates/ergatai-lock/src/lock_manager.rs` (main logic)
- `crates/ergatai-lock/src/watchdog.rs` (heartbeat monitoring)
- `crates/ergatai-lock/src/manager.rs` (orchestration)
- `crates/ergatai-lock/src/token.rs` (token types)
- `crates/ergatai-lock/src/multi_agent_tests.rs` (test coverage)

### Modifying DAG Scheduler

Always update these files together:
- `crates/ergatai-collab/src/dag_scheduler.rs` (DAG execution)
- `crates/ergatai-collab/src/task_scheduler.rs` (task execution)
- `crates/ergatai-collab/src/agent_launcher.rs` (agent process management)

### MCP Protocol Changes

When changing MCP tools or notifications:
1. Update `crates/ergatai-api/src/mcp/server.rs` (tool handlers)
2. Update `crates/ergatai-api/src/mcp/message_relay.rs` (notification push)
3. Update `examples/simple-agent/src/main.rs` (example usage)
4. Update `CLAUDE.md` MCP Tools section
5. Update `README.md` if user-facing API changed

## Security Conventions

- **Token-based locking**: Two-level system (SystemToken + FileToken)
- **Sensitive paths**: `crates/ergatai-lock/src/sensitive_paths.rs` (never edit without review)
- **Audit logging**: `crates/ergatai-lock/src/audit.rs` (all lock operations logged)
- **Git snapshots**: Automatic before file writes (`snapshot.rs`)

## NATS Subject Naming

Follow the established pattern:

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

## License

Apache-2.0 (corrected from MIT in commit `a91423f`). Do not add MIT headers.

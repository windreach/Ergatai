# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is this?

**21st Agents** - A local-first Electron desktop app for AI-powered code assistance. Users create chat sessions linked to local project folders, interact with Claude in Plan or Agent mode, and see real-time tool execution (bash, file edits, web search, etc.).

## Commands

```bash
# Development
bun run dev              # Start Electron with hot reload

# Build
bun run build            # Compile app
bun run package          # Package for current platform (dir)
bun run package:mac      # Build macOS (DMG + ZIP)
bun run package:win      # Build Windows (NSIS + portable)
bun run package:linux    # Build Linux (AppImage + DEB)

# Database (Drizzle + SQLite)
bun run db:generate      # Generate migrations from schema
bun run db:push          # Push schema directly (dev only)
```

## Architecture Principles

**分层职责：**

| 层 | 语言 | 职责 |
|---|------|------|
| **Rust 后端** (`src-rust/`) | Rust | 核心逻辑、性能关键路径、安全保障 |
| **TypeScript 后端** (`src/main/`) | TypeScript | 表层业务逻辑、调用 Rust（通过 NAPI） |
| **前端** (`src/renderer/`) | TypeScript | UI 层 |

**Fork 原则：**
- 前端 + 后端 TS 是从 21st Agents fork 的，可能前后端不一致
- **默认原则：以后端（`src/main/`）为主**
- 发现不一致时，告诉用户进行取舍决策

**调用链：**
```
Frontend (TS) → tRPC → Main (TS) → NAPI → Rust (核心逻辑)
```

## Architecture

### 通信架构（重要！）

```
用户
 │ "重构这个模块"
 ▼
主 Agent (claude-code) ←── ACP ──→ Ergatai (Rust)
 │ 输出 DAG markdown
 ▼
Ergatai 解析 DAG → NATS 分发任务 → 子 Agent A/B/C (ACP 执行)
                   ↑ NATS 事件回传完成
```

**两层通信，各管各的：**

| 层 | 技术 | 方向 | 内容 |
|---|------|------|------|
| **Agent ↔ Ergatai** | ACP (JSON-RPC over stdin/stdout) | 双向 | prompt、response、tool call |
| **Ergatai 内部组件** | NATS (事件总线) | 事件流 | task.submit、node.complete |

- **ACP** = Client(Ergatai) ↔ Agent 协议，双向
- **NATS** = Ergatai 内部事件总线，保证消息可靠传递
- **NATS 不跟 Agent 说话**，Agent 之间也不能直接对话
- Agent 间如需对话，必须经 Ergatai 中转（Phase 5）

### 多 Agent 协作基础设施

项目为多 Agent 并行协作提供完整的安全隔离和协调机制，确保多个 AI Agent 可以同时安全地操作同一项目目录。

#### 文件访问控制（Phase 6）

多 Agent 并行修改代码的核心挑战是**文件冲突**。系统通过 Token 机制实现文件级锁管理：

```
Agent A (WRITE lock on src/foo.rs)
    ↓ 持有 token
    ↓ 修改文件
    ↓ 创建 git snapshot
    ↓ 释放锁
Agent B (等待 WRITE lock → 获取 → 继续)
```

**Token 权限模型：**

| Mode | 权限 | 用途 |
|------|------|------|
| `READ` | 只读，多个 READ 可并存 | 代码分析、审查 |
| `WRITE` | 独占写，同一文件只能一个 WRITE | 代码修改 |
| `ADMIN` | 完全访问（含敏感路径） | 配置文件、密钥操作 |

**核心模块（`src-rust/src/file_access/`）：**

| 模块 | 职责 |
|------|------|
| `token.rs` | FileToken / SystemToken 数据结构 + 路径范围匹配 |
| `lock_manager.rs` | SQLite 持久化锁管理（BEGIN IMMEDIATE 事务） |
| `lock_mode.rs` | 锁升级（READ→WRITE）/ 降级（WRITE→READ） |
| `renewal.rs` | Token / Lock 续期（心跳延续有效期） |
| `audit.rs` | 安全审计日志（所有锁操作记录） |
| `snapshot.rs` | Git blob 快照（WRITE 前自动创建，用于回滚） |
| `watchdog.rs` | 后台监控：心跳超时 → 渐进式超时 → 自动回收锁 |
| `watcher.rs` | 文件系统监听（notify crate），检测锁外修改 |
| `conflict_arbitration.rs` | WRITE 冲突仲裁（优先级决策） |
| `sensitive_paths.rs` | 敏感路径检测（.env, .key, credentials 等需 ADMIN） |
| `performance.rs` | 锁缓存 + 批量操作 + 异步队列优化 |
| `file_events_consumer.rs` | JetStream 消费者（file.ready / file.error 事件） |
| `manager.rs` | 全局 FileLockManager 初始化（类似 NatsManager） |

**锁数据库（SQLite）：**

```
{project_root}/.ergatai/locks.db

Tables:
- system_tokens   → Agent session 级别的 token（id, agent_id, session_id, expires_at, heartbeat_at）
- file_locks      → 文件级锁（token_id, file_path, mode, status, expires_at）
- audit_log       → 审计日志（timestamp, agent_id, action, file_path, mode）
- snapshots       → Git 快照记录（file_path, git_hash, agent_id, created_at）
```

**Watchdog 渐进式超时：**

```
心跳超时检测（每 10s）
├── 第 1 次超时：warn + 30s 宽限期
├── 第 2 次超时：error + 60s 宽限期
└── 第 3 次超时：回收所有锁 + 广播 file.error 事件
```

Agent 执行长任务时可调用 `mark_busy(session_id, duration_secs)` 延长超时。

#### NATS 文件事件流（Phase 7）

JetStream 提供持久化的文件事件通知，确保 Agent 间的文件就绪通知不丢失：

```
FILE_EVENTS Stream (WorkQueue retention)
├── ergatai.file.ready.{md5_hash}    → WRITE 完成，通知 READ_LATEST 等待者
└── ergatai.file.error.{md5_hash}    → WRITE 失败/崩溃，通知等待者
```

**事件流保证：**
- `AckPolicy::Explicit` — 消费者必须确认，否则重投（最多 3 次）
- `WorkQueue` 保留策略 — 消息处理后自动删除
- `FileEventsConsumer` 后台消费事件并调用 `lock_manager.notify_file_ready/error`

#### Agent 间消息路由（Phase 5）

Agent 之间不能直接通信，所有消息经 Ergatai 中转：

```
Agent A: "@agent-b 请review这段代码"
    ↓
Ergatai message_router.rs 检测 @mention
    ↓
NATS publish → ergatai.agent.message.{agent-b}
    ↓
Agent B 通过 ACP 收到消息
```

#### 完整的多 Agent 协作流程

```
1. 用户请求: "用 3 个 Agent 并行重构这个模块"
    ↓
2. 主 Agent (claude-code) 输出 DAG markdown:
   ## Task A (分析代码) - agent-a
   ## Task B (写实现) - agent-b, depends_on: [A]
   ## Task C (写测试) - agent-c, depends_on: [A]
    ↓
3. Ergatai 解析 DAG → DagScheduler
    ↓
4. Task A 无依赖 → 立即提交到 NATS: ergatai.task.submit.agent-a
    ↓
5. TaskScheduler 消费任务 → AgentLauncher 启动 agent-a
   ├── 创建 SystemToken (session 级)
   ├── 创建 FileToken (mode=WRITE, scope=src/module/)
   └── acquire_lock(src/module/*.rs)
    ↓
6. Agent A 通过 ACP 执行任务
   ├── 每次文件操作 → NAPI → FileLockManager 检查权限
   ├── WRITE 前自动创建 git snapshot
   └── 定期 heartbeat 维持 token 有效
    ↓
7. Task A 完成 → NATS: ergatai.dag.node_complete.A
    ↓
8. DagScheduler 检查依赖 → Task B/C 解锁 → 并行提交
    ↓
9. Agent B/C 并行执行（各自持有不同文件的 WRITE 锁）
    ↓
10. 所有任务完成 → NATS: ergatai.dag.complete.{dag_id}
    ↓
11. 主 Agent 汇总结果 → 回复用户
```

#### 文件访问 NAPI 接口

```typescript
// 初始化（应用启动时）
file_access_init(project_root: string): void

// Token 管理
file_access_create_token(agent_id, session_id, project_root, mode, scope): FileToken
file_access_acquire_lock(token, file_path): void
file_access_release_lock(token_id, file_path): void
file_access_heartbeat(token_id): void

// 锁模式切换
file_access_upgrade_to_write(token_id, file_path): void
file_access_downgrade_to_read(token_id, file_path): void

// 快照
file_access_create_snapshot(file_path, agent_id): string  // 返回 git hash
file_access_get_latest_snapshot(file_path): string | null

// 审计
file_access_generate_security_report(): FileAccessStats
file_access_get_audit_log(agent_id?, action?, limit?): AuditEntry[]

// 监控
file_access_mark_busy(session_id, duration_secs): void
file_access_get_active_locks(file_path?): ActiveLock[]
```

### Rust 后端 (`src-rust/`)

```
src-rust/src/
├── acp/                     # ACP 协议层
│   ├── manager.rs           # Session 管理、事件总线 (poll_events)
│   ├── sdk_session.rs       # 单个 ACP session 生命周期
│   ├── sdk_pool_manager.rs  # Agent pool: NATS 任务队列 + 调度
│   └── session_ops.rs       # Session 操作（加载/恢复/权限处理）
│
├── file_access/             # 文件访问控制（多 Agent 安全隔离）
│   ├── token.rs             # FileToken / SystemToken 数据结构
│   ├── lock_manager.rs      # SQLite 持久化锁管理
│   ├── lock_mode.rs         # 锁升级/降级
│   ├── renewal.rs           # Token/Lock 续期
│   ├── audit.rs             # 安全审计日志
│   ├── snapshot.rs          # Git blob 快照（防崩溃回滚）
│   ├── watchdog.rs          # 心跳超时监控 + 自动回收
│   ├── watcher.rs           # 文件系统监听（notify）
│   ├── conflict_arbitration.rs # WRITE 冲突仲裁
│   ├── sensitive_paths.rs   # 敏感路径检测
│   ├── performance.rs       # 锁缓存 + 批量操作优化
│   ├── file_events_consumer.rs # JetStream 事件消费者
│   ├── manager.rs           # 全局 FileLockManager 初始化
│   └── mod.rs               # 模块导出
│
├── nats/                    # NATS 集成（事件总线）
│   ├── server.rs            # nats-server 子进程管理
│   ├── connection.rs        # async-nats client 封装
│   ├── task_queue.rs        # JetStream WorkQueue
│   ├── events.rs            # 事件 payload 定义（DAG + 文件访问）
│   ├── event_bus.rs         # 类型化 pub/sub 封装
│   ├── file_access_streams.rs # FILE_EVENTS JetStream 流定义
│   └── manager.rs           # 全局 NATS 状态 (init/shutdown)
│
├── orchestration/           # DAG 编排
│   ├── dag_topology.rs      # TaskGraph / TaskNode 数据结构
│   ├── dag_parser.rs        # Markdown → TaskGraph 解析器
│   ├── template.rs          # {{var}} 模板引擎
│   └── context.rs           # DagContext (全局变量 + 节点输出)
│
├── cross_agent/             # 跨 Agent 协调
│   ├── dag_scheduler.rs     # DAG 事件驱动调度器
│   ├── task_scheduler.rs    # 任务调度 (NATS consumer / 轮询 fallback)
│   ├── agent_launcher.rs    # Agent 启动 + 完成检测
│   └── task_coordinator.rs  # Plan 文件解析 + AgentAssignment
│
├── agent/                   # Agent 发现与配置
│   ├── config.rs            # Agent JSON 配置加载（含路径遍历防护）
│   ├── discovery.rs         # 自动发现已安装 agent
│   └── runtime_metadata.rs  # 13 个内置 agent 元数据
│
├── error/                   # 错误类型
│   └── types.rs             # ErgataiError 枚举（含文件访问相关变体）
│
└── napi/                    # NAPI 绑定 (Rust → TypeScript)
    ├── nats.rs              # nats_init / nats_is_initialized / nats_shutdown
    ├── file_access.rs       # 文件访问控制 NAPI（FFI 边界）
    └── ...
```

### NATS Subject 命名规范

```
ergatai.
├── task.submit.{agent}              # DagScheduler → TaskScheduler (任务提交)
├── task.complete.{task_id}          # Agent 完成通知
├── task.fail.{task_id}              # Agent 失败通知
├── dag.node_complete.{node}         # AgentLauncher → DagScheduler
├── dag.node_failed.{node}           # AgentLauncher → DagScheduler
├── dag.complete.{dag_id}            # DAG 全部完成
├── agent.spawned.{agent_id}         # Agent 启动
├── agent.stopped.{agent_id}         # Agent 停止
├── agent.message.{agent_id}         # Agent 间消息路由（@mention）
│
├── file.access.request              # 文件访问请求
├── file.access.grant                # 文件访问授权
├── file.access.deny                 # 文件访问拒绝
├── file.access.approve              # 管理员审批通过
├── file.access.revoke               # 文件访问撤销
├── file.ready.{md5_hash}            # 文件 WRITE 完成通知 (JetStream)
└── file.error.{md5_hash}            # 文件 WRITE 失败通知 (JetStream)
```

**JetStream Streams：**

| Stream | Subjects | Retention | 用途 |
|--------|----------|-----------|------|
| `FILE_EVENTS` | `ergatai.file.ready.*`, `ergatai.file.error.*` | WorkQueue | 文件事件持久化，保证不丢失 |
| `TASK_QUEUE` | `ergatai.task.submit.*` | WorkQueue | 任务分发，Agent 消费 |

### DAG 编排流程

```markdown
## Task A (分析代码)
- **agent**: agent-a
- **task**: tasks/analyze.md

## Task B (写测试)
- **agent**: agent-b
- **task**: tasks/test.md
- **depends_on**: [Task A]
- **input**: 分析结果: {{TaskA.review_result}}
- **output**: test_result, coverage
- **retry**: 3
- **timeout**: 300
```

- `{{global.*}}` = 全局变量（DagContext.global_vars）
- `{{node_id.*}}` = 上游节点输出（DagContext.node_outputs）
- 模板在 `generate_node_plan()` 时自动渲染

### Frontend / Main / Renderer

```
src/
├── main/                    # Electron main process
│   ├── index.ts             # App entry, window lifecycle
│   ├── auth-manager.ts      # OAuth flow, token refresh
│   ├── auth-store.ts        # Encrypted credential storage (safeStorage)
│   ├── windows/main.ts      # Window creation, IPC handlers
│   └── lib/
│       ├── db/              # Drizzle + SQLite
│       │   ├── index.ts     # DB init, auto-migrate on startup
│       │   ├── schema/      # Drizzle table definitions
│       │   └── utils.ts     # ID generation
│       └── trpc/routers/    # tRPC routers (projects, chats, claude)
│
├── preload/                 # IPC bridge (context isolation)
│   └── index.ts             # Exposes desktopApi + tRPC bridge
│
└── renderer/                # React 19 UI
    ├── App.tsx              # Root with providers
    ├── features/
    │   ├── agents/          # Main chat interface
    │   │   ├── main/        # active-chat.tsx, new-chat-form.tsx
    │   │   ├── ui/          # Tool renderers, preview, diff view
    │   │   ├── commands/    # Slash commands (/plan, /agent, /clear)
    │   │   ├── atoms/       # Jotai atoms for agent state
    │   │   └── stores/      # Zustand store for sub-chats
    │   ├── sidebar/         # Chat list, archive, navigation
    │   ├── sub-chats/       # Tab/sidebar sub-chat management
    │   └── layout/          # Main layout with resizable panels
    ├── components/ui/       # Radix UI wrappers (button, dialog, etc.)
    └── lib/
        ├── atoms/           # Global Jotai atoms
        ├── stores/          # Global Zustand stores
        ├── trpc.ts          # Real tRPC client
        └── mock-api.ts      # DEPRECATED - being replaced with real tRPC
```

## Database (Drizzle ORM)

**Location:** `{userData}/data/agents.db` (SQLite)

**Schema:** `src/main/lib/db/schema/index.ts`

```typescript
// Three main tables:
projects    → id, name, path (local folder), timestamps
chats       → id, name, projectId, worktree fields, timestamps
sub_chats   → id, name, chatId, sessionId, mode, messages (JSON)
```

**Auto-migration:** On app start, `initDatabase()` runs migrations from `drizzle/` folder (dev) or `resources/migrations` (packaged).

**Queries:**
```typescript
import { getDatabase, projects, chats } from "../lib/db"
import { eq } from "drizzle-orm"

const db = getDatabase()
const allProjects = db.select().from(projects).all()
const projectChats = db.select().from(chats).where(eq(chats.projectId, id)).all()
```

## Key Patterns

### IPC Communication
- Uses **tRPC** with `trpc-electron` for type-safe main↔renderer communication
- All backend calls go through tRPC routers, not raw IPC
- Preload exposes `window.desktopApi` for native features (window controls, clipboard, notifications)

### State Management
- **Jotai**: UI state (selected chat, sidebar open, preview settings)
- **Zustand**: Sub-chat tabs and pinned state (persisted to localStorage)
- **React Query**: Server state via tRPC (auto-caching, refetch)

### Claude Integration
- Dynamic import of `@anthropic-ai/claude-code` SDK
- Two modes: "plan" (read-only) and "agent" (full permissions)
- Session resume via `sessionId` stored in SubChat
- Message streaming via tRPC subscription (`claude.onMessage`)

## Tech Stack

| Layer | Tech |
|-------|------|
| Desktop | Electron 33.4.5, electron-vite, electron-builder |
| UI | React 19, TypeScript 5.4.5, Tailwind CSS |
| Components | Radix UI, Lucide icons, Motion, Sonner |
| State | Jotai, Zustand, React Query |
| Backend | tRPC, Drizzle ORM, better-sqlite3 |
| AI | ACP Protocol (agent-client-protocol SDK) |
| Agent 通信 | NATS (async-nats 0.38) + nats-server 子进程 |
| DAG 编排 | 自研 TaskGraph + 模板引擎 + DagContext |
| Package Manager | bun |

## Rust 测试

```bash
# 全部库测试（排除已知挂起的 agent::discovery）
cargo test --lib -- --skip agent::discovery

# 特定模块测试
cargo test --lib orchestration          # 模板引擎 + DAG 解析 (37 测试)
cargo test --lib cross_agent::dag       # DAG 调度器 (6 测试)
cargo test --lib nats::events           # 事件类型序列化 (8 测试)
cargo test --lib nats::event_bus        # 事件总线 (需要 nats-server)
cargo test --lib file_access            # 文件访问控制 (20+ 测试)
cargo test --lib agent::config          # Agent 配置加载 (28 测试)

# NATS 集成测试需要 nats-server 二进制
# 设置: export ERGATAI_NATS_BINARY=/path/to/nats-server
```

## Current Status

**已完成：**
- ✅ Phase 1: NATS 基础设施 + Pool 任务队列（VecDeque → JetStream 双模式）
- ✅ Phase 2: 模板引擎 + 数据流管线（DagContext + `{{var}}` 渲染）
- ✅ Phase 3: DAG 事件驱动（NATS pub/sub 替代直接调用，fallback 保留）
- ✅ Phase 4: Markdown 编排增强（input/output/retry/timeout/priority）

**已完成 (Phase 5)：Agent 间双向对话**
- ✅ 消息路由器（`message_router.rs`）：检测 @mentions，通过 NATS 路由
- ✅ AgentMessagePayload：agent-to-agent 消息类型
- ✅ NAPI 绑定：`nats_route_agent_message` / `nats_scan_and_route_mentions`
- ✅ Subject: `ergatai.agent.message.{agent_id}`

**已完成 (Phase 6)：文件访问控制**
- ✅ Token 权限模型（READ / WRITE / ADMIN）+ 路径范围匹配
- ✅ SQLite 持久化锁管理（事务保证原子性）
- ✅ 锁升级/降级 + 续期 + 心跳
- ✅ Watchdog 渐进式超时 + 自动回收
- ✅ Git snapshot 快照（WRITE 前自动创建，用于回滚）
- ✅ 安全审计日志 + 敏感路径检测
- ✅ 冲突仲裁（WRITE 冲突优先级决策）
- ✅ 性能优化（锁缓存 + BinaryHeap 优先队列 + 批量操作）

**已完成 (Phase 7)：NATS 文件事件流**
- ✅ FILE_EVENTS JetStream 流（WorkQueue retention）
- ✅ FileEventsConsumer 后台消费（file.ready / file.error）
- ✅ 事件通知 API（notify_file_ready / notify_file_error）
- ✅ EventBus 类型化发布（publish_file_ready / publish_file_error）

**进行中：**
- 前端 mock-api.ts → 真实 tRPC 调用替换

## File Naming

- Components: PascalCase (`ActiveChat.tsx`, `AgentsSidebar.tsx`)
- Utilities/hooks: camelCase (`useFileUpload.ts`, `formatters.ts`)
- Stores: kebab-case (`sub-chat-store.ts`, `agent-chat-store.ts`)
- Atoms: camelCase with `Atom` suffix (`selectedAgentChatIdAtom`)

## Important Files

- `electron.vite.config.ts` - Build config (main/preload/renderer entries)
- `src/main/lib/db/schema/index.ts` - Drizzle schema (source of truth)
- `src/main/lib/db/index.ts` - DB initialization + auto-migrate
- `src/renderer/features/agents/atoms/index.ts` - Agent UI state atoms
- `src/renderer/features/agents/main/active-chat.tsx` - Main chat component
- `src/main/lib/trpc/routers/claude.ts` - Claude SDK integration

## Debugging First Install Issues

When testing auth flows or behavior for new users, you need to simulate a fresh install:

```bash
# 1. Clear all app data (auth, database, settings)
rm -rf ~/Library/Application\ Support/Agents\ Dev/

# 2. Reset macOS protocol handler registration (if testing deep links)
/System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/LaunchServices.framework/Versions/A/Support/lsregister -kill -r -domain local -domain system -domain user

# 3. Clear app preferences
defaults delete dev.21st.agents.dev  # Dev mode
defaults delete dev.21st.agents      # Production

# 4. Run in dev mode with clean state
cd apps/desktop
bun run dev
```

**Common First-Install Bugs:**
- **OAuth deep link not working**: macOS Launch Services may not immediately recognize protocol handlers on first app launch. User may need to click "Sign in" again after the first attempt.
- **Folder dialog not appearing**: Window focus timing issues on first launch. Fixed by ensuring window focus before showing `dialog.showOpenDialog()`.

**Dev vs Production App:**
- Dev mode uses `twentyfirst-agents-dev://` protocol
- Dev mode uses separate userData path (`~/Library/Application Support/Agents Dev/`)
- This prevents conflicts between dev and production installs

## Releasing a New Version

### Prerequisites for Notarization

- Keychain profile: `21st-notarize`
- Create with: `xcrun notarytool store-credentials "21st-notarize" --apple-id YOUR_APPLE_ID --team-id YOUR_TEAM_ID`

### Release Commands

```bash
# Full release (build, sign, submit notarization, upload to CDN)
bun run release

# Or step by step:
bun run build              # Compile TypeScript
bun run package:mac        # Build & sign macOS app
bun run dist:manifest      # Generate latest-mac.yml manifests
./scripts/upload-release-wrangler.sh  # Submit notarization & upload to R2 CDN
```

### Bump Version Before Release

```bash
npm version patch --no-git-tag-version  # 0.0.27 → 0.0.28
```

### After Release Script Completes

1. Wait for notarization (2-5 min): `xcrun notarytool history --keychain-profile "21st-notarize"`
2. Staple DMGs: `cd release && xcrun stapler staple *.dmg`
3. Re-upload stapled DMGs to R2 and GitHub (see RELEASE.md for commands)
4. Update changelog: `gh release edit v0.0.X --notes "..."`
5. **Upload manifests (triggers auto-updates!)** — see RELEASE.md
6. Sync to public: `./scripts/sync-to-public.sh`

### Files Uploaded to CDN

| File | Purpose |
|------|---------|
| `latest-mac.yml` | Manifest for arm64 auto-updates |
| `latest-mac-x64.yml` | Manifest for Intel auto-updates |
| `1Code-{version}-arm64-mac.zip` | Auto-update payload (arm64) |
| `1Code-{version}-mac.zip` | Auto-update payload (Intel) |
| `1Code-{version}-arm64.dmg` | Manual download (arm64) |
| `1Code-{version}.dmg` | Manual download (Intel) |

### Auto-Update Flow

1. App checks `https://cdn.21st.dev/releases/desktop/latest-mac.yml` on startup and when window regains focus (with 1 min cooldown)
2. If version in manifest > current version, shows "Update Available" banner
3. User clicks Download → downloads ZIP in background
4. User clicks "Restart Now" → installs update and restarts

## Debug Mode

When debugging runtime issues in the renderer or main process, use the structured debug logging system. This avoids asking the user to manually copy-paste console output.

**Start the server:**
```bash
bun packages/debug/src/server.ts &
```

**Instrument renderer code** (no import needed, fails silently):
```js
fetch('http://localhost:7799/log',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({tag:'TAG',msg:'MESSAGE',data:{},ts:Date.now()})}).catch(()=>{});
```

**Read logs:** Read `.debug/logs.ndjson` - each line is a JSON object with `tag`, `msg`, `data`, `ts`.

**Clear logs:** `curl -X DELETE http://localhost:7799/logs`

**Workflow:** Hypothesize → instrument → user reproduces → read logs → fix with evidence → verify → remove instrumentation.

See `packages/debug/INSTRUCTIONS.md` for the full protocol.

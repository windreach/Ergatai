# CLAUDE.md — Ergatai

多 agent 协作中间件。将独立运行的 AI 编码助手（如 OpenCode）组织成协作团队，通过 rmux pane 注入实现消息投递。

---

## 通信数据流（核心）

### 消息发送流程

```
Agent A (rmux pane)
  │  通过 MCP 协议调用 send_message tool
  ▼
┌──────────────────────────────────────────────────────────────────┐
│  ① MCP Server — send_message()                    [server.rs]   │
│     解析 target_agent_id，在 AgentRuntime registry 查找目标      │
│     构建 AgentMessagePayload，发布到 NATS JetStream              │
│     返回 { status: "queued" }                                    │
└──────────────────────────────────────────────────────────────────┘
  │
  ▼
┌──────────────────────────────────────────────────────────────────┐
│  ② NATS JetStream — AGENT_MESSAGES stream          [nats/]      │
│     subject: ergatai.agent.message.{target_agent_id}             │
│     持久化到文件，WorkQueue 保留策略，24h TTL                      │
│     配置: agent_message_stream.rs                                │
│     发布: event_bus.rs::publish_agent_message_reliable()         │
└──────────────────────────────────────────────────────────────────┘
  │
  ▼
┌──────────────────────────────────────────────────────────────────┐
│  ③ Message Delivery Consumer                       [message_delivery.rs]
│     后台 pull consumer 从 JetStream 拉取消息                      │
│     反序列化 AgentMessagePayload                                  │
│     调用 AgentRuntime.inject_message()                            │
│     成功 → ack()  /  失败 → nak() 重试 (最多 20 次, 30s 间隔)    │
└──────────────────────────────────────────────────────────────────┘
  │
  ▼
┌──────────────────────────────────────────────────────────────────┐
│  ④ AgentRuntime — inject_message()                  [runtime.rs] │
│     在 registry 中查找 agent_id → 获取 AgentHandle               │
│     委托给 backend.inject_message()                               │
└──────────────────────────────────────────────────────────────────┘
  │
  ▼
┌──────────────────────────────────────────────────────────────────┐
│  ⑤ RmuxBackend — inject_message()                 [rmux.rs]     │
│     从 panes_map 查找 Pane handle                                │
│     sanitize_message() (去换行, 截断 64KiB)                       │
│     pane.send_text() → rmux daemon → 写入 pane 终端              │
│     等同于 rmux send-keys -t {pane_id} "{message}\n"             │
└──────────────────────────────────────────────────────────────────┘
  │
  ▼
Agent B 的 rmux pane 中显示消息
```

### Agent 发现与注册

```
┌──────────────────────────────────────────────────────────────────┐
│  启动时 + 每 30 秒                                 [main.rs]     │
│     discover_and_register_agents()                                │
└──────────────────────────────────────────────────────────────────┘
  │
  ▼
┌──────────────────────────────────────────────────────────────────┐
│  RmuxBackend::discover_agents()                   [rmux.rs]     │
│     rmux.list_sessions() → 获取所有 session                       │
│     rmux.find_panes().all() → 获取所有 pane                       │
│     过滤: 跳过非 Running 状态的 pane                              │
│     过滤: 跳过 session 名以 _ 开头的 (warmup session)             │
│     对每个 pane:                                                  │
│       从 PaneProcessState::Running { pid } 提取子进程 PID          │
│       读 /proc/{pid}/environ 获取 RMUX_PANE 环境变量               │
│       用 RMUX_PANE (如 "%15") 作为 agent_id (确定性绑定)           │
│       fallback: 读不到则用 "pane_N"                               │
│     注册到 AgentRuntime registry                                  │
└──────────────────────────────────────────────────────────────────┘
```

### Agent ID 体系

| ID 来源 | 格式 | 说明 |
|---------|------|------|
| RMUX_PANE 环境变量 | `%15`, `%16` | **确定性 ID**，rmux 自动注入到 pane 进程，从 `/proc/{pid}/environ` 读取 |
| fallback | `pane_0`, `pane_1` | 读不到 RMUX_PANE 时按发现顺序编号（不稳定） |
| MCP client | `opencode@a1b2c3d4` | MCP 连接时自动生成，仅用于 MCP peer registry |

**关键**: agent 之间发消息使用 RMUX_PANE 值（如 `%15`）作为 target_agent_id。

---

## 项目结构

```
crates/
├── ergatai-api/        HTTP/MCP 服务器入口
│   └── src/mcp/
│       ├── server.rs             MCP 工具实现 (send_message, list_agents 等)
│       ├── message_delivery.rs   NATS consumer → AgentRuntime 投递
│       └── mod.rs
├── ergatai-runtime/    Agent 运行时 (发现、注入、生命周期)
│   └── src/backends/
│       ├── rmux.rs              rmux backend (发现 + 消息注入)
│       ├── local_pty.rs         tmux/pty backend (legacy)
│       └── direct_process.rs    直接进程 backend
├── ergatai-nats/       嵌入式 NATS 服务器 + JetStream 事件总线
├── ergatai-collab/     多 agent 协作 (DAG 调度、任务协调)
├── ergatai-dag/        DAG 解析和模板引擎
├── ergatai-lock/       文件访问控制 (token-based locking)
├── ergatai-agent/      Agent 配置和发现
├── ergatai-core/       门面 crate，re-export
├── ergatai-error/      共享错误类型
├── ergatai-binary/     二进制资源 (rmux, nats-server)
└── ergatai-cli/        CLI 工具
```

---

## 命令

```bash
# 构建
cargo build --workspace
cargo build -p ergatai-api

# 测试
cargo test --workspace
cargo test -p ergatai-api
cargo test -p ergatai-runtime

# Lint
cargo clippy --workspace -- -D warnings
cargo fmt --all

# 启动服务器
RUST_LOG=info cargo run -p ergatai-api -- --port 3000

# 启动 + debug 日志
RUST_LOG=debug cargo run -p ergatai-api -- --port 3000
```

---

## 关键类型

| 类型 | 文件 | 用途 |
|------|------|------|
| `AgentRuntime` | `runtime.rs` | 门面：registry + backend 封装 |
| `AgentInfo` | `types.rs` | registry 中的 agent 记录 (agent_id, handle, state, mcp_agent_id) |
| `AgentHandle` | `types.rs` | agent 的 handle (workspace, process_id, metadata) |
| `AgentMessagePayload` | `nats/events.rs` | NATS 消息体 (from, to, content, timestamp) |
| `RmuxBackend` | `backends/rmux.rs` | rmux daemon 交互 (发现 + 注入) |
| `NatsConnection` | `nats/connection.rs` | NATS 连接 + JetStream 上下文 |
| `ErgataiMcpServer` | `mcp/server.rs` | MCP 服务器实现 (工具注册 + 协议处理) |
| `CollaborationSession` | `collab/collaboration.rs` | 一次 DAG 编排对应的协作会话 (participants + policy) |
| `MeshPolicy` | `collab/collaboration.rs` | DAG 内 agent 之间的通信策略枚举 |

---

## 协作范式

DAG 编排 + 通信两层抽象。`CollaborationSession`（`ergatai-collab/src/collaboration.rs`）
绑定 `dag_id` + `participants` + `MeshPolicy`，定义一次编排中 agent 之间的通信规则：

| MeshPolicy | 含义 |
|---|---|
| `Open`（默认） | 任意参与者可互相 @mention |
| `Adjacent` | 仅 DAG 中有依赖边的 agent 对可通信 |
| `Star { hub }` | 所有通信经过指定 hub agent |
| `Restricted { pairs }` | 显式允许的 pair 列表 |

YAML 顶层 `communication` 字段声明模式（`open` / `adjacent` / `star:{hub_agent}`）。
`DagScheduler::with_context()` 在构造时从 `TaskGraph.communication` 解析出 policy，
并构造一个 `CollaborationSession` 存在调度器里。

当前 MVP 仅记录 session 元数据，不强制 ACL；后续可在 `send_message` 中调用
`CollaborationSession::allows(from, to, &graph)` 做强制校验。

**ACL 强制校验已启用**：`send_message` handler 在投递前会扫描所有 active
`DagScheduler`，对每个 scheduler 调用 `check_communication(from, to)`。
当发送方和接收方都是某 session 的 participant 时，按 `MeshPolicy` 校验；
任一方不是 participant 则放行（向后兼容）。`CommunicationCheck` 三值枚举：
`NotApplicable` / `Allowed` / `Denied(reason)`。

**自动清理**：DAG 完成（`on_node_completed` 检测到 `is_complete`）或彻底失败
（`on_node_failed` 级联到全部 terminal）时，`DagScheduler` 会从全局 registry
移除，`CollaborationSession` 随之失效，agent 间通信恢复无约束。

新增 MCP tool: `get_collaboration_status { dag_id? }` 查询当前 session。

---

## 防御性编排（Defensive Orchestration）

DAG 调度器新增多层防御机制，防止资源失控和 agent 僵死：

### DAG 预算与超时

| 字段 | 类型 | 含义 |
|------|------|------|
| `max_agent_calls` | `Option<u64>` | DAG 全局 agent 调用次数上限（所有节点共享） |
| `stall_timeout_secs` | `Option<u64>` | 节点无进度超时（触发 stall watcher） |
| `node_timeout_secs` | `Option<u64>` | 节点硬性超时（三阶段：warn → escalate → fail） |

- **预算检查**：`DagScheduler::check_budget()` 在 `generate_and_submit()` 中调用（节点提交时直接调用；完成/失败路径通过重试提交间接调用），超限则标记 DAG 失败。
- **僵死检测**：`spawn_stall_watcher()` 每 1 秒（`POLL_INTERVAL_SECS`）检查 `last_progress_age_secs()`，超过 `stall_timeout_secs` 则标记节点失败。
- **三阶段超时**：`spawn_timeout_watcher()` 在 50%/80%/100% 时分别触发 `publish_node_warned`、`publish_node_escalated`、标记失败。超时错误记录到 `node.metadata["timeout_error"]`。

### 中间件控制

| 机制 | 位置 | 阈值 |
|------|------|------|
| Agent 速率限制 | `ergatai-api/src/mcp/rate_limiter.rs` | 60 msg/min/agent（滑动窗口） |
| NATS 背压 | `ergatai-nats/src/event_bus.rs` | 1000 pending messages（`ERGATAI_BACKPRESSURE_THRESHOLD`） |
| Agent 健康检查 | `ergatai-runtime/src/backends/proc_linux.rs` | 读取 `/proc/{pid}/stat` 检测 Zombie/Dead |
| 僵死 agent 清理 | `ergatai-runtime/src/runtime.rs` | 连续 2 次 Zombie/Dead 观察后从 registry 移除 |

- **速率限制**：`AgentRateLimiter` 全局单例（`OnceLock`），在 `send_message()` 中检查，超限返回 `ErrorData::invalid_params(...)`。
- **背压检查**：`EventBus::check_backpressure()` 在 `publish_agent_message_reliable()` 顶部调用，缓存 5 秒避免频繁 NATS 查询。
- **健康检查**：`RmuxBackend::health_check_agents()` 遍历所有 pane，提取 PID，读取 `/proc/{pid}/stat` 判断进程状态。
- **自动清理**：`AgentRuntime::prune_unhealthy_agents()` 在 30 秒发现循环中调用，连续 2 次 Zombie/Dead 则从 registry 移除。

### 通信策略（已有，未改动）

`MeshPolicy` 枚举定义 DAG 内 agent 通信规则：`Open`（默认，任意 participant 可通信）、`Adjacent`（仅依赖边）、`Star { hub }`（中心辐射）、`Restricted { pairs }`（显式白名单）。`send_message` 在投递前调用 `CollaborationSession::allows(from, to)` 校验。

---

## NATS Subject 命名

```
ergatai.
├── agent.message.{agent_id}     agent 间消息 (JetStream, 持久化)
├── task.submit.{agent}          DagScheduler → TaskScheduler
├── task.complete.{task_id}      任务完成通知
├── dag.node_complete.{node}     DAG 节点完成
├── dag.complete.{dag_id}        DAG 全部完成
├── file.access.request          文件锁请求 (JetStream)
├── file.ready.{md5}             文件写入完成
└── file.error.{md5}             文件写入失败
```

JetStream Streams:
- `AGENT_MESSAGES` — agent 消息投递 (WorkQueue, 24h TTL)
- `FILE_ACCESS_REQUESTS/GRANTS/ESCALATIONS` — 文件访问控制
- `FILE_EVENTS` — 文件就绪/错误通知

---

## 数据库

- 主库: `{project_root}/.ergatai/ergatai.db` (SQLite)
- 锁库: `{project_root}/.ergatai/locks.db` (SQLite)

---

## 技术栈

| 层 | 技术 |
|----|------|
| 语言 | Rust (100%) |
| Agent ↔ Ergatai | MCP (JSON-RPC over Streamable HTTP) |
| 内部消息 | NATS (async-nats 0.38) + JetStream |
| 终端复用 | rmux (tmux 兼容) |
| 数据库 | SQLite (rusqlite 0.31) |
| HTTP | axum 0.7 |
| 异步 | tokio 1.36 |
| CLI | clap 4.5 |

# CLAUDE.md — Ergatai

多 agent 协作中间件。将独立运行的 AI 编码助手（如 OpenCode）组织成协作团队，通过 tmux pane 注入实现消息投递。提供 CLI（`ergatai`）、REST API、MCP 协议三种接入方式。

---

## 通信数据流（核心）

### 消息发送流程

```
Agent A (tmux pane)
  │  通过 MCP 协议调用 send_message tool
  ▼
┌──────────────────────────────────────────────────────────────────┐
│  ① MCP Server — send_message()                    [server.rs]   │
│     速率限制检查 (60 msg/min/agent, 滑动窗口)                     │
│     通信策略校验 (MeshPolicy, 仅当双方是 DAG participant 时)      │
│     构建 AgentMessagePayload，发布到 NATS JetStream              │
│     返回 { status: "queued" }                                    │
└──────────────────────────────────────────────────────────────────┘
  │
  ▼
┌──────────────────────────────────────────────────────────────────┐
│  ② NATS JetStream — AGENT_MESSAGES stream          [nats/]      │
│     subject: ergatai.agent.message.{target_agent_id}             │
│     背压检查 (≥1000 pending → 拒绝, 缓存 5s)                     │
│     持久化到文件，WorkQueue 保留策略，24h TTL                      │
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
│     在 registry 中查找 agent_id → 获取 AgentRecord               │
│     委托给 backend.inject_message()                               │
└──────────────────────────────────────────────────────────────────┘
  │
  ▼
┌──────────────────────────────────────────────────────────────────┐
│  ⑤ TmuxBackend — inject_message()                 [tmux.rs]     │
│     从 panes_map 查找 Pane handle                                │
│     sanitize_message() (去换行, 截断 64KiB)                       │
│     pane.send_text() → tmux daemon → 写入 pane 终端              │
└──────────────────────────────────────────────────────────────────┘
  │
  ▼
Agent B 的 tmux pane 中显示消息
```

### Agent 发现与注册

```
┌──────────────────────────────────────────────────────────────────┐
│  启动时 + 每 30 秒                                 [main.rs]     │
│     discover_and_register_agents()                                │
│     prune_unhealthy_agents() (连续 2 次 Zombie/Dead → 移除)       │
└──────────────────────────────────────────────────────────────────┘
  │
  ▼
┌──────────────────────────────────────────────────────────────────┐
│  TmuxBackend::discover_agents()                   [tmux.rs]     │
│     tmux.list_sessions() → 获取所有 session                       │
│     tmux.find_panes().all() → 获取所有 pane                       │
│     过滤: 跳过非 Running 状态的 pane                              │
│     过滤: 跳过 session 名以 _ 开头的 (warmup session)             │
│     对每个 pane:                                                  │
│       从 PaneProcessState::Running { pid } 提取子进程 PID          │
│       读 /proc/{pid}/environ 获取 TMUX_PANE 环境变量               │
│       用 TMUX_PANE (如 "%15") 作为 agent_id (确定性绑定)           │
│       fallback: 读不到则用 "pane_N"                               │
│     注册到 AgentRuntime registry                                  │
└──────────────────────────────────────────────────────────────────┘
```

### Agent ID 体系

| ID 来源 | 格式 | 说明 |
|---------|------|------|
| TMUX_PANE 环境变量 | `%15`, `%16` | **确定性 ID**，tmux 自动注入到 pane 进程，从 `/proc/{pid}/environ` 读取 |
| fallback | `pane_0`, `pane_1` | 读不到 TMUX_PANE 时按发现顺序编号（不稳定） |
| MCP client | `opencode@a1b2c3d4` | MCP 连接时自动生成，仅用于 MCP peer registry |
| 命名注册 | 自定义字符串 | 通过 `register_agent_name` MCP 工具绑定可读名称 |

**关键**: agent 之间发消息使用 TMUX_PANE 值（如 `%15`）或命名注册作为 target_agent_id。

---

## 项目结构

```
crates/
├── ergatai-api/           HTTP/MCP 服务器入口
│   ├── src/main.rs              启动流程、路由注册、CLI 参数
│   ├── src/api/                 REST API handlers
│   │   ├── agents.rs              agent 管理 (list/spawn/kill/message)
│   │   ├── workspaces.rs          workspace 管理 (list/create/delete)
│   │   └── status.rs              系统状态聚合
│   └── src/mcp/                 MCP 协议层
│       ├── server.rs              MCP 工具实现 (全部 tool handler)
│       ├── message_delivery.rs    NATS consumer → AgentRuntime 投递
│       ├── rate_limiter.rs        每 agent 滑动窗口速率限制
│       ├── conversation.rs        AutoGen 风格对话管理 (一问一答循环防护)
│       └── batch_aggregator.rs    群发消息聚合器 (1min 窗口合并回复)
├── ergatai-runtime/       Agent 运行时 (发现、注入、生命周期)
│   └── src/
│       ├── runtime.rs             AgentRuntime 门面
│       ├── agent_record.rs        AgentRecord (统一的 agent 状态记录)
│       ├── agent_lifecycle.rs     生命周期状态机
│       ├── backend.rs             Backend trait 定义
│       └── backends/
│           ├── tmux.rs              tmux backend (发现 + 注入, 首选)
│           ├── local_pty.rs         tmux/pty backend (legacy)
│           ├── direct_process.rs    直接进程 backend
│           └── proc_linux.rs        /proc/{pid}/stat 健康检查
├── ergatai-nats/          嵌入式 NATS 服务器 + JetStream 事件总线
│   └── src/
│       ├── server.rs              嵌入式 nats-server 启动
│       ├── connection.rs          NATS 连接 + JetStream 上下文
│       ├── event_bus.rs           事件发布/订阅门面
│       ├── agent_message_stream.rs  AGENT_MESSAGES stream 定义
│       ├── dag_event_stream.rs      DAG_EVENTS stream 定义
│       ├── file_access_streams.rs   文件访问控制 streams 定义
│       ├── task_queue.rs          通用 task queue 抽象
│       └── manager.rs             stream 生命周期管理
├── ergatai-collab/        多 agent 协作 (DAG 调度、任务协调)
│   └── src/
│       ├── dag_scheduler.rs       DAG 调度器 (预算、超时、通信策略)
│       ├── task_scheduler.rs      节点级任务调度
│       ├── task_coordinator.rs    跨节点协调
│       ├── collaboration.rs       CollaborationSession + MeshPolicy
│       ├── agent_launcher.rs      Agent 启动 + worktree/file-token 管理
│       ├── message_router.rs      @agent 提及检测 + NATS 路由
│       ├── plan_watcher.rs        任务计划/结果文件监控
│       └── timeout_tier.rs        三阶段超时 (Warn/Escalate/Fail)
├── ergatai-dag/           DAG 解析和模板引擎
│   └── src/
│       ├── yaml_parser.rs         YAML 解析 + 9 条严格校验规则
│       ├── dag_topology.rs        TaskNode / TaskGraph / TaskComplexity
│       ├── tree_topology.rs       TaskTree (树形拓扑)
│       ├── template.rs            {{var}} 模板展开
│       ├── condition.rs           条件表达式求值
│       ├── context.rs             DAG 上下文 (全局变量)
│       └── critical_path.rs       关键路径分析
├── ergatai-lock/          文件访问控制 (零信任, token-based)
│   └── src/
│       ├── lock_manager.rs        锁管理核心 (SQLite WAL)
│       ├── token.rs               SystemToken + FileToken 双层 token
│       ├── enforcer/              内核级强制 (Linux fanotify)
│       ├── snapshot.rs            Git-based COW 快照 (TOCTOU 防护)
│       ├── watchdog.rs            Token 过期 + 心跳监控
│       ├── watcher.rs             文件系统未授权修改检测
│       ├── lock_waiter.rs         NATS-based 阻塞等待队列
│       ├── renewal.rs             锁续期
│       ├── conflict_arbitration.rs 冲突仲裁
│       └── sensitive_paths.rs     敏感路径保护
├── ergatai-core/          门面 crate，re-export + 跨子系统集成
│   └── src/
│       ├── lib.rs                 统一 re-export
│       ├── unified_registry.rs    统一 agent 注册表 (合并三个子系统)
│       ├── agent_registry.rs      MCP 连接 agent 注册
│       └── signal.rs              优雅停机 (SIGINT/SIGTERM)
├── ergatai-error/         共享错误类型
│   └── src/
│       ├── types.rs               ErgataiError 枚举
│       ├── classify.rs            错误分类 (可恢复/不可恢复)
│       └── lib.rs
├── ergatai-agent/         Agent 配置和发现 (占位)
├── ergatai-binary/        二进制资源 (tmux, nats-server 查找/下载)
└── ergatai-cli/           CLI 工具 (ergatai 命令)
    └── src/
        ├── main.rs              clap CLI (start/workspace/agent/status)
        ├── commands/              子命令实现
        ├── client/                HTTP + WebSocket 客户端
        └── output/                输出格式化
examples/
└── simple-agent/          示例 MCP agent
```

---

## 命令

```bash
# 构建
cargo build --workspace
cargo build -p ergatai-api
cargo build -p ergatai-cli

# 测试
cargo test --workspace
cargo test -p ergatai-api
cargo test -p ergatai-runtime
cargo test -p ergatai-dag

# Lint
cargo clippy --workspace -- -D warnings
cargo fmt --all

# 启动 API 服务器
cargo run -p ergatai-api -- --port 3000
cargo run -p ergatai-api -- --port 3000 --host 0.0.0.0 --verbose
cargo run -p ergatai-api -- --api-token mysecret --tls-cert cert.pem --tls-key key.pem

# CLI 工具
ergatai start <name> [--work-dir /path]    # 快速启动：创建 workspace + 启动 agent + attach
ergatai workspace list|create|delete       # workspace 管理
ergatai agent list|spawn|kill|message      # agent 管理
ergatai status [--watch]                   # 系统状态 (可选 WebSocket 实时刷新)

# 环境变量
ERGATAI_API_URL=http://localhost:3000      # CLI 连接的 API 地址
ERGATAI_API_TOKEN=xxx                      # API 认证 token
ERGATAI_RUNTIME_BACKEND=tmux               # agent 运行时后端 (默认 tmux)
ERGATAI_TMUX_SESSION=ergatai              # session 名前缀
ERGATAI_SSE_KEEP_ALIVE=15                 # SSE keep-alive 间隔 (秒)
ERGATAI_TMUX_BINARY=/path/to/tmux         # tmux 二进制路径 (覆盖查找)
ERGATAI_BACKPRESSURE_THRESHOLD=1000       # NATS 背压阈值
```

### API 服务器参数

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `--port` | `3000` | 监听端口 |
| `--host` | `127.0.0.1` | 绑定地址 |
| `--verbose` / `-v` | `false` | 开启 debug 日志 |
| `--api-token` | (无) | API 认证 token（也可通过 `ERGATAI_API_TOKEN` 设置） |
| `--tls-cert` / `--tls-key` | (无) | TLS 证书/私钥 (PEM) |
| `--sse-keep-alive` | `15` | SSE keep-alive 秒数 |
| `--runtime-backend` | `tmux` | agent 运行时后端 |
| `--session-prefix` | `ergatai` | session 名前缀 |

---

## HTTP REST API

```
# 健康检查 (无认证)
GET  /health                         存活检查
GET  /ready                          就绪检查
GET  /metrics                        Prometheus 指标

# Workspace 管理
GET  /api/v1/workspaces              列出所有 workspace
POST /api/v1/workspaces              创建 workspace
DELETE /api/v1/workspaces/:id        删除 workspace

# Agent 管理
GET  /api/v1/agents                  列出所有 agent
POST /api/v1/agents                  启动新 agent
DELETE /api/v1/agents/:id            停止 agent
POST /api/v1/agents/:id/message      向 agent 发消息 (HTTP 直接注入, 绕过 NATS)

# 系统状态
GET  /api/v1/status                  聚合状态 (agent + workspace + DAG)

# DAG
POST /api/v1/dag                     提交 DAG
GET  /api/v1/dag/status              查询 DAG 状态
GET  /api/v1/dags                    列出所有 DAG

# MCP (Streamable HTTP, 每个 agent 独立路径)
POST /mcp/agent-1/...                MCP JSON-RPC (agent-1)
POST /mcp/agent-2/...                MCP JSON-RPC (agent-2)
POST /mcp/agent-3/...                MCP JSON-RPC (agent-3)
POST /mcp/...                        MCP JSON-RPC (default)
```

认证: `Authorization: Bearer <token>`（当 `--api-token` 设置时）。
HTTP 层限速: tower-governor（per-session / per-IP 分桶）。

---

## MCP 工具

Agent 通过 MCP 协议 (JSON-RPC over Streamable HTTP, protocol 2025-06-18) 调用以下工具：

| 工具 | 说明 |
|------|------|
| `list_agents` | 列出已注册 agent（支持条件过滤） |
| `register_agent_name` | 为当前 agent 绑定可读名称 |
| `send_message` | 向目标 agent 发消息（速率限制 + 通信策略校验） |
| `submit_orchestration` | 提交 DAG 工作流（YAML） |
| `validate_dag_yaml` | 干跑校验 DAG YAML（不执行，返回摘要或第一个错误） |
| `check_dag_status` | 查询 DAG 执行状态 |
| `get_collaboration_status` | 查询当前协作会话 + MeshPolicy |
| `request_file_access` | 请求文件访问 token |
| `release_file_access` | 释放文件访问 token |
| `list_active_locks` | 列出当前所有文件锁 |

---

## 关键类型

| 类型 | 文件 | 用途 |
|------|------|------|
| `AgentRuntime` | `runtime/runtime.rs` | 门面：registry + backend 封装 |
| `AgentRecord` | `runtime/agent_record.rs` | 统一的 agent 状态记录 (替代旧 AgentInfo) |
| `AgentLifecycleState` | `runtime/agent_lifecycle.rs` | agent 生命周期状态机 |
| `Backend` trait | `runtime/backend.rs` | 运行时后端抽象 (tmux/pty/process) |
| `TmuxBackend` | `runtime/backends/tmux.rs` | tmux 实现 (发现 + 注入 + 健康检查) |
| `AgentMessagePayload` | `nats/events.rs` | NATS 消息体 (from, to, content, timestamp) |
| `NatsConnection` | `nats/connection.rs` | NATS 连接 + JetStream 上下文 |
| `EventBus` | `nats/event_bus.rs` | 事件发布/订阅门面 (含背压检查) |
| `ErgataiMcpServer` | `api/mcp/server.rs` | MCP 服务器 (工具注册 + 协议处理) |
| `AgentRateLimiter` | `api/mcp/rate_limiter.rs` | 滑动窗口速率限制 (全局 OnceLock) |
| `ConversationManager` | `api/mcp/conversation.rs` | AutoGen 风格对话管理 (循环防护) |
| `BatchAggregator` | `api/mcp/batch_aggregator.rs` | 群发消息聚合器 |
| `TaskGraph` | `dag/dag_topology.rs` | DAG 图结构 (nodes + 预算 + 通信) |
| `TaskNode` | `dag/dag_topology.rs` | DAG 节点 (agent, task, depends_on, priority, complexity...) |
| `TaskComplexity` | `dag/dag_topology.rs` | 任务复杂度枚举 (Low/Medium/High) |
| `DagScheduler` | `collab/dag_scheduler.rs` | DAG 调度器 (预算、超时、通信策略) |
| `CollaborationSession` | `collab/collaboration.rs` | 一次 DAG 编排对应的协作会话 |
| `MeshPolicy` | `collab/collaboration.rs` | DAG 内 agent 通信策略枚举 |
| `TimeoutTier` | `collab/timeout_tier.rs` | 三阶段超时 (Warn 50% / Escalate 80% / Fail 100%) |
| `UnifiedAgentRegistry` | `core/unified_registry.rs` | 统一 agent 注册表 (合并三个子系统) |
| `LockManager` | `lock/lock_manager.rs` | 文件锁管理 (SQLite WAL) |
| `SystemToken` / `FileToken` | `lock/token.rs` | 双层 token (准入 + 操作权限) |

---

## YAML 严格校验规则

`yaml_parser.rs` 在解析 DAG YAML 时执行以下校验，任一不通过则返回错误：

| # | 规则 | 说明 |
|---|------|------|
| 1 | 顶层未知字段 | `deny_unknown_fields` — 拼写错误立即报错（如 `communcation:` ） |
| 2 | 任务名非空 | `name` 必填且非空白 |
| 3 | 任务名唯一 | 不允许重名 |
| 4 | `priority` 枚举 | DAG 和 task 级: `low` \| `medium` \| `high`（大小写不敏感） |
| 5 | 正值约束 | `timeout` / `max_agent_calls` / `stall_timeout_secs` / `node_timeout_secs` 必须 > 0（0 不是"无限制"） |
| 6 | `communication` 格式 | `open` \| `adjacent` \| `star:{hub}`；hub 必须是某个 task 的 agent |
| 7 | 模板变量引用 | `{{var}}` 必须引用已声明的 `parameters` 条目（未声明 parameters 时跳过检查） |
| 8 | 依赖存在性 | `depends_on` 引用的任务名必须存在 |
| 9 | `scope` glob 合法 | 非法 glob 模式报错（不是静默丢弃），且必须是相对路径 |

任务级未知字段收集为 metadata（允许），不报错。

使用 `validate_dag_yaml` MCP 工具可干跑校验而不触发调度。

---

## 任务复杂度 (TaskComplexity)

YAML 中通过 `complexity: low|medium|high` 标注（默认 Medium）：

| 级别 | 含义 | 典型耗时 |
|------|------|----------|
| `Low` | 格式修复、文档更新、简单配置、技术债 | < 30 分钟 |
| `Medium` | 正常功能开发、bug 修复、小型重构 | 30 分钟 - 2 小时 |
| `High` | 架构改动、跨模块重构、大规模迁移 | > 2 小时 |

调度器用 `as_score()` 将复杂度转换为数值参与优先级计算。
`node_timeout_secs` 按复杂度缩放：Low × 0.5, Medium × 1.0, High × 2.0。

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

**ACL 强制校验已启用**：`send_message` handler 在投递前会扫描所有 active
`DagScheduler`，对每个 scheduler 调用 `check_communication(from, to)`。
当发送方和接收方都是某 session 的 participant 时，按 `MeshPolicy` 校验；
任一方不是 participant 则放行（向后兼容）。`CommunicationCheck` 三值枚举：
`NotApplicable` / `Allowed` / `Denied(reason)`。

**自动清理**：DAG 完成（`on_node_completed` 检测到 `is_complete`）或彻底失败
（`on_node_failed` 级联到全部 terminal）时，`DagScheduler` 会从全局 registry
移除，`CollaborationSession` 随之失效，agent 间通信恢复无约束。

---

## 防御性编排（Defensive Orchestration）

DAG 调度器多层防御机制，防止资源失控和 agent 僵死：

### DAG 预算与超时

| 字段 | 类型 | 含义 |
|------|------|------|
| `max_agent_calls` | `Option<u64>` | DAG 全局 agent 调用次数上限（所有节点共享） |
| `stall_timeout_secs` | `Option<u64>` | 节点无进度超时（触发 stall watcher） |
| `node_timeout_secs` | `Option<u64>` | 节点硬性超时（三阶段：warn → escalate → fail） |

- **预算检查**：`DagScheduler::check_budget()` 在 `generate_and_submit()` 中调用，超限则标记 DAG 失败。
- **僵死检测**：`spawn_stall_watcher()` 每 1 秒检查 `last_progress_age_secs()`，超过 `stall_timeout_secs` 则标记节点失败。
- **三阶段超时**：`spawn_timeout_watcher()` 在 50%/80%/100% 时分别触发 `publish_node_warned`、`publish_node_escalated`、标记失败。超时错误记录到 `node.metadata["timeout_error"]`。

### 中间件控制

| 机制 | 位置 | 阈值 |
|------|------|------|
| Agent 速率限制 | `ergatai-api/src/mcp/rate_limiter.rs` | 60 msg/min/agent（滑动窗口, TOCTOU-safe） |
| NATS 背压 | `ergatai-nats/src/event_bus.rs` | 1000 pending messages（`ERGATAI_BACKPRESSURE_THRESHOLD`） |
| Agent 健康检查 | `ergatai-runtime/src/backends/proc_linux.rs` | 读取 `/proc/{pid}/stat` 检测 Zombie/Dead |
| 僵死 agent 清理 | `ergatai-runtime/src/runtime.rs` | 连续 2 次 Zombie/Dead 观察后从 registry 移除 |
| HTTP 限速 | `tower-governor` | per-session (MCP) / per-IP (REST) 分桶 |

### 对话防护

- **ConversationManager**：AutoGen 风格一问一答循环防护，`max_turns` 到达后自动重置。
- **BatchAggregator**：1 分钟内 A 发给 ≥2 个 agent → 群发模式，收集回复合并推送。

### 启动恢复

- **DAG 崩溃恢复**：`DagScheduler::load_all_from_disk()` 在启动时扫描磁盘，恢复崩溃前正在运行的 DAG —— 将 `Running` 节点回滚到 `Pending`，重新提交调度。
- **周期性 re-discovery**：每 30 秒扫描 agent，清理僵死 agent，同步清理速率限制器的过期窗口。
- **Peer reaper**：每 10 秒清理已断开的 MCP transport。
- **Conversation reaper**：每 5 分钟清理 `ConversationManager` 中超过 1 小时的陈旧条目。

---

## NATS Subject 命名

```
ergatai.
├── agent.message.{agent_id}        agent 间消息 (JetStream, 持久化)
├── task.submit.{agent}             DagScheduler → TaskScheduler
├── task.complete.{task_id}         任务完成通知
├── dag.node_complete.{node}        DAG 节点完成
├── dag.node_failed.{node}          DAG 节点失败
├── dag.node_warned.{node}          超时 warn (50%)
├── dag.node_escalated.{node}       超时 escalate (80%)
├── dag.complete.{dag_id}           DAG 全部完成
├── file.access.request             文件锁请求 (JetStream)
├── file.ready.{md5}                文件写入完成
└── file.error.{md5}                文件写入失败
```

JetStream Streams:

| Stream | 用途 | 保留策略 | TTL |
|--------|------|----------|-----|
| `AGENT_MESSAGES` | agent 消息投递 | WorkQueue | 24h |
| `DAG_EVENTS` | DAG 生命周期事件 | WorkQueue | 24h |
| `FILE_ACCESS_REQUESTS` | 文件锁请求 | WorkQueue | 1h |
| `FILE_ACCESS_GRANTS` | 文件锁授权 | WorkQueue | 1h |
| `FILE_ACCESS_ESCALATIONS` | 文件锁升级 | WorkQueue | 30min |
| `FILE_EVENTS` | 文件就绪/错误通知 | WorkQueue | 1h |
| `LOCK_WAITERS` | 阻塞式锁获取等待队列 (`ergatai.lock.request.*`, `ergatai.lock.release.*`) | WorkQueue | 2h |

---

## 文件访问控制 (ergatai-lock)

零信任文件访问控制，面向多 agent 协作：

- **双层 Token**: `SystemToken`（准入）+ `FileToken`（操作权限）
- **SQLite WAL**: 高并发锁管理
- **Git COW 快照**: Copy-on-Write 防止 TOCTOU
- **Watchdog**: Token 过期 + 心跳监控
- **File Watcher**: 检测未授权修改
- **NATS 等待队列**: 阻塞式锁获取
- **内核级强制**: Linux fanotify 拦截 `open()`，从 advisory 升级到 mandatory（非 Linux 或权限不足时 fail-open）

---

## 数据库

- 主库: `{project_root}/.ergatai/ergatai.db` (SQLite)
- 锁库: `{project_root}/.ergatai/locks.db` (SQLite, WAL mode)

---

## 技术栈

| 层 | 技术 |
|----|------|
| 语言 | Rust (100%, edition 2021) |
| Agent ↔ Ergatai | MCP (JSON-RPC over Streamable HTTP, protocol 2025-06-18) |
| REST API | axum 0.7 (+ tower-governor 限速) |
| 内部消息 | NATS (async-nats **0.50**) + JetStream |
| 终端复用 | tmux (tmux 兼容) |
| 数据库 | SQLite (rusqlite 0.31, bundled) |
| 异步 | tokio 1.36 |
| CLI | clap 4.5 |
| TUI | ratatui 0.30 + tui-widgets 0.7 |
| HTTP 客户端 | reqwest 0.12 (rustls-tls) |
| 序列化 | serde 1.0 + serde_json 1.0 + serde_yaml 0.9 |
| 日志 | tracing 0.1 + tracing-subscriber 0.3 |
| 错误 | thiserror 1.0 + anyhow 1.0 |
| 指标 | metrics-exporter-prometheus |
| 内核拦截 | libc 0.2 (fanotify) |

---

## 优雅停机

`ergatai-core/src/signal.rs` 捕获 SIGINT (Ctrl+C) 和 SIGTERM：

- 第一次信号：触发优雅停机（关闭文件访问控制、NATS 等子系统）
- 第二次信号：强制 `process::exit(1)`（防止停机挂起）
- 停机预算：全局超时，各子系统有独立短超时

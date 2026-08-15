# Ergatai 架构

## 项目概述

Ergatai 是一个多 Agent 协作平台，将独立的 AI 编程助手组织成协调的工程团队，支持并行任务执行、安全的并发文件访问和基于 DAG 的工作流编排。

## 架构设计

### 纯 Rust 实现

Ergatai 采用纯 Rust 实现，分为三个核心 crate：

```
crates/
├── ergatai-core    # 核心库：所有业务逻辑
├── ergatai-cli     # CLI 二进制：交互式对话界面
└── ergatai-api     # API 服务器：HTTP/WebSocket 接口（未来 GUI 使用）
```

### 核心模块 (ergatai-core)

```
ergatai-core/src/
├── acp/                    # ACP 协议层
│   ├── manager.rs          # 会话管理器
│   ├── sdk_session.rs      # ACP SDK 会话
│   ├── sdk_pool.rs         # 会话池
│   └── ...
├── nats/                   # NATS 消息系统
│   ├── manager.rs          # NATS 管理器
│   ├── streams.rs          # JetStream 流
│   └── ...
├── orchestration/          # DAG 编排
│   ├── task_graph.rs       # 任务图
│   ├── template.rs         # 模板引擎
│   └── dag_context.rs      # DAG 上下文
├── cross_agent/            # 多 Agent 协作
│   ├── dag_scheduler.rs    # DAG 调度器
│   ├── task_scheduler.rs   # 任务调度器
│   ├── agent_launcher.rs   # Agent 启动器
│   └── message_router.rs   # 消息路由
├── file_access/            # 文件访问控制
│   ├── lock_manager.rs     # 锁管理器
│   ├── token.rs            # 令牌系统
│   └── watchdog.rs         # 看门狗
└── agent/                  # Agent 管理
    ├── config.rs           # Agent 配置
    ├── discovery.rs        # Agent 发现
    └── hosted_config.rs    # 托管 Agent 配置
```

## 通信架构

### 两层独立的通信

| 层级 | 协议 | 方向 | 用途 |
|------|------|------|------|
| **Agent ↔ Ergatai** | ACP (JSON-RPC over stdio) | 双向 | 提示、响应、工具调用、权限 |
| **Ergatai 内部** | NATS (JetStream) | 事件流 | 任务路由、完成事件、文件通知 |

### Agent 通信流程

```
用户请求: "用 3 个 Agent 重构这个模块"
    ↓
CLI 生成 DAG 定义
    ↓
DagScheduler 解析 → NATS 分发任务 → Sub-agents A/B/C (ACP 执行)
                   ↑ NATS 事件转发完成状态
```

## 文件访问控制

基于令牌的两级锁定系统：

```
Agent A (WRITE 锁: src/foo.rs)
    ↓ 持有令牌
    ↓ 修改文件
    ↓ 创建 git 快照
    ↓ 释放锁
Agent B (等待 WRITE 锁 → 获取 → 继续)
```

**两级令牌系统：**
- `SystemToken` - 会话级准入（绑定 agent_id + session_id）
- `FileToken` - 操作级（READ/WRITE/ADMIN 范围）

**数据库**: `{project_root}/.ergatai/locks.db` (SQLite)

## DAG 编排

```markdown
## Task A (分析代码)
- **agent**: claude-code
- **task**: tasks/analyze.md

## Task B (编写测试)
- **agent**: cursor
- **task**: tasks/test.md
- **depends_on**: [Task A]
- **input**: "分析结果: {{TaskA.review_result}}"
- **output**: test_result, coverage
- **retry**: 3
- **timeout**: 300
```

**模板变量：**
- `{{global.*}}` - 全局变量 (DagContext.global_vars)
- `{{node_id.*}}` - 上游节点输出 (DagContext.node_outputs)

## NATS Subject 命名

```
ergatai.
├── task.submit.{agent}              # DagScheduler → TaskScheduler
├── task.complete.{task_id}          # Agent 完成通知
├── dag.node_complete.{node}         # AgentLauncher → DagScheduler
├── dag.complete.{dag_id}            # 所有任务完成
├── agent.message.{agent_id}         # Agent 间消息 (@mention)
├── file.access.request              # 文件锁请求 (JetStream)
├── file.ready.{md5_hash}            # 文件 WRITE 完成通知
└── file.error.{md5_hash}            # 文件 WRITE 失败通知
```

**JetStream 流：**
- `TASK_QUEUE` - 任务分发 (WorkQueue 保留)
- `FILE_ACCESS_REQUESTS/GRANTS/ESCALATIONS` - 文件访问控制
- `FILE_EVENTS` - 文件就绪/错误通知
- `LOCK_WAITERS` - 锁等待队列

## CLI 使用

### 基本对话

```bash
# 交互式对话
ergatai chat

# 指定 Agent
ergatai chat --agent claude-code

# 单次对话
ergatai chat "帮我重构这个模块"
```

### DAG 多 Agent 模式

```bash
# 提交 DAG
ergatai dag submit workflow.md

# 查看 DAG 状态
ergatai dag status <dag-id>

# 列出所有 DAG
ergatai dag list
```

### Agent 管理

```bash
# 列出可用 Agent
ergatai agents list

# 查看 Agent 详情
ergatai agents info claude-code
```

## 数据库

### SQLite 数据库

**位置**: `{project_root}/.ergatai/ergatai.db`

**主要表：**
- `projects` - 项目信息
- `agents` - Agent 配置
- `sessions` - 会话记录
- `tasks` - 任务记录

## 开发

### 构建

```bash
# 构建所有 crate
cargo build --workspace

# 构建 release 版本
cargo build --release --workspace

# 运行测试
cargo test --workspace
```

### 运行

```bash
# 运行 CLI
cargo run --bin ergatai -- chat

# 运行 API 服务器
cargo run --bin ergatai-api -- --port 3000
```

### 代码质量

```bash
# 代码检查
cargo clippy --workspace -- -D warnings

# 格式化
cargo fmt --all
```

## 技术栈

| 组件 | 技术 |
|------|------|
| 语言 | Rust 100% |
| Agent 协议 | ACP (agent-client-protocol v2) |
| 消息系统 | NATS (async-nats 0.38) + JetStream |
| 数据库 | SQLite (rusqlite 0.31) |
| CLI 框架 | clap + ratatui + crossterm |
| HTTP 服务器 | axum 0.7 |
| 异步运行时 | tokio |

## 项目状态

### ✅ 已完成

**核心基础设施：**
- NATS 消息系统 + JetStream 流
- ACP 协议集成 + 会话池管理
- DAG 编排引擎 + 模板系统
- 文件访问控制 + 令牌锁定
- Agent 发现 + 配置管理

**架构迁移：**
- 从 Electron/React/TypeScript 迁移到纯 Rust
- 移除所有 NAPI 绑定
- 创建 workspace 结构 (core + cli + api)

### 🚧 进行中

**CLI 实现：**
- 交互式对话界面
- Agent 选择和配置
- 实时进度显示

**集成测试：**
- 端到端多 Agent 协作场景
- CLI → Backend → Agent 完整流程

### 📋 计划中

**功能增强：**
- DAG 可视化
- 会话持久化
- Agent 性能统计

**未来 GUI：**
- 基于 ergatai-api 的 Web 界面
- 桌面应用（可选）

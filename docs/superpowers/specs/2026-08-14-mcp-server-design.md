# Ergatai MCP Server 设计文档

**日期**: 2026-08-14  
**状态**: 设计中

## 概述

将 Ergatai 从 CLI/GUI 应用转型为**协作中间件**，核心是一个 MCP (Model Context Protocol) Server。AI agents（如 Claude Code）通过 MCP 协议调用 Ergatai 的 tools 实现多 agent 协作。

## 核心定位

**Ergatai = MCP Server**
- 无头服务进程（headless daemon）
- AI agents 通过 MCP 协议调用协作能力
- 保留 DAG 编排和 ACP agent 管理

## 架构设计

### 通信架构

```
User → Claude Code (Admin Agent)
    ↓ MCP (agent 说话的出口)
Ergatai MCP Server
    ↓ ACP (消息发送给 agent 的入口)
Task Agents (spawned by Ergatai)
```

**两层通信**:
- **MCP**: Agent → Ergatai（agent 调用 tools）
- **ACP**: Ergatai → Agent（Ergatai 启动和控制 task agents）

### Agent 发现机制

**连接即发现**:
1. Agent 连接到 Ergatai MCP server
2. 强制健康检查（MCP connection handshake）
3. 健康检查返回: agent_id, capabilities, status
4. Ergatai 自动发现并记录活跃 agents
5. 通过 `list_agents` tool 查询

### 权限模型

**Admin Agent**（用户的 agent，如 Claude Code）:
- 连接方式: MCP
- 权限: 自由调用所有 MCP tools
- 不需要权限审批（编排者角色）

**Task Agents**（Ergatai 启动的 agents）:
- 连接方式: ACP
- 权限: 文件操作需要审批
  - Read: 自动批准
  - Write: 获取文件锁 + 冲突仲裁
  - Admin: 单 agent 模式自动批准；多 agent 需人工审批

**单 Agent 模式**:
- 触发: 只有 admin agent 连接
- 效果: 所有操作自动批准
- 稳定窗口: 72 秒

## MCP Tools

### 1. `submit_orchestration`

**描述**: 提交 DAG 工作流，启动多 agent 协作任务

**输入**:
```typescript
{
  dag_definition: string,  // Markdown 格式的 DAG 定义
  context?: object         // 可选的上下文变量
}
```

**输出**:
```typescript
{
  dag_id: string,
  status: "submitted" | "running" | "completed" | "failed",
  message: string
}
```

**实现**:
- 解析 DAG markdown
- 创建 TaskGraph
- 通过 NATS 分发任务
- 启动 task agents (ACP)

### 2. `check_dag_status`

**描述**: 查询 DAG 执行状态、进度、结果

**输入**:
```typescript
{
  dag_id: string
}
```

**输出**:
```typescript
{
  dag_id: string,
  status: "pending" | "running" | "completed" | "failed",
  progress: {
    total_nodes: number,
    completed_nodes: number,
    failed_nodes: number
  },
  results: {
    [node_id: string]: {
      status: "completed" | "failed" | "running" | "pending",
      output?: any,
      error?: string
    }
  }
}
```

**实现**:
- 查询 DagScheduler 状态
- 聚合 task agents 的结果

### 3. `list_agents`

**描述**: 列出当前连接的 agents（通过健康检查发现的）

**输入**:
```typescript
{
  include_capabilities?: boolean  // 是否包含 capabilities 详情
}
```

**输出**:
```typescript
{
  agents: [
    {
      agent_id: string,
      status: "active" | "idle" | "disconnected",
      capabilities?: string[],
      connected_at: string,
      last_heartbeat: string
    }
  ]
}
```

**实现**:
- 查询 AgentRegistry（健康检查维护的活跃 agents 列表）

### 4. `send_message`

**描述**: 发送消息给其他 agents，支持 @mention

**输入**:
```typescript
{
  target_agent_id: string,
  message: string,
  mention_type?: "info" | "request" | "response"
}
```

**输出**:
```typescript
{
  message_id: string,
  status: "sent" | "queued" | "failed",
  delivered_at?: string
}
```

**实现**:
- 通过 NATS 发送消息到目标 agent
- 目标 agent 通过 MCP 接收消息

## 实现计划

### Phase 1: MCP Server 基础框架

**目标**: 将 ergatai-api 改造为 MCP server

**任务**:
1. 引入 MCP server 依赖（`mcp` crate 或实现 JSON-RPC）
2. 实现 MCP 协议处理（initialize, tools/list, tools/call）
3. 实现健康检查机制
4. 搭建 tool handler 框架

**文件**:
- `crates/ergatai-api/src/mcp/mod.rs` - MCP 协议处理
- `crates/ergatai-api/src/mcp/tools.rs` - Tool handlers
- `crates/ergatai-api/src/mcp/health_check.rs` - 健康检查

### Phase 2: 实现 MCP Tools

**目标**: 实现四个核心 tools

**任务**:
1. `submit_orchestration` - 集成 DAG 编排
2. `check_dag_status` - 查询 DAG 状态
3. `list_agents` - 查询活跃 agents
4. `send_message` - 发送 agent 间消息

**依赖**:
- `ergatai-dag` - DAG 编排
- `ergatai-collab` - 协作逻辑
- `ergatai-nats` - 消息传递

### Phase 3: Agent 发现与注册

**目标**: 实现基于健康检查的 agent 发现

**任务**:
1. AgentRegistry - 维护活跃 agents 列表
2. 健康检查流程（MCP connection handshake）
3. Agent 状态管理（active/idle/disconnected）
4. Heartbeat 机制

**文件**:
- `crates/ergatai-core/src/agent_registry.rs`

### Phase 4: 集成测试

**目标**: 端到端测试 MCP server

**任务**:
1. 测试 MCP connection + 健康检查
2. 测试 submit_orchestration 流程
3. 测试多 agent 协作场景
4. 测试权限模型

## 技术选型

### MCP 实现

**选项 1**: 使用现有 MCP crate
- `mcp` crate (如果存在)
- 优点: 快速实现
- 缺点: 可能不灵活

**选项 2**: 自己实现 MCP 协议
- JSON-RPC over stdio/HTTP
- 优点: 完全控制
- 缺点: 工作量大

**推荐**: 先调研现有 MCP crate，优先使用

### 传输协议

**选项 1**: stdio
- 优点: 简单，适合本地 agent
- 缺点: 不适合远程 agents

**选项 2**: HTTP/WebSocket
- 优点: 支持远程 agents
- 缺点: 更复杂

**推荐**: HTTP + WebSocket（支持本地和远程）

## 迁移策略

### 删除的内容

- ❌ `ergatai-cli` - 已删除
- ❌ TUI 相关代码（ratatui）
- ❌ GUI 相关计划

### 保留的内容

- ✅ `ergatai-core` - 业务逻辑
- ✅ `ergatai-acp` - ACP agent 管理
- ✅ `ergatai-dag` - DAG 编排
- ✅ `ergatai-lock` - 文件锁
- ✅ `ergatai-nats` - 内部消息
- ✅ `ergatai-collab` - 协作逻辑

### 改造的内容

- 🔄 `ergatai-api` - 从 REST/WebSocket 改为 MCP server

## 风险与挑战

1. **MCP 协议成熟度**: MCP 还在演进，需要跟进规范变化
2. **Agent 兼容性**: 不同 agent（Claude Code, Cursor 等）的 MCP 支持可能不同
3. **性能**: MCP + ACP 双层通信可能增加延迟
4. **调试**: 多 agent 协作的调试复杂度

## 成功标准

- ✅ MCP server 可以接受 agent 连接
- ✅ 健康检查机制工作正常
- ✅ 四个核心 tools 可用
- ✅ 多 agent 协作流程端到端测试通过
- ✅ 权限模型正常工作

## 下一步

1. 调研现有 MCP crate/实现
2. 实现 MCP server 基础框架
3. 实现第一个 tool（`list_agents`）
4. 端到端测试

# Ergatai 协作中间件设计文档

**日期**: 2026-08-14  
**状态**: 设计中

## 概述

将 Ergatai 从 CLI/GUI 应用转型为**多 agent 协作中间件**。Ergatai 作为消息 broker，在 agents 之间转发消息，实现 agent-to-agent 对话。

## 核心定位

**Ergatai = 消息中间件/Relay**

- 无头服务进程（headless daemon）
- 在 agents 之间转发消息
- 双向通信：MCP（agent→Ergatai）+ ACP（Ergatai→agent）

## 架构设计

### 通信架构

```
Agent A ←→ MCP ←→ Ergatai ←→ ACP ←→ Agent B
```

**两层通信**:
- **MCP** (Agent → Ergatai): Agent 是 client，Ergatai 是 server
  - Agents 通过 MCP 发送消息给 Ergatai
  - Ergatai 接收并路由消息
- **ACP** (Ergatai → Agent): Ergatai 是 client，Agent 是 server
  - Ergatai 通过 ACP 转发消息给目标 agents
  - Agents 作为 ACP server 接收消息

**关键要求**:
- 每个 agent 必须安装 Ergatai 的 MCP（作为 MCP client）
- 每个 agent 必须暴露 ACP server（接收 Ergatai 的消息）
- Ergatai 在中间 relay 消息

### 数据流示例

**场景：Agent A 想和 Agent B 对话**

```
1. Agent A 通过 MCP 发送消息给 Ergatai
   ↓ (MCP: agent 是 client, Ergatai 是 server)
2. Ergatai MCP Server 接收消息
   - 解析目标 agent (Agent B)
   - 查找 Agent B 的 ACP 连接
   ↓
3. Ergatai 通过 ACP 转发给 Agent B
   ↓ (ACP: Ergatai 是 client, agent 是 server)
4. Agent B 处理并响应
   ↓
5. Ergatai 通过 MCP 转发回 Agent A
```

### Agent 发现机制

**连接即发现**:
1. Agent 启动时，连接 Ergatai MCP server
2. MCP connection handshake 包含健康检查
3. 健康检查返回: agent_id, capabilities, status
4. Ergatai 记录该 agent 为"活跃"
5. Agent 同时暴露 ACP server，等待 Ergatai 的消息
6. 通过 `list_agents` tool 查询活跃 agents

### 权限模型

由于 Ergatai 是消息 relay，权限模型简化为：

**消息级权限**:
- Agent 可以发送消息给任何已注册的 agent
- 可以配置 ACL（访问控制列表）限制 agent 间的通信
- 敏感操作需要人工审批

**文件访问**（如果 agents 需要协作修改文件）:
- 保留现有的文件锁机制
- 通过 MCP tool 请求文件锁
- 多 agent 写同一文件时需要冲突仲裁

## MCP Tools

### 1. `send_message`

**描述**: 发送消息给其他 agents

**输入**:
```typescript
{
  target_agent_id: string,
  message: string,
  message_type?: "request" | "response" | "broadcast"
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
- 查找目标 agent 的 ACP 连接
- 通过 ACP 转发消息
- 等待响应并返回

### 2. `list_agents`

**描述**: 列出当前连接的 agents

**输入**:
```typescript
{
  include_capabilities?: boolean
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

### 3. `submit_orchestration`

**描述**: 提交 DAG 工作流，启动多 agent 协作任务

**输入**:
```typescript
{
  dag_definition: string,
  context?: object
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

### 4. `check_dag_status`

**描述**: 查询 DAG 执行状态

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
  results: object
}
```

## 实现计划

### Phase 1: MCP Server 基础框架

**目标**: 实现 MCP server，接受 agent 连接

**任务**:
1. 引入 MCP server 依赖
2. 实现 MCP 协议处理（initialize, tools/list, tools/call）
3. 实现健康检查机制
4. Agent registry（维护活跃 agents）

**文件**:
- `crates/ergatai-api/src/mcp/mod.rs` - MCP 协议处理
- `crates/ergatai-api/src/mcp/tools.rs` - Tool handlers
- `crates/ergatai-core/src/agent_registry.rs` - Agent 注册表

### Phase 2: 实现核心 MCP Tools

**目标**: 实现 `send_message` 和 `list_agents`

**任务**:
1. `send_message` - 通过 ACP 转发消息给目标 agent
2. `list_agents` - 查询活跃 agents
3. 集成 ACP client（Ergatai 连接 agent 的 ACP server）

**依赖**:
- `ergatai-acp` - ACP client 实现

### Phase 3: DAG 编排 Tools

**目标**: 实现 `submit_orchestration` 和 `check_dag_status`

**任务**:
1. 集成 DAG 编排引擎
2. 通过 NATS 分发任务
3. 启动 task agents (ACP)

**依赖**:
- `ergatai-dag` - DAG 编排
- `ergatai-nats` - 消息传递

### Phase 4: 集成测试

**目标**: 端到端测试消息 relay

**任务**:
1. 两个 agents 通过 Ergatai 对话
2. 测试消息路由和转发
3. 测试 agent 发现和注册
4. 测试 DAG 编排流程

## 技术选型

### MCP 实现

**选项 1**: 使用 `mcp` crate（如果存在）
- 优点: 快速实现
- 缺点: 可能不灵活

**选项 2**: 自己实现 MCP 协议
- JSON-RPC over stdio/HTTP
- 优点: 完全控制
- 缺点: 工作量大

**推荐**: 先调研现有 MCP crate，优先使用

### 传输协议

**MCP**: HTTP + WebSocket（支持本地和远程 agents）
**ACP**: stdio（agents 作为子进程）或 HTTP（远程 agents）

## 迁移策略

### 删除的内容

- ❌ `ergatai-cli` - 已删除
- ❌ TUI 相关代码（ratatui）
- ❌ GUI 相关计划

### 保留的内容

- ✅ `ergatai-core` - 业务逻辑
- ✅ `ergatai-acp` - ACP client（Ergatai 连接 agents）
- ✅ `ergatai-dag` - DAG 编排
- ✅ `ergatai-lock` - 文件锁
- ✅ `ergatai-nats` - 内部消息
- ✅ `ergatai-collab` - 协作逻辑

### 改造的内容

- 🔄 `ergatai-api` - 从 REST/WebSocket 改为 MCP server

## 风险与挑战

1. **MCP 协议成熟度**: MCP 还在演进，需要跟进规范变化
2. **Agent 兼容性**: 不同 agent 的 MCP/ACP 支持可能不同
3. **性能**: 双层通信（MCP + ACP）可能增加延迟
4. **调试**: 多 agent 消息 relay 的调试复杂度

## 成功标准

- ✅ MCP server 可以接受 agent 连接
- ✅ 健康检查机制工作正常
- ✅ Agent 可以通过 `send_message` 互相发消息
- ✅ `list_agents` 返回正确的活跃 agents 列表
- ✅ 端到端消息 relay 测试通过

## 下一步

1. 调研现有 MCP crate/实现
2. 实现 MCP server 基础框架
3. 实现 `list_agents` tool
4. 实现 `send_message` tool
5. 端到端测试

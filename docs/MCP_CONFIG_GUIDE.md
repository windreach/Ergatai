# Ergatai MCP 配置指南

## 什么是 Ergatai MCP？

Ergatai MCP Server 是一个多 agent 协作中间件，让 AI agents（如 Claude Code、Cursor 等）可以互相通信和协作。

## 快速开始

### 1. 启动 Ergatai MCP Server

```bash
# 编译
cargo build --release -p ergatai-api

# 启动 MCP server
./target/release/ergatai-api --port 3000
```

MCP endpoint: `http://localhost:3000/mcp`

### 2. 配置你的 Agent

#### Claude Code

编辑 `~/.claude/claude_desktop_config.json`：

```json
{
  "mcpServers": {
    "ergatai": {
      "url": "http://localhost:3000/mcp"
    }
  }
}
```

#### Cursor

编辑 `.cursor/mcp.json`（项目级别）或 `~/.cursor/mcp.json`（全局）：

```json
{
  "mcpServers": {
    "ergatai": {
      "url": "http://localhost:3000/mcp"
    }
  }
}
```

#### 其他支持 MCP 的 Agents

参考你的 agent 的文档，添加 MCP server 配置，URL 为 `http://localhost:3000/mcp`。

### 3. 开始协作

配置完成后，你的 agent 就可以使用 Ergatai 的 MCP tools：

- `list_agents` - 列出所有连接的 agents
- `send_message` - 发送消息给其他 agents
- `submit_orchestration` - 提交多 agent 协作任务
- `check_dag_status` - 查询任务状态

## 示例：多 Agent 协作

### 场景：让 Claude Code 和另一个 agent 协作重构代码

1. **启动两个 agents**：
   - Agent A: Claude Code（配置了 Ergatai MCP）
   - Agent B: Cursor（配置了 Ergatai MCP）

2. **在 Claude Code 中**：
   ```
   用户：列出所有可用的 agents
   Claude：[调用 list_agents tool]
   Claude：当前连接的 agents：
   - claude-code (active)
   - cursor (active)
   
   用户：让 cursor agent 帮我重构 src/auth.rs
   Claude：[调用 send_message tool]
   Claude：已发送消息给 cursor agent
   ```

3. **Cursor 收到消息**：
   - Cursor 通过 MCP 接收到任务
   - 执行重构任务
   - 返回结果

## 高级配置

### 认证（可选）

如果启用了 API token：

```bash
# 启动时设置 token
./target/release/ergatai-api --port 3000 --api-token your-secret-token
```

配置中添加 header：

```json
{
  "mcpServers": {
    "ergatai": {
      "url": "http://localhost:3000/mcp",
      "headers": {
        "Authorization": "Bearer your-secret-token"
      }
    }
  }
}
```

### 远程 MCP Server

如果 Ergatai 运行在远程服务器：

```json
{
  "mcpServers": {
    "ergatai": {
      "url": "https://your-server.com/mcp",
      "headers": {
        "Authorization": "Bearer your-token"
      }
    }
  }
}
```

## MCP Tools 参考

### `list_agents`

列出所有连接的 agents。

**参数**：
```json
{
  "include_capabilities": true
}
```

**返回**：
```json
{
  "agents": [
    {
      "agent_id": "claude-code",
      "status": "active",
      "capabilities": ["chat", "code"],
      "connected_at": "2026-08-14T10:00:00Z",
      "last_heartbeat": "2026-08-14T10:05:00Z"
    }
  ],
  "total": 1
}
```

### `send_message`

发送消息给其他 agent。

**参数**：
```json
{
  "target_agent_id": "cursor",
  "message": "请帮我重构 src/auth.rs",
  "message_type": "request"
}
```

**返回**：
```json
{
  "message_id": "msg-123",
  "status": "sent",
  "target_agent_id": "cursor",
  "message_type": "request"
}
```

### `submit_orchestration`

提交多 agent 协作任务（DAG 工作流）。

**参数**：
```json
{
  "dag_definition": "## Task A\n- agent: claude-code\n- task: analyze code\n\n## Task B\n- agent: cursor\n- task: write tests\n- depends_on: [Task A]",
  "context": {
    "project": "ergatai"
  }
}
```

**返回**：
```json
{
  "dag_id": "dag-123",
  "status": "submitted",
  "message": "DAG workflow submitted successfully"
}
```

### `check_dag_status`

查询 DAG 执行状态。

**参数**：
```json
{
  "dag_id": "dag-123"
}
```

**返回**：
```json
{
  "dag_id": "dag-123",
  "status": "running",
  "progress": {
    "total_nodes": 3,
    "completed_nodes": 1,
    "failed_nodes": 0
  },
  "results": {}
}
```

## 架构说明

```
Agent A ←→ MCP ←→ Ergatai ←→ ACP ←→ Agent B
```

- **MCP**: Agent → Ergatai（agent 发送消息）
- **ACP**: Ergatai → Agent（Ergatai 转发消息给其他 agents）

Ergatai 在中间作为消息 relay，让 agents 可以互相通信。

## 故障排查

### Agent 无法连接

1. 检查 Ergatai server 是否运行：
   ```bash
   curl http://localhost:3000/health
   ```

2. 检查 MCP endpoint：
   ```bash
   curl -X POST http://localhost:3000/mcp \
     -H "Content-Type: application/json" \
     -d '{"jsonrpc":"2.0","method":"ping","id":1}'
   ```

3. 检查 agent 配置文件格式是否正确

### Agent 列表为空

- 确保其他 agents 也配置了 Ergatai MCP
- 检查 agents 是否已启动并连接

## 更多问题？

查看 [完整文档](https://github.com/ergatai/ergatai) 或提交 Issue。

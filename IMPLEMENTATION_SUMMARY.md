# Ergatai 消息路由完整实现

## 架构理解

```
┌─────────────────────────────────────────────────────────────────┐
│                        消息路由流程                              │
└─────────────────────────────────────────────────────────────────┘

Agent A                    Ergatai                      Agent B
   │                          │                             │
   │ 1. MCP: send_message     │                             │
   │ ────────────────────────>│                             │
   │    (工具调用)             │                             │
   │                          │                             │
   │                          │ 2. NATS: 发布消息           │
   │                          │    ergatai.agent.message.B  │
   │                          │    (可靠传输)               │
   │                          │                             │
   │                          │ 3. 返回成功                 │
   │ ◄────────────────────────│    {status: "routed"}       │
   │                          │                             │
   │                          │ 4. 转发服务订阅 NATS        │
   │                          │    (后台任务)               │
   │                          │                             │
   │                          │ 5. ACP: 发送 PromptRequest  │
   │                          │ ───────────────────────────>│
   │                          │    POST /acp                │
   │                          │    {method: "session/prompt"}│
   │                          │    (标准 ACP 协议)          │
   │                          │                             │
   │                          │ 6. Agent B 处理消息         │
   │                          │                             │
```

## 三层架构

### 1. MCP (Model Context Protocol) - 工具层

**作用**: Agent 调用 Ergatai 提供的工具

**实现**: `crates/ergatai-api/src/mcp/server.rs`

**工具**:
- `list_agents` - 查看所有连接的 agent
- `send_message` - 发送消息给其他 agent
- `set_acp_endpoint` - 注册 ACP endpoint

**协议**: MCP 2025-06-18, Streamable HTTP transport

**示例**:
```json
// Agent A 调用 send_message
POST /mcp
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "tools/call",
  "params": {
    "name": "send_message",
    "arguments": {
      "target_agent_id": "agent-b",
      "message": "Hello from Agent A!"
    }
  }
}
```

### 2. NATS (Messaging System) - 传输层

**作用**: 可靠的消息路由和传输

**实现**: `crates/ergatai-nats/src/event_bus.rs`

**功能**:
- 发布消息到 `ergatai.agent.message.{agent_id}`
- 订阅所有 agent 消息
- JetStream 持久化（可选）

**代码**:
```rust
// send_message 工具中
let bus = EventBus::new(conn);
let payload = AgentMessagePayload {
    from_agent: "agent-a".to_string(),
    to_agent: "agent-b".to_string(),
    content: message.to_string(),
    timestamp,
    metadata: HashMap::new(),
};
bus.publish_agent_message(&payload).await?;
```

### 3. ACP (Agent Client Protocol) - 对话层

**作用**: Agent 之间的实际通信协议

**实现**: 
- Client: `crates/ergatai-acp/src/http_client.rs`
- Forwarder: `crates/ergatai-api/src/mcp/message_forwarder.rs`

**协议**: 标准 ACP v1
- `initialize` - 建立连接
- `session/new` - 创建会话
- `session/prompt` - 发送消息

**代码**:
```rust
// NATS → ACP 转发服务
let manager = http_connection_manager();

// 连接到 agent (如果还没连接)
manager.connect(
    &payload.to_agent,
    acp_endpoint,
    cwd,
    SessionKind::Chat,
).await?;

// 发送 ACP PromptRequest
let message_text = format!(
    "来自 {} 的消息:\n\n{}",
    payload.from_agent, payload.content
);
manager.send_prompt(&payload.to_agent, message_text).await?;
```

## 完整数据流

```
1. Agent A 调用 MCP 工具
   └─> POST /mcp (send_message)

2. Ergatai 验证并路由
   ├─> 检查目标 agent 有 ACP endpoint
   ├─> 发布到 NATS: ergatai.agent.message.B
   └─> 返回 {status: "routed"}

3. NATS 转发服务收到消息
   └─> 订阅 ergatai.agent.message.*

4. 转发服务查找目标 agent
   └─> registry.get_acp_endpoint("agent-b")
       -> "http://localhost:8080"

5. 转发服务使用 ACP Client
   ├─> HttpConnectionManager.connect()
   │   ├─> HttpClient::new("http://localhost:8080")
   │   ├─> POST /acp {method: "initialize"}
   │   └─> POST /acp {method: "session/new"}
   └─> HttpConnectionManager.send_prompt()
       └─> POST /acp {method: "session/prompt"}

6. Agent B 通过 ACP 收到消息
   └─> 标准 ACP PromptRequest
```

## 代码位置

### MCP 工具实现
- `crates/ergatai-api/src/mcp/server.rs`
  - `send_message()` - 使用 NATS 发布消息
  - `set_acp_endpoint()` - 注册 ACP endpoint
  - `list_agents()` - 查看所有 agent

### NATS EventBus
- `crates/ergatai-nats/src/event_bus.rs`
  - `publish_agent_message()` - 发布到 NATS
  - `subscribe_all_agent_messages()` - 订阅所有消息

### NATS → ACP 转发
- `crates/ergatai-api/src/mcp/message_forwarder.rs`
  - `start_nats_acp_forwarder()` - 启动后台任务
  - `handle_nats_message()` - 处理单个消息
  - `forward_via_acp()` - 使用 ACP Client 转发

### ACP Client
- `crates/ergatai-acp/src/http_client.rs`
  - `HttpAcpClient` - ACP HTTP client
  - `HttpConnectionManager` - 管理多个连接
  - 使用 `agent-client-protocol` SDK

## 测试步骤

### 1. 启动 Ergatai
```bash
cargo run -p ergatai-api -- --port 3000
```

日志应该显示:
```
MCP server initialized (protocol 2025-06-18, Streamable HTTP)
NATS → ACP message forwarder started
Subscribed to NATS agent messages (ergatai.agent.message.*)
```

### 2. 启动支持 ACP 的 Agent

需要一个实现标准 ACP 协议的 agent。例如：

```bash
# 启动 agent (端口 8080)
my-acp-agent --port 8080
```

Agent 需要：
- 暴露标准 `/acp` endpoint
- 实现 ACP 协议方法（initialize, session/new, session/prompt）

### 3. 注册 ACP Endpoint

在 agent 中调用：
```json
POST /mcp
{
  "method": "tools/call",
  "params": {
    "name": "set_acp_endpoint",
    "arguments": {
      "agent_id": "my-agent",
      "endpoint": "http://localhost:8080"
    }
  }
}
```

### 4. 发送消息

从另一个 agent 调用：
```json
POST /mcp
{
  "method": "tools/call",
  "params": {
    "name": "send_message",
    "arguments": {
      "target_agent_id": "my-agent",
      "message": "Hello!"
    }
  }
}
```

### 5. 验证

应该看到:
1. ✅ send_message 返回 `{status: "routed"}`
2. ✅ Ergatai 日志: "Received NATS message: from=..., to=my-agent"
3. ✅ Ergatai 日志: "Forwarding message to my-agent at http://localhost:8080 via ACP"
4. ✅ Ergatai 日志: "Connected to my-agent, sending message"
5. ✅ Agent 收到 ACP PromptRequest

## 关键改进

### 之前的问题
1. ❌ 误解 ACP - 以为是自定义 REST API
2. ❌ 发送自定义 JSON 到 `/acp/message`
3. ❌ 没有使用 ACP SDK

### 现在的实现
1. ✅ 正确使用 ACP 协议 - 标准 JSON-RPC 方法
2. ✅ 使用 `agent-client-protocol` SDK
3. ✅ 通过标准 ACP HTTP transport (`POST /acp`)
4. ✅ 发送 `PromptRequest`（不是自定义格式）

## 关于 OpenCode

### OpenCode TUI 模式
- ❌ 不支持 ACP
- ✅ 支持 MCP
- 只能调用工具，不能接收消息

### OpenCode ACP 模式
```bash
opencode acp
```
- ✅ 支持 ACP
- ❌ 之前测试时有崩溃问题
- 需要修复 OpenCode 的 ACP 实现

### 替代方案
如果 OpenCode ACP 模式不稳定，可以：
1. 实现 ACP 代理层（在 OpenCode TUI 和 ACP 之间）
2. 使用其他支持 ACP 的 agent
3. 自己实现一个简单的 ACP agent 用于测试

## 总结

**正确的架构**:
- MCP = Agent 调用 Ergatai 工具（send_message, list_agents）
- NATS = 可靠的消息传输层（发布/订阅）
- ACP = Agent 之间的实际对话协议（标准 ACP v1）

**完整的流程**:
```
Agent A → MCP (send_message) → NATS → ACP Client → Agent B
```

**关键代码**:
- `message_forwarder.rs` - NATS → ACP 转发
- `http_client.rs` - ACP Client 实现
- `server.rs` - MCP 工具实现

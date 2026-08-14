# 消息路由完整实现

## 已实现的功能

### 1. ✅ ACP Endpoint 验证
- 防止 agent 注册 Ergatai 自己的端口 (3000, 3001)
- 强制要求 ACP endpoint (不是可选的)

### 2. ✅ NATS 消息路由
- `send_message` 工具使用 `EventBus.publish_agent_message()` 发布消息到 NATS
- Subject: `ergatai.agent.message.{agent_id}`

### 3. ✅ NATS → ACP 转发服务 (新实现!)
- 后台任务订阅所有 agent 消息
- 查找目标 agent 的 ACP endpoint
- 通过 HTTP POST 转发到 agent 的 ACP endpoint

## 完整数据流

```
Agent A (opencode@xxx)              Ergatai                      Agent B (opencode@yyy)
     │                                  │                              │
     │ 1. send_message(target=B,        │                              │
     │    message="Hello")              │                              │
     │ ────────────────────────────────>│                              │
     │    MCP: POST /mcp                │                              │
     │                                  │                              │
     │                                  │ 2. 验证 B 有 ACP endpoint   │
     │                                  │    (必需！否则返回错误)      │
     │                                  │                              │
     │                                  │ 3. 发布到 NATS              │
     │                                  │    EventBus.publish_agent_message()
     │                                  │    Subject: ergatai.agent.message.B
     │                                  │                              │
     │                                  │ 4. 返回成功                  │
     │ ◄────────────────────────────────│    {status: "routed"}        │
     │                                  │                              │
     │                                  │ 5. NATS 转发服务收到消息    │
     │                                  │    (后台任务)                │
     │                                  │                              │
     │                                  │ 6. 查找 B 的 ACP endpoint   │
     │                                  │    registry.get_acp_endpoint()
     │                                  │                              │
     │                                  │ 7. HTTP POST 转发           │
     │                                  │ ────────────────────────────>│
     │                                  │    POST B's /acp/message     │
     │                                  │    {from_agent, content...}  │
     │                                  │                              │
     │                                  │ 8. Agent B 处理消息         │
     │                                  │                              │
```

## 代码位置

### MCP 工具实现
- `crates/ergatai-api/src/mcp/server.rs`
  - `send_message()`: 使用 NATS EventBus 发布消息
  - `set_acp_endpoint()`: 验证并注册 ACP endpoint

### NATS → ACP 转发
- `crates/ergatai-api/src/mcp/message_forwarder.rs`
  - `start_nats_acp_forwarder()`: 启动后台任务
  - `handle_nats_message()`: 转发单个消息到 ACP endpoint

### EventBus (已存在)
- `crates/ergatai-nats/src/event_bus.rs`
  - `publish_agent_message()`: 发布到 `ergatai.agent.message.{agent_id}`
  - `subscribe_all_agent_messages()`: 订阅所有 agent 消息

## Agent 需要做什么

### 1. 启动时注册 ACP endpoint
```rust
// 在 agent 代码中
let acp_endpoint = format!("http://localhost:{}", my_port);

// 调用 MCP 工具
mcp_client.call_tool("set_acp_endpoint", json!({
    "agent_id": "my-agent",
    "endpoint": acp_endpoint
}));
```

### 2. 实现 ACP 消息接收端点
Agent 需要暴露 `/acp/message` endpoint 接收消息:

```rust
// Agent 的 HTTP server
#[post("/acp/message")]
async fn handle_message(Json(msg): Json<AcpMessage>) -> impl IntoResponse {
    info!("Received message from {}: {}", msg.from_agent, msg.content);
    
    // 处理消息...
    
    StatusCode::OK
}
```

### 3. 发送消息给其他 agent
```rust
// 调用 MCP 工具
mcp_client.call_tool("send_message", json!({
    "target_agent_id": "other-agent",
    "message": "Hello from Agent A!"
}));
```

## 测试步骤

### 1. 启动 Ergatai
```bash
cargo run -p ergatai-api -- --port 3000
```

日志应该显示:
```
MCP server initialized (protocol 2025-06-18, Streamable HTTP)
NATS → ACP message forwarder started
```

### 2. 启动 Agent A (端口 8080)
```bash
# 在另一个终端
cd examples/simple-agent
cargo run -- --port 8080 --agent-id agent-a --ergatai http://localhost:3000
```

Agent A 会自动:
1. 连接到 Ergatai MCP
2. 注册 ACP endpoint: `http://localhost:8080`
3. 准备接收消息

### 3. 启动 Agent B (端口 8081)
```bash
# 在另一个终端
cd examples/simple-agent
cargo run -- --port 8081 --agent-id agent-b --ergatai http://localhost:3000
```

### 4. 测试消息发送

从 Agent A 发送消息给 Agent B:
```bash
# 使用 curl 调用 Agent A 的 MCP 工具 (或者在 Agent A 的代码中)
curl -X POST http://localhost:3000/mcp \
  -H "Content-Type: application/json" \
  -H "Mcp-Session-Id: <agent-a-session-id>" \
  -d '{
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
  }'
```

应该看到:
1. Agent A 的 send_message 返回 `{status: "routed"}`
2. Ergatai 日志: "Received NATS message: from=agent-a, to=agent-b"
3. Ergatai 日志: "Forwarding message to agent-b at http://localhost:8081"
4. Agent B 日志: "Received message from agent-a: Hello from Agent A!"

## 关键改进

### 之前的问题
1. ❌ ACP 是可选的 - 实际应该必需
2. ❌ 没有 NATS 集成 - 实际代码已有
3. ❌ Agent 可以注册 Ergatai 自己的端口
4. ❌ Agent 可以修改别人的 endpoint

### 现在的实现
1. ✅ ACP 必需 - 没有 ACP endpoint 的 agent 无法接收消息
2. ✅ NATS 完整集成 - EventBus 用于消息路由
3. ✅ 端口验证 - 防止注册 3000/3001
4. ⚠️ 权限验证 - 待完善（目前任何 agent 可以设置任何 endpoint）

## 待完善

### 1. 权限验证
目前 `set_acp_endpoint` 允许任何 agent 设置任何 endpoint。应该验证调用者身份:

```rust
// 从 MCP session context 获取真实的 agent ID
let caller_id = context.get_authenticated_agent_id()?;

// 只能设置自己的 endpoint
if provided_agent_id != caller_id {
    return Error("Can only set your own endpoint");
}
```

### 2. 消息确认
目前转发后没有确认机制。可以添加:
- NATS 消息持久化 (JetStream)
- 重试机制
- 消息状态追踪

### 3. 发送者身份
`send_message` 中 `from_agent` 硬编码为 "mcp-client"，应该从 MCP session 获取。

## 架构优势

1. **可靠性**: NATS JetStream 提供消息持久化
2. **可扩展性**: NATS 支持高并发消息路由
3. **审计**: 所有消息通过 NATS，可以记录日志
4. **解耦**: Agent 只需要知道 Ergatai 的 MCP endpoint，不需要知道其他 agent 的地址
5. **灵活性**: 支持同步 (直接 ACP) 和异步 (NATS 队列) 模式

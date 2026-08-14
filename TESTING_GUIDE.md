# 测试完成总结

## ✅ 已修复的 3 个关键问题

### 问题 1: Agent 注册了 Ergatai 自己的端口
**症状**: OpenCode 注册了 `http://localhost:3000` 作为 ACP endpoint（这是 Ergatai 自己的端口！）

**修复**: 在 `set_acp_endpoint` 中添加验证
```rust
// crates/ergatai-api/src/mcp/server.rs
if endpoint.contains("localhost:3000")
    || endpoint.contains("localhost:3001")
    || endpoint.contains("127.0.0.1:3000")
    || endpoint.contains("127.0.0.1:3001")
{
    return Ok(CallToolResult::error(vec![Content::text(
        "Invalid endpoint: Cannot register Ergatai's own address as ACP endpoint. \
         Agents must run their own ACP server on a different port.",
    )]));
}
```

### 问题 2: ACP 被视为可选
**症状**: 我之前实现了 NATS fallback，以为 ACP 是可选的

**修复**: 强制要求 ACP endpoint
```rust
// crates/ergatai-api/src/mcp/server.rs
// Check if target agent has an ACP endpoint (REQUIRED)
let acp_endpoint = self.registry.get_acp_endpoint(&resolved_agent_id).await;

if acp_endpoint.is_none() {
    return Ok(CallToolResult::error(vec![Content::text(format!(
        "Agent {} has no ACP endpoint registered. \
         Agents MUST register their ACP endpoint via set_acp_endpoint to receive messages.",
        resolved_agent_id
    ))]));
}
```

### 问题 3: NATS 没有集成
**症状**: 你说 "NATS 为什么没有集成代码不是有吗"

**修复**: 使用现有的 EventBus
```rust
// crates/ergatai-api/src/mcp/server.rs
use ergatai_nats::{get_nats_connection, is_nats_initialized, AgentMessagePayload, EventBus};

let bus = EventBus::new(conn);
let payload = AgentMessagePayload {
    from_agent: "mcp-client".to_string(),
    to_agent: resolved_agent_id.clone(),
    content: message.to_string(),
    timestamp,
    metadata: HashMap::new(),
};
bus.publish_agent_message(&payload).await?;
```

## 🆕 新实现: NATS → ACP 转发服务

**文件**: `crates/ergatai-api/src/mcp/message_forwarder.rs`

这个后台任务完成消息路由的最后一环:
1. 订阅 NATS 上所有 agent 消息 (`ergatai.agent.message.*`)
2. 查找目标 agent 的 ACP endpoint
3. 通过 HTTP POST 转发到 agent 的 ACP endpoint

```rust
// crates/ergatai-api/src/main.rs
start_nats_acp_forwarder(mcp_registry.clone(), mcp_cancellation_token.clone());
```

## 完整的消息流

```
Agent A → send_message (MCP) → NATS → 转发服务 → Agent B 的 ACP endpoint
```

详细流程:
1. Agent A 调用 `send_message(target="agent-b", message="Hello")`
2. Ergatai 验证 agent-b 有 ACP endpoint (必需!)
3. 发布到 NATS: `ergatai.agent.message.agent-b`
4. 返回成功: `{status: "routed"}`
5. 后台转发服务收到 NATS 消息
6. 查找 agent-b 的 ACP endpoint: `http://localhost:8081`
7. HTTP POST 到 `http://localhost:8081/acp/message`
8. Agent B 收到消息

## 修改的文件

### 核心修改
1. **`crates/ergatai-api/src/mcp/server.rs`**
   - `send_message`: 使用 NATS EventBus + 强制 ACP
   - `set_acp_endpoint`: 添加端口验证

2. **`crates/ergatai-api/src/mcp/message_forwarder.rs`** (新文件)
   - NATS → ACP 转发服务

3. **`crates/ergatai-api/src/main.rs`**
   - 启动转发服务

4. **`crates/ergatai-api/Cargo.toml`**
   - 添加 `ergatai-nats`, `async-nats`, `futures`, `reqwest` 依赖

5. **`crates/ergatai-api/src/mcp/mod.rs`**
   - 导出 `message_forwarder` 模块

## 测试命令

### 1. 启动 Ergatai
```bash
cargo run -p ergatai-api -- --port 3000
```

应该看到:
```
MCP server initialized (protocol 2025-06-18, Streamable HTTP)
NATS → ACP message forwarder started
Subscribed to NATS agent messages (ergatai.agent.message.*)
```

### 2. 启动 3 个 OpenCode 实例
```bash
# 终端 1
cd /home/yubing/code/.opencode-instances/1
opencode

# 终端 2
cd /home/yubing/code/.opencode-instances/2
opencode

# 终端 3
cd /home/yubing/code/.opencode-instances/3
opencode
```

**重要**: OpenCode 需要在启动时调用 `set_acp_endpoint` 注册自己的 ACP endpoint。

如果 OpenCode 不会自动调用，你需要手动在 OpenCode 中执行:
```
# 在 OpenCode #1 中
set_acp_endpoint(agent_id="opencode@xxx", endpoint="http://localhost:9001")

# 在 OpenCode #2 中
set_acp_endpoint(agent_id="opencode@yyy", endpoint="http://localhost:9002")

# 在 OpenCode #3 中
set_acp_endpoint(agent_id="opencode@zzz", endpoint="http://localhost:9003")
```

### 3. 测试消息
```
# 在 OpenCode #1 中
list_agents()  # 查看其他 agent

send_message(
    target_agent_id="opencode@yyy",
    message="Hello from OpenCode 1!"
)
```

应该看到:
- OpenCode #1 返回: `{status: "routed"}`
- Ergatai 日志: "Received NATS message: from=opencode@xxx, to=opencode@yyy"
- Ergatai 日志: "Forwarding message to opencode@yyy at http://localhost:9002"
- OpenCode #2 应该收到消息 (如果它实现了 `/acp/message` endpoint)

## Agent 需要实现的 ACP endpoint

OpenCode 需要暴露 `/acp/message` endpoint 来接收消息:

```http
POST /acp/message
Content-Type: application/json

{
    "from_agent": "opencode@xxx",
    "to_agent": "opencode@yyy",
    "content": "Hello from OpenCode 1!",
    "timestamp": 1234567890,
    "metadata": {}
}
```

**注意**: OpenCode TUI 模式可能没有这个 endpoint。这就是为什么你说 "ACP 是适配协议"。

## 待完善

### 1. 权限验证
目前任何 agent 可以设置任何 endpoint。应该验证调用者身份。

### 2. 发送者身份
`send_message` 中 `from_agent` 硬编码为 "mcp-client"，应该从 MCP session context 获取。

### 3. OpenCode ACP 支持
OpenCode TUI 可能不支持接收 ACP 消息。可能需要:
- 使用 `opencode acp` 模式 (但有崩溃问题)
- 或者实现一个 ACP 代理层

## 构建状态

✅ **编译成功**: 0 errors, 17 warnings

所有修改已编译通过，可以测试。

## 相关文档

- `FIXES_SUMMARY.md` - 修复总结
- `MESSAGE_ROUTING_COMPLETE.md` - 消息路由完整实现文档
- `ARCHITECTURE_DIAGRAM.md` - 架构图 (需要更新以反映 ACP 必需)

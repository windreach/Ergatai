# ACP (Agent Client Protocol) 详解

## 什么是 ACP？

**ACP (Agent Client Protocol)** 是一个**协议规范**，就像 MCP 一样。它定义了 AI agent 和 client（编辑器/工具）之间的通信方式。

## ACP vs MCP

| 特性 | MCP (Model Context Protocol) | ACP (Agent Client Protocol) |
|------|------------------------------|------------------------------|
| **目的** | 提供工具/上下文给 LLM | 管理 agent 会话和交互 |
| **方向** | Agent → 工具提供者 | Client ↔ Agent |
| **传输** | stdio, HTTP+SSE | stdio, HTTP+SSE, WebSocket |
| **角色** | Client (LLM) ↔ Server (工具) | Client (编辑器) ↔ Agent (AI) |
| **标准方法** | `tools/call`, `resources/read` | `initialize`, `session/prompt`, `session/new` |

## ACP 协议核心

### 标准 ACP 方法

```json
// 1. 初始化连接
POST /acp
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "initialize",
  "params": {
    "protocolVersion": "v1",
    "clientInfo": {"name": "ergatai", "version": "0.1.0"}
  }
}

// 2. 创建新会话
POST /acp
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "session/new",
  "params": {
    "cwd": "/path/to/project"
  }
}

// 3. 发送提示给 agent
POST /acp
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "session/prompt",
  "params": {
    "sessionId": "session-123",
    "prompt": [
      {"type": "text", "text": "请帮我重构这段代码"}
    ]
  }
}

// 4. 接收 agent 响应 (通过 SSE)
GET /acp
Accept: text/event-stream

data: {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-123","update":{...}}}
```

### HTTP Transport

ACP 的 HTTP transport 使用**单一的 `/acp` endpoint**：

- `POST /acp` - 发送 JSON-RPC 请求
- `GET /acp` - SSE 流接收响应和通知
- `DELETE /acp` - 关闭连接

**Headers**:
- `Acp-Connection-Id` - 连接标识符（initialize 后返回）
- `Acp-Session-Id` - 会话标识符（session/new 后返回）

## Ergatai 中的 ACP

### 已有实现

Ergatai 已经有完整的 ACP client 实现：

**文件**: `crates/ergatai-acp/src/http_client.rs`

```rust
use agent_client_protocol::{Client, ConnectionTo, Agent};
use agent_client_protocol_http::HttpClient;

// 创建 HTTP ACP client
let http_client = HttpClient::new("http://localhost:8080")?;

// 连接到 agent
Client.builder()
    .on_receive_notification(...)
    .on_receive_request(...)
    .connect_with(http_client, |connection| async move {
        // 1. Initialize
        let _init = connection
            .send_request(InitializeRequest::new(ProtocolVersion::V1))
            .await?;
        
        // 2. Create session
        let session = connection
            .send_request(NewSessionRequest::new(cwd))
            .await?;
        
        // 3. Send prompt
        let result = connection
            .send_request(PromptRequest::new(
                session.session_id,
                vec![ContentBlock::Text(TextContent::new("Hello!"))]
            ))
            .await?;
    })
    .await?;
```

### 正确使用方式

当 Ergatai 要向 agent 发送消息时：

1. **创建 `HttpAcpClient`**
   ```rust
   let client = HttpAcpClient::new("agent-b", "http://localhost:8080")?;
   ```

2. **连接并创建会话**
   ```rust
   let session = client.connect(cwd, SessionKind::HttpAcp).await?;
   ```

3. **发送 PromptRequest**
   ```rust
   session.send_prompt("来自 Agent A 的消息: Hello!").await?;
   ```

## 我之前实现的错误

### ❌ 错误实现

```rust
// 发送自定义 JSON 到 /acp/message
let acp_request = serde_json::json!({
    "from_agent": "agent-a",
    "to_agent": "agent-b",
    "content": "Hello",
    "timestamp": 1234567890
});

http_client.post(format!("{}/acp/message", endpoint))
    .json(&acp_request)
    .send()
    .await?;
```

**问题**:
1. `/acp/message` 不是标准 ACP endpoint
2. 自定义 JSON 格式不符合 ACP 协议
3. 没有使用 ACP SDK

### ✅ 正确实现

```rust
use ergatai_acp::http_client::HttpAcpClient;
use ergatai_acp::manager::SessionKind;

// 1. 创建 ACP client
let client = HttpAcpClient::new(&payload.to_agent, &acp_endpoint)?;

// 2. 连接并创建会话
let session = client.connect(cwd, SessionKind::HttpAcp).await?;

// 3. 发送标准 ACP PromptRequest
let message_text = format!("来自 {} 的消息: {}", payload.from_agent, payload.content);
session.send_prompt(&message_text).await?;
```

## OpenCode 和 ACP

### OpenCode TUI 模式

OpenCode TUI（交互式终端）**不支持 ACP**，因为：
- TUI 模式是为人工交互设计的
- 没有暴露 `/acp` HTTP endpoint
- 只能通过 MCP 调用工具

### OpenCode ACP 模式

```bash
opencode acp
```

这会启动一个 ACP server，暴露标准 `/acp` endpoint。但是：
- 之前测试时发现会崩溃（与某些 provider 不兼容）
- 需要修复 OpenCode 的 ACP 模式

## 测试方案

### 方案 1: 使用支持 ACP 的 agent

找一个支持 ACP 的 agent（或者自己实现一个），然后测试：

```bash
# 启动支持 ACP 的 agent
my-acp-agent --port 8080

# 在 agent 中注册 ACP endpoint
set_acp_endpoint(agent_id="my-agent", endpoint="http://localhost:8080")

# 发送消息
send_message(target_agent_id="my-agent", message="Hello!")
```

Ergatai 会：
1. 发布消息到 NATS
2. 转发服务收到消息
3. 使用 `HttpAcpClient` 连接 agent
4. 发送标准 ACP `PromptRequest`
5. Agent 通过 ACP 协议收到消息

### 方案 2: 实现 ACP 代理层

为 OpenCode TUI 实现一个 ACP 代理：

```
OpenCode TUI ←→ ACP Proxy ←→ Ergatai
   (MCP)         (HTTP+ACP)    (NATS)
```

- ACP Proxy 暴露标准 `/acp` endpoint
- 接收 ACP `PromptRequest`
- 转换为 MCP 工具调用或其他方式传给 OpenCode TUI

## 总结

**ACP 是什么**:
- ✅ 协议规范（像 MCP）
- ✅ 支持多种传输（stdio, HTTP, WebSocket）
- ✅ 定义标准方法（initialize, session/prompt, etc.）
- ✅ 使用 JSON-RPC 2.0

**ACP 不是什么**:
- ❌ 不是自定义 REST API
- ❌ 不是 `/acp/message` endpoint
- ❌ 不是 agent 必须实现的特定接口

**Ergatai 应该**:
- ✅ 使用 `agent-client-protocol` SDK
- ✅ 发送标准 ACP 消息
- ✅ 通过 HTTP transport 连接到 agent 的 `/acp` endpoint

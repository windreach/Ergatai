# ACP SDK 使用指南

> Agent Client Protocol Rust SDK v2 使用文档

## 概述

ACP (Agent Client Protocol) 是 Zed + JetBrains 联合发布的开放标准，用于编辑器与 AI Agent 之间的通信。

**核心特性**：
- JSON-RPC 2.0 over stdio（本地 agent）
- 支持会话管理、工具调用、权限请求
- 官方 Rust SDK：`agent-client-protocol` crate v2

## 安装依赖

```toml
# Cargo.toml
[dependencies]
agent-client-protocol = "2"
agent-client-protocol-tokio = "0.11"
tokio = { version = "1.37", features = ["full"] }
```

## 核心概念

### 1. Client（客户端）

客户端是连接到 Agent 的程序（如编辑器、CLI）。

**职责**：
- 启动 Agent 进程
- 发送 initialize 请求
- 创建会话
- 发送 prompt
- 接收响应和通知

### 2. Agent（代理）

Agent 是使用 AI 修改代码的程序。

**职责**：
- 响应 initialize
- 创建会话
- 处理 prompt
- 发送更新通知
- 请求权限

### 3. Connection（连接）

`ConnectionTo<Counterpart>` 是 Client 和 Agent 之间的通信通道。

**关键特性**：
- **Clone**：可以廉价克隆，所有克隆共享同一底层连接
- **Send + Sync**：可以跨线程使用
- 通过 `connect_with` 闭包创建

### 4. AcpAgent（Agent 配置）

`AcpAgent` 封装了 agent 进程的启动配置：

```rust
use agent_client_protocol::{AcpAgent, AcpAgentConfig};

let agent = AcpAgent::new(
    AcpAgentConfig::new("python")
        .arg("agent.py")
        .env("RUST_LOG", "debug"),
);
```

## 使用示例

### 示例 1：简单客户端（一次性 prompt）

```rust
use agent_client_protocol::schema::v1::{
    ContentBlock, InitializeRequest, NewSessionRequest, PromptRequest,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionNotification, SessionUpdate, TextContent,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{
    AcpAgent, AcpAgentConfig, Agent, Client, ConnectionTo,
};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. 创建 Agent 配置
    let agent_config = AcpAgentConfig::new("python")
        .arg("my_agent.py")
        .env("API_KEY", "xxx");
    let agent = AcpAgent::new(agent_config);

    // 2. 构建 Client 并连接
    Client.builder()
        // 处理通知（agent 推送的消息）
        .on_receive_notification(
            async |notification: SessionNotification, _connection: ConnectionTo<Agent>| {
                println!("收到通知: {:?}", notification.update);
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        // 处理权限请求
        .on_receive_request(
            async |request: RequestPermissionRequest, responder, _connection: ConnectionTo<Agent>| {
                // 自动批准所有权限请求
                let option_id = request.options.first().map(|opt| opt.option_id.clone());
                if let Some(id) = option_id {
                    responder.respond(RequestPermissionResponse::new(
                        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id)),
                    ))
                } else {
                    responder.respond(RequestPermissionResponse::new(
                        RequestPermissionOutcome::Cancelled,
                    ))
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        // 连接到 Agent
        .connect_with(agent, |connection: ConnectionTo<Agent>| async move {
            // 3. 初始化
            let init_response = connection
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            println!("Agent 已初始化: {:?}", init_response.agent_info);

            // 4. 创建会话
            let new_session_response = connection
                .send_request(NewSessionRequest::new(PathBuf::from("/project")))
                .block_task()
                .await?;
            let session_id = new_session_response.session_id;
            println!("会话已创建: {:?}", session_id);

            // 5. 发送 prompt
            let prompt_response = connection
                .send_request(PromptRequest::new(
                    session_id.clone(),
                    vec![ContentBlock::Text(TextContent::new("帮我写一个 hello world"))],
                ))
                .block_task()
                .await?;

            println!("Agent 完成，停止原因: {:?}", prompt_response.stop_reason);
            Ok(())
        })
        .await?;

    Ok(())
}
```

### 示例 2：Ergatai 集成（后台会话管理）

Ergatai 使用 channel + 后台 task 模式管理会话生命周期：

```rust
use agent_client_protocol::schema::v1::{
    ContentBlock, InitializeRequest, NewSessionRequest, PromptRequest,
    SessionNotification, SessionUpdate, TextContent,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Agent, Client, ConnectionTo};
use tokio::sync::{mpsc, oneshot};
use std::sync::{Arc, Mutex};

enum SessionCommand {
    SendPrompt { text: String, reply_tx: oneshot::Sender<anyhow::Result<()>> },
    Close,
}

pub fn spawn_session_task(
    command: String,
    args: Vec<String>,
    env: std::collections::HashMap<String, String>,
    cwd: String,
    session_id_tx: oneshot::Sender<anyhow::Result<String>>,
) {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<SessionCommand>();
    let session_id_tx = Arc::new(Mutex::new(Some(session_id_tx)));

    let mut agent_config = AcpAgentConfig::new(&command).args(args);
    for (k, v) in &env {
        agent_config = agent_config.env(k, v);
    }
    let agent = AcpAgent::new(agent_config);

    tokio::spawn({
        let session_id_tx = session_id_tx.clone();
        async move {
            let result = Client.builder()
                .on_receive_notification(
                    async |notification: SessionNotification, _connection: ConnectionTo<Agent>| {
                        // 转发通知到全局事件 channel
                        println!("Session update: {:?}", notification.update);
                        Ok(())
                    },
                    agent_client_protocol::on_receive_notification!(),
                )
                .on_receive_request(
                    async |request: RequestPermissionRequest, responder, _connection| {
                        // YOLO: 自动批准
                        let id = request.options.first().map(|o| o.option_id.clone());
                        if let Some(id) = id {
                            responder.respond(RequestPermissionResponse::new(
                                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id)),
                            ))
                        } else {
                            responder.respond(RequestPermissionResponse::new(
                                RequestPermissionOutcome::Cancelled,
                            ))
                        }
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .connect_with(agent, {
                    let tx_for_closure = session_id_tx.clone();
                    move |connection: ConnectionTo<Agent>| async move {
                        let tx = tx_for_closure.lock().unwrap().take();

                        // 初始化
                        connection
                            .send_request(InitializeRequest::new(ProtocolVersion::V1))
                            .block_task()
                            .await?;

                        // 创建会话
                        let resp = connection
                            .send_request(NewSessionRequest::new(std::path::PathBuf::from(&cwd)))
                            .block_task()
                            .await?;
                        let session_id = resp.session_id.to_string();

                        // 通知调用者
                        if let Some(tx) = tx {
                            let _ = tx.send(Ok(session_id.clone()));
                        }

                        // 命令循环（闭包永不返回，直到 Close 或出错）
                        let mut cmd_rx = cmd_rx;
                        let session_id_arc = SessionId::new(session_id.clone());
                        loop {
                            match cmd_rx.recv().await {
                                Some(SessionCommand::SendPrompt { text, reply_tx }) => {
                                    let result = connection
                                        .send_request(PromptRequest::new(
                                            session_id_arc.clone(),
                                            vec![ContentBlock::Text(TextContent::new(text))],
                                        ))
                                        .block_task()
                                        .await
                                        .map(|_| ())
                                        .map_err(|e| anyhow::anyhow!("Prompt failed: {}", e));
                                    let _ = reply_tx.send(result);
                                }
                                Some(SessionCommand::Close) | None => break,
                            }
                        }
                        Ok(())
                    }
                })
                .await;

            if let Err(e) = result {
                if let Some(tx) = session_id_tx.lock().unwrap().take() {
                    let _ = tx.send(Err(anyhow::anyhow!("Connection failed: {}", e)));
                }
            }
        }
    });
}
```

## API 参考

### Client 构建器

```rust
Client.builder()
    // 处理通知
    .on_receive_notification(handler, on_receive_notification!())
    // 处理请求
    .on_receive_request(handler, on_receive_request!())
    // 连接到 agent
    .connect_with(agent, |connection: ConnectionTo<Agent>| async { ... })
    .await
```

**注意**：
- `Client` 是 struct，用 `Client.builder()` 而不是 `Client::builder()`
- 通知处理器签名：`async |notification, connection| { ... }`
- 请求处理器签名：`async |request, responder, connection| { ... }`

### 常用请求

| 请求 | 用途 | 参数 |
|------|------|------|
| `InitializeRequest` | 初始化连接 | `ProtocolVersion::V1` |
| `NewSessionRequest` | 创建会话 | `cwd: impl Into<PathBuf>` |
| `PromptRequest` | 发送提示词 | `session_id: impl Into<SessionId>`, `prompt: Vec<ContentBlock>` |
| `LoadSessionRequest` | 加载已有会话 | `session_id` |
| `ListSessionsRequest` | 列出所有会话 | 无（需要 `unstable_session_list` feature） |

### SessionId

```rust
use agent_client_protocol::schema::v1::SessionId;

// 从字符串创建
let session_id = SessionId::new("session-123");

// 转为字符串
let id_str = session_id.to_string();
```

### ProtocolVersion

```rust
use agent_client_protocol::schema::ProtocolVersion;

// 使用常量
let v1 = ProtocolVersion::V1;
// let v2 = ProtocolVersion::V2; // 需要 unstable_protocol_v2 feature
```

### 通知类型

```rust
use agent_client_protocol::schema::v1::{SessionNotification, SessionUpdate};

SessionNotification {
    session_id: SessionId,
    update: SessionUpdate,
    meta: Option<Meta>,
}

enum SessionUpdate {
    UserMessageChunk(ContentChunk),
    AgentMessageChunk(ContentChunk),
    AgentThoughtChunk(ContentChunk),
    ToolCall(ToolCall),
    ToolCallUpdate(ToolCallUpdate),
    Plan(Plan),
    AvailableCommandsUpdate(AvailableCommandsUpdate),
    CurrentModeUpdate(CurrentModeUpdate),
    ConfigOptionUpdate(ConfigOptionUpdate),
    SessionInfoUpdate(SessionInfoUpdate),
    UsageUpdate(UsageUpdate),
    // ... (#[non_exhaustive])
}
```

### 内容块

```rust
use agent_client_protocol::schema::v1::{ContentBlock, TextContent};

enum ContentBlock {
    Text(TextContent),
    Image(ImageContent),
    Audio(AudioContent),
    ResourceLink(ResourceLink),
    Resource(EmbeddedResource),
}

// 创建文本内容
let text = TextContent::new("Hello, world!");
let block = ContentBlock::Text(text);
```

## 关键宏

### `on_receive_notification!()`

用于注册通知处理器：

```rust
.on_receive_notification(
    async |notification: SessionNotification, connection: ConnectionTo<Agent>| {
        // 处理通知
        Ok(())
    },
    on_receive_notification!(),
)
```

### `on_receive_request!()`

用于注册请求处理器：

```rust
.on_receive_request(
    async |request: RequestPermissionRequest, responder, connection: ConnectionTo<Agent>| {
        // 处理请求并响应
        responder.respond(response)
    },
    on_receive_request!(),
)
```

## 错误处理

```rust
use agent_client_protocol::schema::v1::{Error, Result};

async fn my_function() -> Result<()> {
    // SDK 操作
    Ok(())
}
```

## 最佳实践

### 1. 环境变量

在 `AcpAgentConfig` 上设置，会正确传递给子进程：

```rust
let agent_config = AcpAgentConfig::new("python")
    .arg("agent.py")
    .env("API_KEY", "xxx")
    .env("DEBUG", "true");
let agent = AcpAgent::new(agent_config);
```

### 2. 会话生命周期

使用 channel + 后台 task 模式：

```rust
// 1. 创建 command channel
let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

// 2. Spawn 后台 task
tokio::spawn(async move {
    Client.builder()
        .connect_with(agent, |connection| async move {
            // init + create session
            // 命令循环
            loop {
                match cmd_rx.recv().await {
                    Some(cmd) => { /* 处理命令 */ }
                    None => break,
                }
            }
            Ok(())
        })
        .await
});

// 3. 通过 cmd_tx 发送命令
cmd_tx.send(SessionCommand::SendPrompt { ... });
```

### 3. 权限处理

自动批准或发送到前端让用户决定：

```rust
.on_receive_request(
    async |request: RequestPermissionRequest, responder, _connection| {
        // 选项 1：自动批准（YOLO）
        let id = request.options.first().unwrap().option_id.clone();
        responder.respond(RequestPermissionResponse::new(
            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id)),
        ));

        // 选项 2：发送到前端让用户决定
        // event_tx.send(PermissionRequest { ... });
        // 等待用户响应...
    },
    on_receive_request!(),
)
```

## 调试技巧

### 1. 启用日志

```rust
use tracing_subscriber;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    // ...
}
```

### 2. 打印所有消息

```rust
.on_receive_notification(
    async |notification: SessionNotification, _connection| {
        println!("📨 通知: {:#?}", notification);
        Ok(())
    },
    on_receive_notification!(),
)
```

## Ergatai 架构

### 数据流

```
TS: createSession("claude", "/project")
  → IPC → NAPI: acp_create_session
    → tokio::spawn: connect_with(agent, |conn| async {
        init → new_session → session_id
        oneshot_tx.send(session_id)       ← NAPI 拿到 session_id 返回
        loop {
          cmd = cmd_rx.recv()
          match cmd {
            SendPrompt(text, reply_tx) → conn.send(PromptRequest) → reply_tx.send(result)
            Close → break
          }
        }
      })
    → oneshot_rx.await → return session_id

Agent → SDK notification handler → event_tx.send(event)
TS: acp_poll_events() → event_rx.try_recv() → events → webContents.send()
```

### 关键组件

- `SessionManager`：全局会话注册表，管理多个 `SessionHandle`
- `SessionHandle`：持有 `cmd_tx` channel，用于向 session task 发送命令
- `SessionEvent`：agent 推送的事件，通过全局 event channel 收集
- `NapiSessionEvent` / `NapiSessionInfo`：NAPI 导出类型

### 设计决策

1. **连接生命周期**：`ConnectionTo<Agent>` 永远不离开 `connect_with` 闭包，零风险
2. **事件传递**：全局 `mpsc::unbounded_channel` 收集事件，TS 侧 100ms 轮询
3. **权限处理**：YOLO（自动批准），后续加前端确认
4. **多会话支持**：`SessionManager` 管理多个 session，每个有自己的 task 和 channel

## 参考链接

- **官方文档**：https://agentclientprotocol.com
- **Rust SDK**：https://github.com/agentclientprotocol/agent-client-protocol
- **API 参考**：https://docs.rs/agent-client-protocol
- **协议规范**：https://agentclientprotocol.com/specification

## 本地文档

完整的 SDK 源码和示例已克隆到：

```
docs/acp-sdk/
├── README.md                          # SDK 概述
├── src/
│   └── agent-client-protocol/
│       └── examples/
│           ├── yolo_one_shot_client.rs  # 客户端示例
│           └── simple_agent.rs          # Agent 示例
└── md/                                # 设计文档
```

## 版本说明

Ergatai 使用 `agent-client-protocol` v2.0，主要变化：

- `ConnectionTo<Counterpart>` 是 Clone + Send + Sync
- `Client` 是 struct，用 `Client.builder()` 方法
- `ProtocolVersion::V1` 是常量（不是 enum variant）
- `SessionId::new(string)` 从字符串创建
- 通知处理器签名：`async |notification, connection|`
- 请求处理器签名：`async |request, responder, connection|`

从 v0.9 迁移：
- 旧 API（`ClientSideConnection`、`?Send` trait）已废弃
- 新 API 更简洁，`ConnectionTo` 可以自由克隆和跨线程使用
- `AcpAgent` 现在使用 `AcpAgentConfig`（不是 MCP 的 `McpServer`）

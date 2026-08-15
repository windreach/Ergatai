# Ergatai 修复总结

## 问题诊断

你测试时发现了 3 个关键问题：

### 问题 1: Agent 注册了 Ergatai 自己的端口
```
Agent opencode@49059fcf registering tmux pane: http://localhost:3000
Agent opencode@740e0eb8 registering tmux pane: http://localhost:3000
```
**原因**: `register_tmux_pane` 没有验证，agent 可以注册任何端口，包括 Ergatai 自己的 3000 端口。

### 问题 2: tmux pane 冲突
两个 agent 都注册了 `localhost:3000`，导致消息路由混乱。

### 问题 3: 错误的架构理解
我之前以为 tmux 注入 是可选的，可以实现 NATS fallback。但实际架构是：
- **tmux 注入是必需的** - agent 必须有 tmux pane 才能接收消息
- **NATS 是路由层** - 消息通过 NATS 路由，但最终必须通过 tmux 注入推送给 agent

## 正确的架构

```
Agent A                      Ergatai                        Agent B
   │                            │                              │
   │ send_message(target=B)     │                              │
   │ ──────────────────────────>│                              │
   │                            │                              │
   │                            │ 1. 验证 B 有 tmux pane   │
   │                            │    (必需！)                  │
   │                            │                              │
   │                            │ 2. 发布到 NATS              │
   │                            │    subject:                  │
   │                            │    ergatai.agent.message.B   │
   │                            │                              │
   │                            │ 3. Ergatai 订阅 NATS        │
   │                            │    并转发到 B 的 tmux 注入        │
   │                            │ ────────────────────────────>│
   │                            │    POST B's /tmux endpoint    │
   │                            │                              │
   │ 4. 返回结果                │                              │
   │ <──────────────────────────│                              │
   │    {status: "routed"}      │                              │
```

**关键点**:
- Agent **必须**注册 tmux pane
- 消息通过 NATS 路由（可靠性、审计）
- 最终通过 tmux 注入 推送到 agent

## 修复内容

### 1. register_tmux_pane 添加验证

```rust
// 防止注册 Ergatai 自己的端口
if endpoint.contains("localhost:3000") 
    || endpoint.contains("localhost:3001")
    || endpoint.contains("127.0.0.1:3000")
    || endpoint.contains("127.0.0.1:3001") 
{
    return Err("Cannot register Ergatai's own address");
}
```

### 2. send_message 使用 NATS EventBus

```rust
// 检查 tmux pane (必需)
if tmux_pane.is_none() {
    return Err("Agent MUST have tmux pane");
}

// 使用 NATS EventBus 发布消息
let bus = EventBus::new(conn);
let payload = AgentMessagePayload {
    from_agent: "sender".to_string(),
    to_agent: "receiver".to_string(),
    content: message.to_string(),
    timestamp: now,
    ...
};
bus.publish_agent_message(&payload).await?;
```

### 3. 添加 ergatai-nats 依赖

```toml
# ergatai-api/Cargo.toml
[dependencies]
ergatai-nats = { path = "../ergatai-nats" }
```

## 当前状态

### ✅ 已修复
- [x] 防止注册 Ergatai 自己的端口
- [x] 使用 NATS EventBus 路由消息
- [x] 强制要求 tmux pane

### ⚠️ 待完善
- [ ] 权限验证：agent 只能设置自己的 endpoint（目前任何 agent 可以设置任何 endpoint）
- [ ] 发送者身份：从 MCP session 获取真实的 sender agent ID（目前是硬编码 "mcp-client"）
- [ ] NATS → tmux 注入 转发：需要有后台任务订阅 NATS 并转发到 tmux pane

## 下一步

### 1. 实现 NATS → tmux 注入 转发服务

需要在 Ergatai 中启动一个后台任务，订阅 `ergatai.agent.message.*` 并转发到 agent 的 tmux pane：

```rust
// 在 ergatai-api/src/main.rs 中启动
tokio::spawn(async move {
    let bus = EventBus::new(nats_conn);
    let mut subscriber = bus.subscribe_all_agent_messages().await?;
    
    while let Some(msg) = subscriber.next().await {
        let payload: AgentMessagePayload = serde_json::from_slice(&msg.payload)?;
        
        // 查找目标 agent 的 tmux pane
        if let Some(endpoint) = registry.get_tmux_pane(&payload.to_agent).await {
            // 转发到 tmux pane
            http_client.post(endpoint)
                .json(&payload)
                .send()
                .await?;
        }
    }
});
```

### 2. 添加权限验证

在 `register_tmux_pane` 中验证调用者身份：

```rust
// 从 MCP session context 获取 agent ID
let caller_agent_id = context.session.get_agent_id()?;

// 只能设置自己的 endpoint
if provided_agent_id != caller_agent_id {
    return Err("Can only set your own endpoint");
}
```

### 3. 实现 list_messages 工具（可选）

如果 agent 想主动查询消息（而不是被动接收）：

```rust
#[tool(description = "List pending messages for this agent")]
async fn list_messages(&self) -> Result<CallToolResult, ErrorData> {
    // 从 NATS 或内存队列获取消息
    let messages = message_queue.get_messages_for_agent(self.agent_id).await?;
    Ok(CallToolResult::success(messages))
}
```

## 测试步骤

1. 重启 Ergatai:
```bash
cargo run -p ergatai-api -- --port 3000
```

2. 启动 OpenCode 实例并注册 tmux pane:
```bash
./start-opencode-1.sh
# 在 OpenCode 中调用:
# register_tmux_pane(agent_id="opencode@xxx", endpoint="http://localhost:9001")
```

3. 测试消息发送:
```bash
# Agent A 调用:
send_message(target_agent_id="opencode@yyy", message="Hello!")

# 应该看到:
# 1. NATS 发布成功
# 2. Ergatai 转发到 Agent B 的 tmux pane
# 3. Agent B 收到消息
```

## 架构总结

```
┌─────────────────────────────────────────────────────────┐
│                     Ergatai (中间件)                     │
│                                                          │
│  ┌──────────┐  ┌──────────┐  ┌────────────────────┐   │
│  │MCP Server│  │ Agent    │  │ NATS EventBus      │   │
│  │ (rmcp)   │  │ Registry │  │ - 消息路由         │   │
│  └──────────┘  └──────────┘  │ - 任务分发         │   │
│       │              │        │ - 文件锁           │   │
│       │              │        └────────────────────┘   │
│       │              │                 │                 │
│       ▼              ▼                 ▼                 │
│  ┌─────────────────────────────────────────────────┐   │
│  │ 消息路由层                                       │   │
│  │ 1. 验证 tmux pane (必需)                     │   │
│  │ 2. 发布到 NATS                                   │   │
│  │ 3. 订阅 NATS → 转发到 tmux 注入                       │   │
│  └─────────────────────────────────────────────────┘   │
│                                                          │
└─────────────────────────────────────────────────────────┘
         │ MCP                                    ▲ tmux 注入
         │                                        │
    ┌────┴────┐                              ┌────┴────┐
    │ Agent A │                              │ Agent B │
    │ (MCP)   │                              │ (tmux 注入)   │
    └─────────┘                              └─────────┘
```

**核心原则**:
- MCP = Agent 调用 Ergatai 工具
- NATS = 内部消息路由
- tmux 注入 = Ergatai 推送消息给 Agent

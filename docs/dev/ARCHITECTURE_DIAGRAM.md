# Ergatai 多 Agent 协作架构图

## 1. 整体架构

```
┌─────────────────────────────────────────────────────────────────┐
│                    Agent A (OpenCode TUI #1)                     │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  MCP Client (连接到 Ergatai)                             │  │
│  │  - 调用工具: list_agents, send_message, etc.            │  │
│  │  - 接收工具返回值                                        │  │
│  └──────────────────────────────────────────────────────────┘  │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  (可选) ACP Server                                       │  │
│  │  - 接收 Ergatai 推送的任务                               │  │
│  │  - OpenCode TUI 没有这个，只有 opencode acp 才有        │  │
│  └──────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
         │ MCP (工具调用)                      ▲ ACP HTTP (可选)
         │ POST /mcp                           │ POST /acp
         ▼                                     │
┌─────────────────────────────────────────────────────────────────┐
│                        Ergatai (中间件)                          │
│                                                                  │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────────┐  │
│  │  MCP Server  │  │  Agent       │  │  ACP HTTP Client     │  │
│  │  (rmcp SDK)  │  │  Registry    │  │  (推送任务给 agent)  │  │
│  │  端口: 3000  │  │  (agent 列表)│  │                      │  │
│  └──────────────┘  └──────────────┘  └──────────────────────┘  │
│           │                    │                     │            │
│           ▼                    ▼                     ▼            │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  消息路由层 (message_relay)                               │  │
│  │  - 检查目标 agent 是否有 ACP endpoint                    │  │
│  │  - 有 ACP → HTTP 直接推送                                │  │
│  │  - 无 ACP → NATS 消息队列                                │  │
│  └──────────────────────────────────────────────────────────┘  │
│                          │                                       │
│                          ▼                                       │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  NATS Event Bus (JetStream)                              │  │
│  │  - agent.message.{agent_id} - agent 间消息               │  │
│  │  - task.submit.{agent} - DAG 任务分发                    │  │
│  │  - file.access.request - 文件锁请求                      │  │
│  └──────────────────────────────────────────────────────────┘  │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
         ▲ MCP                                    │ ACP HTTP (可选)
         │                                        ▼
┌────────┴────────────────────────────────────────────────────────┐
│                    Agent B (OpenCode TUI #2)                     │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  MCP Client (连接到 Ergatai)                             │  │
│  │  - 调用工具: list_agents, send_message, list_messages   │  │
│  └──────────────────────────────────────────────────────────┘  │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  (可选) ACP Server                                       │  │
│  │  - OpenCode TUI 没有这个                                 │  │
│  └──────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

## 2. 两个 Agent 对话的数据流

### 场景：Agent A 发消息给 Agent B

```
Agent A                    Ergatai                      Agent B
   │                           │                            │
   │ 1. send_message(          │                            │
   │   target="opencode@xxx",  │                            │
   │   message="你好"          │                            │
   │ )                         │                            │
   │ ─────────────────────────►│                            │
   │   (MCP: POST /mcp)        │                            │
   │                           │                            │
   │                           │ 2. 检查 Agent B 的 ACP     │
   │                           │    endpoint                │
   │                           │                            │
   │                           │ 3a. 有 ACP?                │
   │                           │ ┌─────────────────────────┐│
   │                           │ │ HTTP POST B's /acp      ││
   │                           │ │ (直接推送)              ││
   │                           │ └─────────────────────────┘│
   │                           │         │                  │
   │                           │         │                  │
   │                           │ 3b. 无 ACP?                │
   │                           │ ┌─────────────────────────┐│
   │                           │ │ NATS publish            ││
   │                           │ │ ergatai.agent.message.B ││
   │                           │ │ (消息队列)              ││
   │                           │ └─────────────────────────┘│
   │                           │         │                  │
   │                           │         │                  │
   │ 4. 返回结果               │         │                  │
   │ ◄─────────────────────────│         │                  │
   │   {status: "sent",        │         │                  │
   │    delivery: "nats_queue"}│         │                  │
   │                           │         │                  │
   │                           │         │  5. Agent B 获取 │
   │                           │         │     消息         │
   │                           │         │ ┌──────────────┐ │
   │                           │         │ │ list_messages│ │
   │                           │         │ │ (MCP 工具)   │ │
   │                           │         │ └──────────────┘ │
   │                           │         │ ◄────────────────│
   │                           │         │                  │
```

## 3. 我修改了什么

### 修改前（有问题）

```rust
// send_message 只能使用 ACP
async fn send_message(...) {
    // 1. 必须注册 ACP endpoint，否则报错
    if !agent.has_acp_endpoint() {
        return Error("Agent has no ACP endpoint");
    }
    
    // 2. HTTP 推送到 ACP endpoint
    http_client.post(agent.acp_endpoint).send(message);
}

// set_acp_endpoint 没有验证
async fn set_acp_endpoint(...) {
    // 任何 agent 可以注册任何 endpoint
    // 甚至可以注册 Ergatai 自己的端口！
    registry.set_endpoint(agent_id, endpoint);
}
```

**问题：**
- ❌ OpenCode TUI 没有 ACP，无法通信
- ❌ Agent 可以注册 `localhost:3000`（Ergatai 自己）
- ❌ Agent 可以修改别人的 endpoint

### 修改后（支持混合模式）

```rust
// send_message 支持 ACP + NATS 双模式
async fn send_message(...) {
    if agent.has_acp_endpoint() {
        // 模式 1: ACP HTTP 直接推送
        http_client.post(agent.acp_endpoint).send(message);
        return {delivery: "acp_http"};
    } else {
        // 模式 2: NATS 消息队列（新增！）
        nats.publish("ergatai.agent.message.{id}", message);
        return {delivery: "nats_queue"};
    }
}

// set_acp_endpoint 添加验证
async fn set_acp_endpoint(...) {
    // 1. 验证不是 Ergatai 自己的端口
    if endpoint.contains("localhost:3000") {
        return Error("Cannot register Ergatai's own address");
    }
    
    // 2. 记录日志（后续需要添加权限验证）
    registry.set_endpoint(agent_id, endpoint);
}
```

**改进：**
- ✅ OpenCode TUI 可以通过 NATS 通信
- ✅ 防止注册 Ergatai 自己的端口
- ⚠️ 权限验证待完善（目前任何 agent 可以设置任何 endpoint）

## 4. OpenCode TUI vs OpenCode ACP

| 特性 | OpenCode TUI (`opencode`) | OpenCode ACP (`opencode acp`) |
|------|---------------------------|-------------------------------|
| 启动方式 | 交互式终端 | 无头 HTTP 服务 |
| MCP Client | ✅ 有（调用 Ergatai 工具） | ✅ 有 |
| ACP Server | ❌ 无 | ✅ 有（但会崩溃） |
| 使用场景 | 人工交互 | 自动化调度 |
| 你的脚本 | ✅ 用这个 | ❌ 不稳定 |

**结论：** 你的 OpenCode 实例是 TUI 模式，没有 ACP endpoint，所以需要用 NATS 消息队列来通信。

## 5. 当前支持的通信方式

```
Agent A (有 ACP) ──ACP HTTP──→ Agent B (有 ACP)
   ✓ 直接推送                      ✓ 实时接收

Agent A (无 ACP) ──NATS──→ Ergatai ──NATS──→ Agent B (无 ACP)
   ✓ 发送消息              队列存储           ✓ 轮询获取

Agent A (任意) ──MCP──→ Ergatai ──ACP──→ Agent B (有 ACP)
   ✓ 调用 send_message    路由选择         ✓ 实时推送
```

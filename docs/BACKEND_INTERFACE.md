# 后端接口文档 - tmux 注入 + MCP 通知协议

> 本文档描述后端（Rust + TypeScript）生成的所有事件类型和转换逻辑

## 目录

- [概述](#概述)
- [系统架构](#系统架构)
- [Rust 层事件](#rust-层事件)
  - [SessionEvent 结构](#sessionevent-结构)
  - [事件类型列表](#事件类型列表)
- [TypeScript 转换层](#typescript-转换层)
  - [translateEvent 函数](#translateevent-函数)
  - [事件映射表](#事件映射表)
  - [详细转换规则](#详细转换规则)
- [Poll 机制](#poll-机制)
- [Session 生命周期](#session-生命周期)
- [错误处理](#错误处理)

---

## 概述

后端通过 **tmux 注入**（首选）或 **MCP 通知**（回退）与 Agent 通信，通过 TypeScript 中间层将 Rust 事件转换为前端可理解的格式。

**关键文件：**
- `src-rust/src/tmux/injector.rs` - tmux 注入器（向 agent 的 tmux pane 写入内容）
- `src-rust/src/mcp/server.rs` - MCP server 会话管理
- `src-rust/src/napi/events.rs` - NAPI 绑定
- `src/main/lib/trpc/routers/agent.ts` - TypeScript 转换层

---

## 系统架构

```
Agent (在 tmux pane 中运行)
    ↑ tmux send-keys 注入（首选）
    │ 或 MCP notification（回退）
Rust tmux 注入器 / MCP server
    ↓ SessionEvent
Rust Manager (evt_tx)
    ↓ mpsc channel
TypeScript NAPI (pollEvents)
    ↓ SessionEvent[]
translateEvent()
    ↓ UIMessageChunk[]
tRPC Subscription (emit.next)
    ↓ onData(chunk)
Frontend
```

---

## Rust 层事件

### SessionEvent 结构

Rust 生成的原始事件结构：

```rust
pub struct SessionEvent {
    pub session_id: String,           // Session 唯一标识
    pub event_type: String,           // 事件类型
    pub data: serde_json::Value,      // 事件数据（JSON）
}
```

**NAPI 绑定后的 TypeScript 类型：**

```typescript
interface NapiSessionEvent {
  sessionId: string
  eventType: string
  data: string  // JSON string
}
```

### 事件类型列表

| 事件类型 | 来源 | 描述 |
|---|---|---|
| `agent_message_chunk` | tmux 注入器 | Agent 文本输出增量 |
| `agent_thought_chunk` | tmux 注入器 | Agent 思考过程增量 |
| `tool_call` | tmux 注入器 | 工具调用开始 |
| `tool_call_update` | tmux 注入器 | 工具调用结果更新 |
| `permission_request` | tmux 注入器 | 权限请求（需要用户确认） |
| `usage_update` | tmux 注入器 | Token 使用量更新 |
| `available_commands_update` | tmux 注入器 | 可用命令列表更新 |
| `closed` | 本地生成 | Session 关闭 |
| `task_dispatched` | Pool 系统 | 任务已分发 |
| `task_completed` | Pool 系统 | 任务已完成 |
| `task_failed` | Pool 系统 | 任务失败 |
| `turn_started` | Observer | 轮次开始 |
| `turn_completed` | Observer | 轮次完成 |
| `model_switched` | Observer | 模型切换 |
| `pool_stopped` | Pool 系统 | Agent Pool 停止 |
| `session_info_update` | tmux 注入器 | Session 信息更新 |

---

## TypeScript 转换层

### translateEvent 函数

**位置：** `src/main/lib/trpc/routers/agent.ts:78-270`

```typescript
function translateEvent(event: NapiSessionEvent): any[] {
  const chunks: any[] = []
  let data: any

  // 解析 JSON 数据
  try {
    data = typeof event.data === "string" ? JSON.parse(event.data) : event.data
  } catch {
    data = event.data
  }

  // 根据事件类型转换为前端 chunk
  switch (event.eventType) {
    case "agent_message_chunk": { /* ... */ }
    // ... 其他 case
  }

  return chunks
}
```

### 事件映射表

| Rust eventType | 前端 chunk.type | 转换逻辑 |
|---|---|---|
| `agent_message_chunk` | `text-delta` | 提取文本内容，添加 id |
| `agent_thought_chunk` | `reasoning-delta` | 提取思考内容，添加 id |
| `tool_call` | `tool-input-start` + `tool-input-available` | 生成两个 chunk |
| `tool_call_update` | `tool-output` | 提取工具输出 |
| `permission_request` | `ask-user-question` | 转换为问题格式 |
| `usage_update` | `message-metadata` | 包装为 messageMetadata |
| `available_commands_update` | `available-commands` | 映射命令数组 |
| `closed` | `finish` | 添加 finishReason |

---

### 详细转换规则

#### agent_message_chunk → text-delta

**输入：**
```json
{
  "eventType": "agent_message_chunk",
  "data": {
    "content": "Hello, world!",
    "ContentBlock": { "text": "Hello, world!" }
  }
}
```

**输出：**
```json
{
  "type": "text-delta",
  "id": "text-session-123",
  "delta": "Hello, world!"
}
```

**转换逻辑：**
```typescript
case "agent_message_chunk": {
  const content = data?.content || data?.ContentBlock || data
  if (typeof content === "string") {
    chunks.push({
      type: "text-delta",
      id: `text-${event.sessionId}`,
      delta: content,
    })
  } else if (content?.text) {
    chunks.push({
      type: "text-delta",
      id: `text-${event.sessionId}`,
      delta: content.text,
    })
  }
  break
}
```

---

#### agent_thought_chunk → reasoning-delta

**输入：**
```json
{
  "eventType": "agent_thought_chunk",
  "data": {
    "content": "Let me think about this..."
  }
}
```

**输出：**
```json
{
  "type": "reasoning-delta",
  "id": "reasoning-session-123",
  "delta": "Let me think about this..."
}
```

**转换逻辑：**
```typescript
case "agent_thought_chunk": {
  const content = data?.content || data?.ContentBlock || data
  const text = typeof content === "string" ? content : content?.text || ""
  if (text) {
    chunks.push({
      type: "reasoning-delta",
      id: `reasoning-${event.sessionId}`,
      delta: text,
    })
  }
  break
}
```

---

#### tool_call → tool-input-start + tool-input-available

**输入：**
```json
{
  "eventType": "tool_call",
  "data": {
    "tool_name": "bash",
    "tool_input": "{\"command\": \"ls -la\"}",
    "tool_call_id": "call-123"
  }
}
```

**输出：**
```json
[
  {
    "type": "tool-input-start",
    "toolName": "bash",
    "toolCallId": "call-123"
  },
  {
    "type": "tool-input-available",
    "toolName": "bash",
    "toolCallId": "call-123",
    "args": { "command": "ls -la" }
  }
]
```

**转换逻辑：**
```typescript
case "tool_call": {
  const toolName = data?.tool_name || data?.toolName || "unknown"
  const toolInput = data?.tool_input || data?.toolInput || data?.input || {}
  const toolCallId = data?.tool_call_id || data?.toolCallId || `tc-${Date.now()}`
  chunks.push({
    type: "tool-input-start",
    toolName,
    toolCallId,
  })
  chunks.push({
    type: "tool-input-available",
    toolName,
    toolCallId,
    args: typeof toolInput === "string" ? tryParseJSON(toolInput) : toolInput,
  })
  break
}
```

---

#### tool_call_update → tool-output

**输入：**
```json
{
  "eventType": "tool_call_update",
  "data": {
    "tool_name": "bash",
    "result": "total 24\ndrwxr-xr-x 5 user staff 160 Aug 9 12:00 .",
    "tool_call_id": "call-123"
  }
}
```

**输出：**
```json
{
  "type": "tool-output",
  "toolName": "bash",
  "toolCallId": "call-123",
  "output": "total 24\ndrwxr-xr-x 5 user staff 160 Aug 9 12:00 ."
}
```

**转换逻辑：**
```typescript
case "tool_call_update": {
  const toolName = data?.tool_name || data?.toolName || "unknown"
  const result = data?.result || data?.output || ""
  const toolCallId = data?.tool_call_id || data?.toolCallId || ""
  chunks.push({
    type: "tool-output",
    toolName,
    toolCallId,
    output: typeof result === "string" ? result : JSON.stringify(result),
  })
  break
}
```

---

#### permission_request → ask-user-question

**输入：**
```json
{
  "eventType": "permission_request",
  "data": {
    "requestId": "perm-123",
    "options": [
      { "optionId": "allow", "label": "Allow" },
      { "optionId": "deny", "label": "Deny" }
    ],
    "toolName": "bash"
  }
}
```

**输出：**
```json
{
  "type": "ask-user-question",
  "toolUseId": "perm-123",
  "questions": [
    { "id": "allow", "label": "Allow" },
    { "id": "deny", "label": "Deny" }
  ],
  "toolName": "bash"
}
```

**转换逻辑：**
```typescript
case "permission_request": {
  const requestId = data?.requestId || data?.request_id || `perm-${Date.now()}`
  const options = data?.options || []
  const toolName = data?.toolName || data?.tool_name || "tool"

  permissionMap.set(requestId, {
    subChatId: "", // filled by caller
    sessionId: event.sessionId,
  })

  chunks.push({
    type: "ask-user-question",
    toolUseId: requestId,
    questions: options.map((opt: any) => ({
      id: opt.optionId || opt.option_id,
      label: opt.label,
    })),
    toolName,
  })
  break
}
```

---

#### usage_update → message-metadata

**输入：**
```json
{
  "eventType": "usage_update",
  "data": {
    "usage": {
      "input_tokens": 150,
      "output_tokens": 230
    }
  }
}
```

**输出：**
```json
{
  "type": "message-metadata",
  "messageMetadata": {
    "usage": {
      "inputTokens": 150,
      "outputTokens": 230
    }
  }
}
```

**转换逻辑：**
```typescript
case "usage_update": {
  const usage = data?.usage || data
  chunks.push({
    type: "message-metadata",
    messageMetadata: {
      usage: {
        inputTokens: usage?.input_tokens || usage?.inputTokens || 0,
        outputTokens: usage?.output_tokens || usage?.outputTokens || 0,
      },
    },
  })
  break
}
```

---

#### available_commands_update → available-commands

**输入：**
```json
{
  "eventType": "available_commands_update",
  "data": {
    "commands": [
      { "name": "/help", "description": "Show help", "input": "[command]" },
      { "name": "/clear", "description": "Clear chat" }
    ]
  }
}
```

**输出：**
```json
{
  "type": "available-commands",
  "commands": [
    { "name": "/help", "description": "Show help", "input": "[command]" },
    { "name": "/clear", "description": "Clear chat" }
  ]
}
```

**转换逻辑：**
```typescript
case "available_commands_update": {
  const commands: Array<{ name: string; description: string; input?: string }> =
    Array.isArray(data?.commands)
      ? data.commands.map((c: any) => ({
          name: c.name ?? "",
          description: c.description ?? "",
          input: c.input,
        }))
      : []
  chunks.push({
    type: "available-commands",
    commands,
  })
  break
}
```

---

#### closed → finish

**输入：**
```json
{
  "eventType": "closed",
  "data": null
}
```

**输出：**
```json
{
  "type": "finish",
  "finishReason": "stop"
}
```

**转换逻辑：**
```typescript
case "closed": {
  chunks.push({
    type: "finish",
    finishReason: "stop" as const,
  })
  break
}
```

---

## Poll 机制

### 轮询循环

**位置：** `src/main/lib/trpc/routers/agent.ts:350-430`

```typescript
// 每 100ms 轮询一次
const timer = setInterval(async () => {
  // 防止重叠的 poll 迭代
  if (isPolling) return
  isPolling = true

  try {
    const events = pollEvents()  // 从 Rust 获取所有待处理事件

    for (const event of events) {
      if (event.sessionId !== sessionId) continue

      const chunks = translateEvent(event)
      for (const chunk of chunks) {
        // 1ms 延迟避免 overwhelming stream
        await new Promise(resolve => setTimeout(resolve, 1))

        emit.next(chunk)  // 发送给前端

        if (chunk.type === "finish") {
          clearInterval(timer)
          emit.complete()
          return
        }
      }
    }
  } catch (err) {
    console.error("[Agent] Poll error:", err)
    emit.next({ type: "error", errorText: String(err) })
    clearInterval(timer)
    emit.complete()
  } finally {
    isPolling = false
  }
}, 100)  // POLL_INTERVAL = 100ms
```

### 关键配置

```typescript
const POLL_INTERVAL = 100  // 轮询间隔（毫秒）
```

---

## Session 生命周期

### 创建 Session

```typescript
// 1. 创建 Agent Session（通过 tmux 注入或 MCP 连接）
const sessionId = await createSession(agent, cwd)

// 2. 保存到映射表
sessionMap.set(subChatId, sessionId)

// 3. 保存 Session 元数据
saveSessionMeta(sessionId, agent, cwd)
```

### 发送 Prompt

```typescript
// 发送 prompt 到 Agent（通过 tmux send-keys 注入到 agent 的 pane）
await sendPrompt(sessionId, prompt)
```

### 关闭 Session

```typescript
// 1. 停止轮询
clearInterval(timer)

// 2. 清理映射
sessionMap.delete(subChatId)

// 3. 关闭 Agent Session
await closeSession(sessionId)
```

---

## 错误处理

### Poll 错误

```typescript
catch (err) {
  console.error("[Agent] Poll error:", err)
  emit.next({ type: "error", errorText: String(err) })
  clearInterval(timer)
  activePollers.delete(subChatId)
  emit.complete()
}
```

### Session 创建错误

```typescript
catch (err) {
  console.error("[Agent] Failed to start session:", err)
  emit.next({ type: "error", errorText: `Failed to start agent session: ${err}` })
  emit.complete()
  return
}
```

---

## 调试日志

### 后端日志示例

```
[Agent] Chat start: sub=6740ekcu agent=claude cwd=/home/yubing/Public
[Agent] Starting session for agent=claude, cwd=/home/yubing/Public
[Agent] Creating new session for agent=claude
[Agent] Session created: 23189f14-3206-406f-80b3-7aedb85a6433
[Agent] Sending prompt via tmux injection (length=2): 你好...
[Agent] Prompt injected successfully
[Agent] Got 71 events, polling iteration 1, types: available_commands_update, usage_update, agent_thought_chunk, ...
[Agent] Emitted chunk: type=available-commands subChatId=6740ekcu
[Agent] Emitted chunk: type=message-metadata subChatId=6740ekcu
[Agent] Emitted chunk: type=reasoning-delta subChatId=6740ekcu
...
```

### 启用详细日志

在 `src/main/lib/trpc/routers/agent.ts` 中取消注释：

```typescript
console.log(`[Agent] Got ${events.length} events, polling iteration ${pollCount}, types: ${events.map(e => e.eventType).join(", ")}`)
console.log(`[Agent] Emitted chunk: type=${chunk.type} subChatId=${subChatId.slice(-8)}`)
```

---

## 更新日志

- **2026-08-15**: 移除旧协议，改用 tmux 注入（首选）+ MCP 通知（回退）
- **2026-08-09**: 初始版本，基于 Rust 事件层和 TypeScript 转换层创建
- **2026-08-09**: 修复 chunk 格式问题（textDelta → delta，添加 id 字段）

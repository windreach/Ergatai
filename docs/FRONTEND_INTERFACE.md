# 前端接口文档 - Agent Chat Protocol

> 本文档描述前端（React + @ai-sdk/react）期望接收的所有消息格式

## 目录

- [概述](#概述)
- [消息流架构](#消息流架构)
- [Chunk 类型定义](#chunk-类型定义)
  - [文本流式输出](#文本流式输出)
  - [思考过程输出](#思考过程输出)
  - [工具调用](#工具调用)
  - [用户交互](#用户交互)
  - [元数据和状态](#元数据和状态)
  - [错误处理](#错误处理)
- [Subscription 生命周期](#subscription-生命周期)
- [ReadableStream 管理](#readablestream-管理)

---

## 概述

前端使用 `@ai-sdk/react` 的 `useChat` hook 来管理聊天流。所有从后端接收的消息必须符合 `UIMessageChunk` 类型定义。

**关键文件：**
- `src/renderer/features/agents/lib/ipc-chat-transport.ts` - 主要传输层
- `src/renderer/features/agents/main/active-chat.tsx` - Chat 组件
- `node_modules/ai/dist/index.d.ts` - UIMessageChunk 类型定义

---

## 消息流架构

```
后端 (Rust/TypeScript)
    ↓ emit.next(chunk)
tRPC Subscription
    ↓ onData(chunk)
ReadableStream<UIMessageChunk>
    ↓ controller.enqueue(chunk)
useChat hook (@ai-sdk/react)
    ↓ 解析 chunk
React UI 更新
```

---

## Chunk 类型定义

### 文本流式输出

#### text-start
开始一段新的文本内容。

```typescript
{
  type: 'text-start';
  id: string;                    // 必需：文本段唯一标识
  providerMetadata?: ProviderMetadata;  // 可选：提供商元数据
}
```

#### text-delta
流式输出文本增量。

```typescript
{
  type: 'text-delta';
  id: string;                    // 必需：关联的 text-start id
  delta: string;                 // 必需：文本增量内容
  providerMetadata?: ProviderMetadata;
}
```

**示例：**
```json
{
  "type": "text-delta",
  "id": "text-session-123",
  "delta": "这是一段文本"
}
```

#### text-end
结束一段文本内容。

```typescript
{
  type: 'text-end';
  id: string;
  providerMetadata?: ProviderMetadata;
}
```

---

### 思考过程输出

#### reasoning-start
开始输出思考过程（Chain of Thought）。

```typescript
{
  type: 'reasoning-start';
  id: string;
  providerMetadata?: ProviderMetadata;
}
```

#### reasoning-delta
流式输出思考过程增量。

```typescript
{
  type: 'reasoning-delta';
  id: string;                    // 必需：关联的 reasoning-start id
  delta: string;                 // 必需：思考内容增量
  providerMetadata?: ProviderMetadata;
}
```

**示例：**
```json
{
  "type": "reasoning-delta",
  "id": "reasoning-session-123",
  "delta": "让我分析一下这个问题..."
}
```

#### reasoning-end
结束思考过程输出。

```typescript
{
  type: 'reasoning-end';
  id: string;
  providerMetadata?: ProviderMetadata;
}
```

---

### 工具调用

#### tool-input-start
工具调用开始。

```typescript
{
  type: 'tool-input-start';
  toolCallId: string;            // 必需：工具调用唯一标识
  toolName: string;              // 必需：工具名称
  providerExecuted?: boolean;    // 可选：是否由提供商执行
  providerMetadata?: ProviderMetadata;
  dynamic?: boolean;             // 可选：是否为动态工具
  title?: string;                // 可选：工具调用标题
}
```

#### tool-input-delta
工具输入参数增量（用于流式参数）。

```typescript
{
  type: 'tool-input-delta';
  toolCallId: string;
  inputTextDelta: string;        // 参数文本增量
}
```

#### tool-input-available
工具输入参数就绪。

```typescript
{
  type: 'tool-input-available';
  toolCallId: string;
  toolName: string;
  input: unknown;                // 必需：完整的工具输入参数
  providerExecuted?: boolean;
  providerMetadata?: ProviderMetadata;
  dynamic?: boolean;
  title?: string;
}
```

**示例：**
```json
{
  "type": "tool-input-available",
  "toolCallId": "call-123",
  "toolName": "bash",
  "input": {
    "command": "ls -la"
  }
}
```

#### tool-input-error
工具输入参数错误。

```typescript
{
  type: 'tool-input-error';
  toolCallId: string;
  toolName: string;
  input: unknown;
  errorText: string;             // 必需：错误描述
  providerExecuted?: boolean;
  providerMetadata?: ProviderMetadata;
  dynamic?: boolean;
  title?: string;
}
```

#### tool-output-available
工具输出结果就绪。

```typescript
{
  type: 'tool-output-available';
  toolCallId: string;
  output: unknown;               // 必需：工具输出结果
  providerExecuted?: boolean;
  dynamic?: boolean;
  preliminary?: boolean;         // 可选：是否为初步结果
}
```

#### tool-output-error
工具执行错误。

```typescript
{
  type: 'tool-output-error';
  toolCallId: string;
  errorText: string;
  providerExecuted?: boolean;
  dynamic?: boolean;
}
```

#### tool-output-denied
工具执行被拒绝。

```typescript
{
  type: 'tool-output-denied';
  toolCallId: string;
}
```

#### tool-approval-request
请求工具执行批准。

```typescript
{
  type: 'tool-approval-request';
  approvalId: string;            // 批准请求 ID
  toolCallId: string;
}
```

---

### 用户交互

#### ask-user-question
向后端请求用户输入（自定义类型，非 AI SDK 标准）。

```typescript
{
  type: 'ask-user-question';
  toolUseId: string;             // 问题唯一标识
  questions: Array<{
    id: string;                  // 选项 ID
    label: string;               // 选项标签
  }>;
  toolName: string;              // 关联的工具名称
}
```

---

### 元数据和状态

#### start
消息流开始。

```typescript
{
  type: 'start';
  messageId?: string;            // 可选：消息 ID
  messageMetadata?: unknown;     // 可选：消息元数据
}
```

#### start-step
开始一个新的处理步骤。

```typescript
{
  type: 'start-step';
}
```

#### finish-step
结束当前处理步骤。

```typescript
{
  type: 'finish-step';
}
```

#### message-metadata
消息元数据更新。

```typescript
{
  type: 'message-metadata';
  messageMetadata: {             // 必需：元数据对象
    usage?: {
      inputTokens: number;
      outputTokens: number;
    };
    [key: string]: unknown;
  };
}
```

**示例：**
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

#### finish
消息流正常结束。

```typescript
{
  type: 'finish';
  finishReason?: 'stop' | 'length' | 'content-filter' | 'tool-calls' | 'error' | 'other';
  messageMetadata?: unknown;
}
```

**示例：**
```json
{
  "type": "finish",
  "finishReason": "stop"
}
```

#### abort
消息流被中止。

```typescript
{
  type: 'abort';
  reason?: string;               // 中止原因
}
```

#### available-commands
可用命令列表更新（自定义类型）。

```typescript
{
  type: 'available-commands';
  commands: Array<{
    name: string;                // 命令名称
    description: string;         // 命令描述
    input?: string;              // 可选：输入格式说明
  }>;
}
```

---

### 资源引用

#### source-url
URL 资源引用。

```typescript
{
  type: 'source-url';
  sourceId: string;
  url: string;
  title?: string;
  providerMetadata?: ProviderMetadata;
}
```

#### source-document
文档资源引用。

```typescript
{
  type: 'source-document';
  sourceId: string;
  mediaType: string;             // MIME 类型
  title: string;
  filename?: string;
  providerMetadata?: ProviderMetadata;
}
```

#### file
文件资源。

```typescript
{
  type: 'file';
  url: string;                   // 文件 URL
  mediaType: string;             // MIME 类型
  providerMetadata?: ProviderMetadata;
}
```

---

### 错误处理

#### error
流式传输过程中发生错误。

```typescript
{
  type: 'error';
  errorText: string;             // 必需：错误描述
}
```

**示例：**
```json
{
  "type": "error",
  "errorText": "Failed to connect to API"
}
```

---

## Subscription 生命周期

### 创建 Subscription

```typescript
const sub = trpcClient.claude.chat.subscribe({
  subChatId: string,
  chatId: string,
  prompt: string,
  cwd: string,
  projectPath?: string,
  mode: "plan" | "agent",
  sessionId?: string,
  agentName?: string,
  model?: string,
  maxThinkingTokens?: number,
  images?: any[],
  // ... 其他参数
}, {
  onData: (chunk: UIMessageChunk) => { ... },
  onError: (err: Error) => { ... },
  onComplete: () => { ... },
})
```

### 事件回调

- **onData(chunk)**: 每次接收到 chunk 时调用
- **onError(err)**: 发生错误时调用
- **onComplete()**: subscription 正常完成时调用

---

## ReadableStream 管理

### 创建 Stream

```typescript
return new ReadableStream<UIMessageChunk>({
  start: (controller) => {
    const sub = trpcClient.claude.chat.subscribe(..., {
      onData: (chunk) => {
        try {
          controller.enqueue(chunk)
        } catch (e) {
          // Stream 已关闭
          console.error('Failed to enqueue chunk:', e)
        }

        if (chunk.type === "finish") {
          controller.close()
        }
      },
      onComplete: () => {
        controller.close()
      },
      onError: (err) => {
        controller.error(err)
      },
    })
  },
})
```

### 关键注意事项

1. **Chunk 格式必须严格匹配**：字段名和类型必须与 `UIMessageChunk` 定义一致
2. **必需字段不能缺失**：如 `id`、`delta` 等
3. **及时关闭 Stream**：收到 `finish` chunk 后必须调用 `controller.close()`
4. **错误处理**：enqueue 失败时不要中断 subscription

---

## 常见问题

### Q: 为什么 Stream 会意外关闭？

A: 可能原因：
1. Chunk 格式不符合 `UIMessageChunk` 定义
2. `useChat` hook 在 re-render 时重新初始化
3. ReadableStream 被垃圾回收

### Q: 如何调试 Stream 问题？

A: 添加日志追踪：
```typescript
onData: (chunk) => {
  console.log('[DEBUG] Received chunk:', chunk.type, chunk)
  try {
    controller.enqueue(chunk)
    console.log('[DEBUG] Enqueue success')
  } catch (e) {
    console.error('[DEBUG] Enqueue failed:', e)
  }
}
```

---

## 更新日志

- **2026-08-09**: 初始版本，基于 AI SDK 类型定义创建

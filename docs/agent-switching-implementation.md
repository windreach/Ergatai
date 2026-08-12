# Agent 切换功能实现

## ✅ 已完成功能

### 1. **数据获取层** - tRPC API

**文件**: `src/main/lib/trpc/routers/dag.ts`

新增 API:
```typescript
dag.getAgentsStatus() → Array<{
  task_id: string
  agent_name: string
  session_id: string | null  // ACP session ID
  status: string
}>
```

**作用**: 获取所有运行中的 Agent 状态，包含 task_id → session_id 映射

---

### 2. **状态管理层** - useAgents Hook

**文件**: `src/renderer/features/agents/hooks/use-agents.ts`

**功能**:
- 轮询 `dag.getState()` 获取 DAG 节点状态
- 轮询 `dag.getAgentsStatus()` 获取 Agent 的 session_id
- 建立映射: `node.id → task_id → session_id`
- 返回增强的 AgentInfo[]（包含 sessionId 字段）

**返回数据**:
```typescript
{
  agents: AgentInfo[],  // 每个 agent 都有 sessionId
  hasActiveAgents: boolean,
  counts: { ... }
}
```

---

### 3. **UI 层** - AgentsPanel 组件

**文件**: `src/renderer/features/agents/ui/agents-panel.tsx`

**更新**: AgentInfo 类型新增 `sessionId?: string` 字段

---

### 4. **集成层** - 切换逻辑

**文件**: `src/renderer/features/sidebar/agents-subchats-sidebar.tsx`

**实现逻辑**:
```typescript
onSelectAgent(agentId) {
  // 1. 找到对应的 Agent
  const agent = agents.find(a => a.agentId === agentId)
  
  // 2. 获取其 sessionId
  const sessionId = agent.sessionId
  
  // 3. 在所有 SubChats 中找到匹配的
  const targetSubChat = allSubChats.find(
    sc => sc.acpSessionId === sessionId
  )
  
  // 4. 切换到该 SubChat
  setActiveSubChat(targetSubChat.id)
  addToOpenSubChats(targetSubChat.id)
}
```

---

## 🔄 完整数据流

```
┌─────────────────────────────────────────────────────┐
│ 1. Rust DagScheduler 创建 Agent                     │
│    task_id: "n1"                                    │
│    session_id: "acp-session-xyz"                    │
└─────────────────────────────────────────────────────┘
    ↓
┌─────────────────────────────────────────────────────┐
│ 2. task_get_agents_status() NAPI                    │
│    返回: [{ task_id, session_id, status }]          │
└─────────────────────────────────────────────────────┘
    ↓
┌─────────────────────────────────────────────────────┐
│ 3. trpc.dag.getAgentsStatus() tRPC                  │
└─────────────────────────────────────────────────────┘
    ↓
┌─────────────────────────────────────────────────────┐
│ 4. useAgents() Hook                                 │
│    建立映射: node.id → session_id                   │
│    返回: agents[] (每个都有 sessionId)              │
└─────────────────────────────────────────────────────┘
    ↓
┌─────────────────────────────────────────────────────┐
│ 5. AgentsPanel 显示 Agent 列表                      │
│    点击 Agent → onSelectAgent(agentId)              │
└─────────────────────────────────────────────────────┘
    ↓
┌─────────────────────────────────────────────────────┐
│ 6. 切换逻辑                                         │
│    agentId → sessionId → subChatId                  │
│    setActiveSubChat(subChatId)                      │
└─────────────────────────────────────────────────────┘
    ↓
┌─────────────────────────────────────────────────────┐
│ 7. UI 切换到对应 Agent 的 SubChat                   │
└─────────────────────────────────────────────────────┘
```

---

## 📊 关键映射关系

| 层级 | 标识符 | 来源 |
|------|--------|------|
| DAG Node | `node.id` (e.g., "n1") | `dag.getState()` |
| Task | `task_id` | `dag.getAgentsStatus()` |
| ACP Session | `session_id` | `dag.getAgentsStatus()` |
| SubChat | `subChatId` | `allSubChats[]` |
| **关联** | `subChat.acpSessionId === agent.sessionId` | 前端匹配 |

---

## 🎯 用户交互流程

### 场景: 多 Agent 并行任务

1. **用户发送请求**:
   ```
   用 3 个 Agent 并行重构这个模块
   ```

2. **主 Agent 输出 DAG**:
   ```markdown
   ## Task A - agent-a: 分析代码
   ## Task B - agent-b: 写实现
   ## Task C - agent-c: 写测试
   ```

3. **DagDetector 自动提交** → Rust 创建 3 个 ACP Sessions

4. **AgentsPanel 显示**:
   ```
   ▼ Agents (3)
     ✓ 主 Agent              [当前]
     🔄 Agent-A: 分析代码
     ✓ Agent-B: 写实现
     ⏳ Agent-C: 写测试
   ```

5. **用户点击 "Agent-A: 分析代码"**:
   - 查找 agent.sessionId = "acp-session-aaa"
   - 查找 subChat.acpSessionId = "acp-session-aaa"
   - 找到 subChat.id = "subchat-123"
   - 调用 `setActiveSubChat("subchat-123")`

6. **UI 切换** → 显示 Agent-A 的对话界面
   - 可以看到 Agent-A 的实时输出
   - 可以查看它的工具调用（Read, Edit, Bash 等）
   - 可以发送消息给 Agent-A（未来功能）

---

## 🔍 调试技巧

### 1. 查看 Agent 状态

```javascript
// DevTools Console
const { agents } = window.__TRPC__.dag.getAgentsStatus.query()
console.table(agents)
// 应该看到 task_id, session_id, status
```

### 2. 查看 SubChat 映射

```javascript
// 在 React DevTools 中
// 找到 AgentsSubChatsSidebar 组件
// 查看 allSubChats 数组
// 每个 subChat 都有 acpSessionId
```

### 3. 测试切换逻辑

```javascript
// 在 AgentsPanel 的 onSelectAgent 中添加
console.log("Switching:", {
  agentId,
  sessionId: agent.sessionId,
  targetSubChat: targetSubChat?.id,
})
```

### 4. 查看日志

```bash
# 启用详细日志
DEBUG=agents:* bun run dev
```

---

## 🚀 下一步优化

### TODO 1: 实时推送替代轮询

当前使用 2 秒轮询，可以改为 NATS 事件推送：

```typescript
// 监听 ACP session 创建事件
useEffect(() => {
  const unsubscribe = trpc.dag.onAgentCreated.subscribe(
    {},
    {
      onData: (event) => {
        // 立即更新 agents 列表
        refetch()
      }
    }
  )
  return () => unsubscribe()
}, [])
```

### TODO 2: Agent 完成时自动切换

```typescript
// 监听 Agent 完成事件
useEffect(() => {
  if (agent.status === "completed") {
    // 自动切换到下一个 pending 的 Agent
    const nextAgent = agents.find(a => a.status === "pending")
    if (nextAgent) {
      onSelectAgent(nextAgent.agentId)
    }
  }
}, [agent.status])
```

### TODO 3: 多 Agent 对话

允许用户向特定 Agent 发送消息：

```typescript
function sendMessageToAgent(agentId: string, message: string) {
  const agent = agents.find(a => a.agentId === agentId)
  const subChat = findSubChatBySessionId(agent.sessionId)
  
  // 调用 trpc.acp.sendMessage
  trpc.acp.sendMessage.mutate({
    subChatId: subChat.id,
    message,
  })
}
```

### TODO 4: Agent 进度可视化

```typescript
// 在 AgentsPanel 中显示进度条
<ProgressBar
  value={agent.progress}  // 0-100
  status={agent.status}
/>
```

---

## 📚 相关文件

| 文件 | 作用 |
|------|------|
| `src/main/lib/trpc/routers/dag.ts` | 添加 getAgentsStatus API |
| `src/renderer/features/agents/hooks/use-agents.ts` | 数据获取 + 映射 |
| `src/renderer/features/agents/ui/agents-panel.tsx` | UI 组件 |
| `src/renderer/features/sidebar/agents-subchats-sidebar.tsx` | 集成 + 切换逻辑 |
| `src/renderer/features/agents/stores/sub-chat-store.ts` | SubChat 状态管理 |
| `src-rust/src/napi/tasks.rs` | Rust NAPI 绑定 |
| `src-rust/src/cross_agent/agent_launcher.rs` | Agent 启动 + session 管理 |

---

## ✨ 总结

现在 AgentsPanel 已经完整实现了：

1. ✅ **真实数据获取** - 从 Rust 后端获取 DAG 状态
2. ✅ **状态实时更新** - 每 2 秒轮询，智能停止
3. ✅ **Agent 切换** - 点击 Agent 切换到对应 SubChat
4. ✅ **自动展开** - 有活跃 Agent 时自动展开面板

用户现在可以：
- 看到所有子 Agent 的实时状态
- 点击任意 Agent 查看其对话
- 在不同 Agent 之间快速切换
- 监控整个多 Agent 协作过程

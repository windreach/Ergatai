# 多 Agent 协作前端数据流

##  📊 数据获取架构

```
┌─────────────────────────────────────────────────────┐
│ 用户发送消息: "用 3 个 Agent 并行重构"                │
└─────────────────────────────────────────────────────┘
    ↓
┌─────────────────────────────────────────────────────┐
│ 主 Agent 输出 DAG Markdown:                          │
│ ```dag                                               │
│ ## Task A - agent-a: 分析代码                        │
│ ## Task B - agent-b: 写实现                          │
│ ## Task C - agent-c: 写测试                          │
│ ```                                                  │
└─────────────────────────────────────────────────────┘
    ↓
┌─────────────────────────────────────────────────────┐
│ DagDetector 检测 → autoSubmitDag()                  │
│ 调用 trpc.dag.submit({ markdown })                  │
└─────────────────────────────────────────────────────┘
    ↓
┌─────────────────────────────────────────────────────┐
│ Rust DagScheduler                                    │
│ 1. 解析 Markdown → TaskGraph                        │
│ 2. 创建 ACP Sessions                                │
│ 3. 分发任务给子 Agent A/B/C                         │
└─────────────────────────────────────────────────────┘
    ↓
┌─────────────────────────────────────────────────────┐
│ 子 Agent 执行 (通过 ACP)                             │
│ • Agent-A: 分析代码 → Running                       │
│ • Agent-B: 等待依赖 → Pending                       │
│ • Agent-C: 等待依赖 → Pending                       │
└─────────────────────────────────────────────────────┘
    ↓
┌─────────────────────────────────────────────────────┐
│ 前端 useAgents() Hook                                │
│ • 每 2 秒轮询 trpc.dag.getState()                   │
│ • 获取 TaskGraph JSON                               │
│ • 转换为 AgentInfo[] 格式                           │
└─────────────────────────────────────────────────────┘
    ↓
┌─────────────────────────────────────────────────────┐
│ AgentsPanel 组件                                     │
│ • 显示所有子 Agent 及其状态                          │
│ • 实时更新 (每 2 秒)                                 │
│ • 点击可切换查看对应 Agent 的对话                    │
└─────────────────────────────────────────────────────┘
```

##  🔌 API 接口

### 1. `trpc.dag.getState()`

**返回数据结构** (TaskGraph):
```typescript
{
  nodes: [
    {
      id: "n1",                    // Agent ID
      agent: "agent-a",            // Agent 名称
      task: "分析代码",             // 任务描述
      status: "Running",           // 状态
      depends_on: [],              // 依赖的其他节点
      input?: string,              // 输入数据
      output?: string,             // 输出路径
      result_path?: string,        // 完成后的结果路径
      max_retries: 3,              // 最大重试次数
      retry_count: 0,              // 当前重试次数
      priority?: "high",           // 优先级
    },
    // ... 更多节点
  ],
  created_at: "2024-01-01T12:00:00Z",
  description: "并行重构模块"
}
```

**TaskStatus 枚举**:
- `Pending` - 等待执行（依赖未完成）
- `Running` - 正在执行
- `Completed` - 已完成
- `Failed` - 执行失败
- `Skipped` - 跳过

##  🎨 前端组件

### useAgents Hook

**位置**: `src/renderer/features/agents/hooks/use-agents.ts`

**功能**:
- 轮询 `trpc.dag.getState()` 获取最新状态
- 将 TaskGraph 转换为 AgentInfo[] 格式
- 智能轮询：有活跃 Agent 时每 2 秒轮询，否则停止
- 提供便利方法：`hasActiveAgents`, `counts`

**返回数据**:
```typescript
{
  agents: AgentInfo[],           // Agent 列表
  isLoading: boolean,            // 加载中
  error: string | null,          // 错误信息
  refetch: () => void,           // 手动刷新
  hasActiveAgents: boolean,      // 是否有活跃的 Agent
  counts: {
    total: number,
    running: number,
    completed: number,
    failed: number,
    pending: number,
  }
}
```

### AgentsPanel 组件

**位置**: `src/renderer/features/agents/ui/agents-panel.tsx`

**Props**:
```typescript
{
  agents?: AgentInfo[],          // Agent 列表（不传用 mock）
  selectedAgentId?: string,      // 当前选中的 Agent
  onSelectAgent?: (id) => void,  // 选中回调
  defaultExpanded?: boolean,     // 默认展开
}
```

**AgentInfo 类型**:
```typescript
{
  agentId: string,               // Agent ID
  name: string,                  // 显示名称
  status: AgentStatus,           // running|completed|failed|pending
  isMain?: boolean,              // 是否主 Agent
  lastActiveAt?: number,         // 最后活跃时间
}
```

##  🔄 状态映射

| Rust TaskStatus | Frontend AgentStatus | 图标 | 颜色 |
|----------------|---------------------|------|------|
| Pending | pending | Clock | 灰色 |
| Running | running | Loader2 (spin) | 前景色 |
| Completed | completed | Check | 绿色 |
| Failed | failed | X | 红色 |
| Skipped | completed | Check | 绿色 |

##  📍 集成位置

**中栏**: `src/renderer/features/sidebar/agents-subchats-sidebar.tsx`

```typescript
// 1. 导入 hook
import { useAgents } from "../agents/hooks/use-agents"

// 2. 在组件中使用
const { agents, hasActiveAgents, counts } = useAgents()

// 3. 传递给 AgentsPanel
<AgentsPanel
  agents={agents}
  defaultExpanded={hasActiveAgents}
  onSelectAgent={(id) => { /* 切换逻辑 */ }}
/>
```

##  🚀 下一步优化

### TODO: Agent 切换逻辑

点击 Agent 时应该切换到对应的 SubChat：

```typescript
onSelectAgent={(agentId) => {
  // 1. 根据 agentId 找到对应的 SubChat
  const subChat = findSubChatByAgentId(agentId)
  
  // 2. 切换到该 SubChat
  if (subChat) {
    setActiveSubChat(subChat.id)
  }
}}
```

### TODO: 实时推送替代轮询

当前使用轮询，可以改为 NATS 事件推送：

```typescript
// 监听 NATS 事件
useEffect(() => {
  const unsubscribe = trpc.dag.onNodeStatusChange.subscribe(
    { dagId },
    {
      onData: (event) => {
        // 实时更新 Agent 状态
        updateAgentStatus(event.nodeId, event.status)
      }
    }
  )
  return () => unsubscribe()
}, [])
```

##  🧪 测试流程

1. **启动开发服务器**:
   ```bash
   bun run dev
   ```

2. **创建新 Chat**:
   - 点击 "New Chat"
   - 选择项目文件夹

3. **发送多 Agent 请求**:
   ```
   用 3 个 Agent 并行重构这个模块:
   - Agent-A: 分析现有代码结构
   - Agent-B: 实现新功能
   - Agent-C: 编写单元测试
   ```

4. **观察 AgentsPanel**:
   - 面板应该自动展开
   - 显示 3 个子 Agent
   - 状态实时更新（Pending → Running → Completed）

5. **验证数据**:
   ```javascript
   // 在 DevTools Console
   // 检查 DAG 状态
   window.__TRPC__.dag.getState.query()
   ```

##  🔍 调试技巧

### 查看网络请求

```javascript
// DevTools Network Tab
// Filter: trpc.dag.getState
// 查看请求频率和返回数据
```

### 查看 Rust 日志

```bash
# 启用详细日志
RUST_LOG=debug bun run dev
```

### 手动测试 Hook

```typescript
// 在 React DevTools 中
// 找到 AgentsSubChatsSidebar 组件
// 查看 useAgents() 返回值
```

##  📚 相关文件

| 文件 | 作用 |
|------|------|
| `src/renderer/features/agents/hooks/use-agents.ts` | 数据获取 Hook |
| `src/renderer/features/agents/ui/agents-panel.tsx` | UI 组件 |
| `src/renderer/features/sidebar/agents-subchats-sidebar.tsx` | 集成位置 |
| `src/main/lib/trpc/routers/dag.ts` | tRPC Router |
| `src-rust/src/napi/dag.rs` | Rust NAPI 绑定 |
| `src-rust/src/cross_agent/dag_scheduler.rs` | DAG 调度器 |
| `src-rust/src/orchestration/dag_topology.rs` | TaskGraph 数据结构 |

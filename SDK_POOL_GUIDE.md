# SDK Pool Manager 使用指南

## 概述

`SdkPoolManager` 是基于官方 ACP SDK 的 Agent 并发池管理器，提供：
- ✅ 多个 Agent 实例并行处理任务
- ✅ 任务队列（当所有 agent 忙时排队等待）
- ✅ 负载均衡（自动分配给空闲 agent）
- ✅ 高可用设计（超时控制、错误处理、事件通知）

## API 参考

### 1. 创建 Pool

```typescript
// 创建一个包含 3 个 agent 实例的并发池
await acp_pool_create("claude-code", 3);
```

**参数**：
- `agent_name`: Agent 配置名称（需要在 ~/.config/ergatai/agents/ 中有对应配置）
- `pool_size`: 并发 agent 数量

**返回**：`Promise<void>`

### 2. 提交任务

```typescript
// 提交任务到 pool
const taskId = await acp_pool_submit_task(
  "claude-code",
  "实现用户登录功能",
  "/path/to/project"
);
console.log(`Task submitted: ${taskId}`);
```

**参数**：
- `agent_name`: Pool 名称
- `prompt`: 任务提示
- `cwd`: 工作目录

**返回**：`Promise<string>` - 任务 ID

### 3. 查询 Pool 状态

```typescript
const status = await acp_pool_status("claude-code");
console.log(`Pool size: ${status.pool_size}`);
console.log(`Idle agents: ${status.idle_agents}`);
console.log(`Busy agents: ${status.busy_agents}`);
console.log(`Pending tasks: ${status.pending_tasks}`);
```

**返回**：`Promise<PoolStatus>`
```typescript
interface PoolStatus {
  agent_name: string;
  pool_size: number;
  idle_agents: number;
  busy_agents: number;
  pending_tasks: number;
  in_flight_tasks: number;
}
```

### 4. 取消任务

```typescript
await acp_pool_cancel_task("claude-code", taskId);
```

**参数**：
- `agent_name`: Pool 名称
- `task_id`: 任务 ID

**返回**：`Promise<void>`

### 5. 列出所有 Pools

```typescript
const pools = await acp_pool_list();
pools.forEach(pool => {
  console.log(`${pool.agent_name}: ${pool.status} (size: ${pool.pool_size})`);
});
```

**返回**：`Promise<PoolInfo[]>`

### 6. 关闭 Pool

```typescript
await acp_pool_shutdown("claude-code");
```

**返回**：`Promise<void>`

## 完整示例

```typescript
import { 
  acp_pool_create, 
  acp_pool_submit_task, 
  acp_pool_status,
  acp_pool_shutdown 
} from './native-binding';

async function main() {
  // 1. 创建包含 3 个 agent 的 pool
  console.log("Creating pool...");
  await acp_pool_create("claude-code", 3);

  // 2. 提交多个任务
  console.log("Submitting tasks...");
  const task1 = await acp_pool_submit_task(
    "claude-code",
    "实现用户登录功能",
    "/projects/myapp"
  );
  const task2 = await acp_pool_submit_task(
    "claude-code",
    "编写单元测试",
    "/projects/myapp"
  );
  const task3 = await acp_pool_submit_task(
    "claude-code",
    "优化数据库查询",
    "/projects/myapp"
  );

  console.log(`Tasks: ${task1}, ${task2}, ${task3}`);

  // 3. 监控状态
  const checkStatus = async () => {
    const status = await acp_pool_status("claude-code");
    console.log(`Status: ${status.busy_agents} busy, ${status.idle_agents} idle, ${status.pending_tasks} pending`);
    return status;
  };

  // 每 5 秒检查一次状态
  const interval = setInterval(checkStatus, 5000);

  // 4. 等待所有任务完成（简化示例）
  await new Promise(resolve => setTimeout(resolve, 60000));

  // 5. 关闭 pool
  clearInterval(interval);
  await acp_pool_shutdown("claude-code");
  console.log("Pool shutdown complete");
}

main().catch(console.error);
```

## 事件监听

Pool 会发送以下事件到前端（通过 `acp_poll_events`）：

### task_dispatched
任务被分配给 agent
```json
{
  "session_id": "xxx",
  "event_type": "task_dispatched",
  "data": {
    "task_id": "pool-task-1",
    "agent_index": 0,
    "prompt_preview": "实现用户登录功能"
  }
}
```

### task_completed
任务完成
```json
{
  "session_id": "xxx",
  "event_type": "task_completed",
  "data": {
    "task_id": "pool-task-1"
  }
}
```

### task_failed
任务失败
```json
{
  "session_id": "xxx",
  "event_type": "task_failed",
  "data": {
    "task_id": "pool-task-1",
    "error": "Task timed out"
  }
}
```

## 架构说明

```
┌─────────────────────────────────────┐
│      SdkPoolManager (Singleton)     │
│  ┌──────────────────────────────┐  │
│  │  pools: HashMap<name, Pool>  │  │
│  └──────────────────────────────┘  │
└─────────────────────────────────────┘
                ↓
┌─────────────────────────────────────┐
│         PoolHandle                  │
│  ┌────────────────────────────┐    │
│  │  agents: Vec<PoolAgent>    │    │  ← N 个 SDK session
│  │  task_queue: VecDeque      │    │  ← 任务队列
│  │  event_loop (async)        │    │  ← 事件循环
│  └────────────────────────────┘    │
└─────────────────────────────────────┘
                ↓
┌─────────────────────────────────────┐
│         PoolAgent                   │
│  ┌────────────────────────────┐    │
│  │  session_id: String        │    │
│  │  cmd_tx: Sender            │    │  ← SDK session 通道
│  │  busy: bool                │    │
│  │  current_task_id: Option   │    │
│  └────────────────────────────┘    │
└─────────────────────────────────────┘
                ↓
┌─────────────────────────────────────┐
│      SDK Session (官方 ACP)         │
│  - Client.builder().connect_with() │
│  - Session management              │
│  - Prompt/Response                 │
│  - Permission handling             │
└─────────────────────────────────────┘
```

## 与旧实现的对比

| 特性 | 旧实现 (buzz) | 新实现 (SDK) |
|------|--------------|-------------|
| ACP 协议 | 自制 AcpClient | 官方 SDK ✅ |
| 代码量 | 762 KB | 17 KB ✅ |
| Agent Pool | ✅ | ✅ |
| 任务队列 | ✅ | ✅ |
| 负载均衡 | ✅ | ✅ |
| 超时控制 | ✅ | ✅ |
| 错误处理 | ✅ | ✅ |
| 事件通知 | ✅ | ✅ |
| 维护成本 | 高（依赖 buzz） | 低（官方维护）✅ |

## 故障排查

### Pool 创建失败
- 检查 agent 配置是否存在：`~/.config/ergatai/agents/{name}.json`
- 检查 agent 命令是否可执行
- 查看日志：`tracing::error!` 输出

### 任务超时
- 默认超时：2 小时（`PROMPT_MAX_DURATION`）
- 可调整：修改 `sdk_pool_manager.rs` 中的常量

### Agent 崩溃
- Pool 会自动检测并记录错误
- 任务会标记为 `task_failed`
- 可以重新提交任务

## 性能建议

1. **Pool 大小**：根据 CPU 核心数和任务复杂度调整
   - 轻量任务：3-5 个 agent
   - 重度任务：1-2 个 agent

2. **任务粒度**：将大任务拆分为多个小任务，提高并发效率

3. **资源监控**：定期检查 `acp_pool_status`，避免过度占用资源

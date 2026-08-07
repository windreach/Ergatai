# ACP 跨 Agent 对话系统

## 🎯 概述

基于 ACP 协议实现的跨 Agent 对话系统，让多个 AI Agent 能够通过 Desktop 中介进行直接对话和协作。

## 🏗️ 架构设计

### 核心理念

虽然 ACP 是 Client-Agent 协议（类似 HTTP），但我们通过 Desktop 作为智能中介，让 Agent 感觉在进行直接对话：

```
Agent A: "@codex 请审查这段代码"
    ↓ (ACP)
Desktop: 识别意图，转发给 Codex
    ↓ (ACP)
Agent B (Codex): "好的，我发现了一个问题..."
    ↓ (ACP)
Desktop: 转发回 Agent A
    ↓ (ACP)
Agent A: "谢谢！我来修复"
```

### 系统组件

```
┌─────────────────────────────────────────┐
│  Frontend (Electron)                    │
│  - 发送跨 Agent 消息                    │
│  - 查看对话历史                         │
└──────────────┬──────────────────────────┘
               │ NAPI
┌──────────────▼──────────────────────────┐
│  Cross-Agent Manager (Rust)             │
│  - 管理对话会话                         │
│  - 消息路由                             │
│  - 意图检测                             │
└──────────────┬──────────────────────────┘
               │
┌──────────────▼──────────────────────────┐
│  ACP Bridge                             │
│  - 连接 ACP Session                     │
│  - 转发消息到 Agent                     │
│  - 接收 Agent 响应                      │
└──────────────┬──────────────────────────┘
               │ ACP
        ┌──────┴──────┬──────┐
        ↓             ↓      ↓
    Agent A       Agent B  Agent C
```

## 📦 核心模块

### 1. Cross-Agent Manager (`cross_agent/mod.rs`)

管理所有跨 Agent 对话的核心组件：

```rust
pub struct CrossAgentManager {
    // 活跃的对话
    conversations: HashMap<String, Conversation>,
    // Agent 消息队列
    agent_queues: HashMap<String, mpsc::Sender<AgentMessage>>,
}
```

**主要功能**：
- 注册/注销 Agent
- 创建和管理对话
- 消息路由和转发
- 对话历史存储

### 2. ACP Bridge (`cross_agent/acp_bridge.rs`)

连接 ACP Session 和跨 Agent 通信：

```rust
pub struct AcpCrossAgentBridge {
    agent_id: String,
    session_id: String,
    message_rx: mpsc::Receiver<AgentMessage>,
}
```

**主要功能**：
- 监听跨 Agent 消息
- 转发消息到 ACP Session
- 检测消息中的跨 Agent 意图

### 3. 意图检测

自动识别消息中是否包含跨 Agent 通信意图：

```rust
// 支持的格式
"@codex please review this"     // @mention
"send to claude: hello"         // send to
"ask opencode to help"          // ask to
```

## 🔌 API 接口

### NAPI 接口（前端调用）

```typescript
// 发送跨 Agent 消息
const conversationId = await ergatai.crossAgentSendMessage(
  "agent-a",      // 发送者
  "agent-b",      // 接收者
  "请审查这段代码"  // 消息内容
);

// 获取对话历史
const conversation = await ergatai.crossAgentGetConversation(conversationId);

// 列出所有活跃对话
const conversations = await ergatai.crossAgentListConversations();

// 检测消息中的跨 Agent 意图
const targetAgent = await ergatai.crossAgentDetectIntent(
  "@codex 请帮我看看这个 bug"
);
// 返回: "codex"
```

### Rust API（内部使用）

```rust
use crate::cross_agent::{cross_agent_manager, AcpCrossAgentBridge};

// 发送消息
let conv_id = cross_agent_manager()
    .send_message("agent-a".to_string(), "agent-b".to_string(), "hello".to_string())
    .await?;

// 创建 ACP Bridge
let mut bridge = AcpCrossAgentBridge::new(
    "agent-a".to_string(),
    session_id.to_string(),
).await;

// 启动消息转发
bridge.start_message_forwarding(cmd_tx).await;

// 发送消息给其他 Agent
bridge.send_to_agent("agent-b".to_string(), "请帮助我".to_string()).await?;
```

## 💡 使用场景

### 场景 1：代码审查协作

```
用户 → Agent A (Claude): "帮我写一个函数，然后让 Codex 审查"

Agent A: 
  1. 编写函数代码
  2. 发送 "@codex 请审查这段代码：[代码]"
  
Desktop:
  1. 检测到 @codex
  2. 转发给 Codex Agent

Agent B (Codex):
  1. 接收代码审查请求
  2. 分析代码
  3. 返回 "发现以下问题：..."
  
Desktop:
  1. 转发回 Agent A

Agent A:
  1. 根据审查结果修改代码
  2. 返回给用户
```

### 场景 2：多 Agent 协作开发

```
用户 → PM Agent: "开发一个用户登录功能"

PM Agent:
  1. 分析需求
  2. 发送 "@dev 请实现后端 API"
  3. 发送 "@frontend 请实现登录界面"

Desktop: 分别转发给 Dev 和 Frontend Agent

Dev Agent:
  1. 实现 API
  2. 返回 "@pm API 已完成，端点：/api/login"

Frontend Agent:
  1. 实现界面
  2. 返回 "@pm 登录界面已完成"

PM Agent:
  1. 整合结果
  2. 返回给用户
```

### 场景 3：知识共享

```
Agent A: "@research 这个算法的时间复杂度是多少？"

Desktop: 转发给 Research Agent

Research Agent:
  1. 分析算法
  2. 返回 "时间复杂度是 O(n log n)，因为..."

Desktop: 转发回 Agent A

Agent A:
  1. 根据复杂度信息优化代码
```

## 🚀 实现步骤

### Phase 1: 基础框架 ✅

- [x] Cross-Agent Manager 实现
- [x] 消息路由和存储
- [x] ACP Bridge 基础结构
- [x] NAPI 接口暴露

### Phase 2: 意图检测 ✅

- [x] @mention 模式识别
- [x] "send to" 模式识别
- [x] "ask to" 模式识别

### Phase 3: 集成测试（进行中）

- [ ] 创建测试 Agent 配置
- [ ] 测试双 Agent 对话
- [ ] 测试多 Agent 协作
- [ ] 验证消息转发正确性

### Phase 4: 前端集成（待开始）

- [ ] 添加跨 Agent 对话 UI
- [ ] 显示对话历史
- [ ] 实时消息更新
- [ ] 对话管理界面

### Phase 5: 高级功能（未来）

- [ ] 对话模板（PM→Dev→QA）
- [ ] 自动任务分配
- [ ] 结果聚合和冲突解决
- [ ] 对话持久化

## 🔧 配置示例

### Agent 配置（支持跨 Agent 通信）

```json
{
  "name": "claude-code",
  "command": "claude-agent-acp",
  "args": [],
  "env": {
    "ANTHROPIC_API_KEY": "xxx"
  },
  "cross_agent": {
    "enabled": true,
    "can_send_to": ["codex", "opencode"],
    "can_receive_from": ["codex", "opencode"]
  }
}
```

## 📊 性能考虑

### 消息延迟

```
Agent A → Desktop → Agent B → Desktop → Agent A

延迟组成：
- ACP 消息传输: ~10-50ms
- Desktop 处理: ~1-5ms
- 总延迟: ~20-110ms

对于对话场景，这个延迟是可接受的。
```

### 内存占用

```
每个对话：
- 消息历史: ~1KB/消息
- 元数据: ~200 bytes
- 总计: ~1.2KB/消息

100 个活跃对话，每个 10 条消息：~1.2MB
```

## 🐛 已知限制

1. **不是真正的 P2P**: 所有消息都经过 Desktop 中转
2. **延迟**: 比直接 P2P 通信延迟高
3. **上下文丢失**: Agent 可能不理解完整的对话上下文
4. **意图检测**: 基于简单模式匹配，可能误判

## 🔮 未来改进

1. **更智能的意图检测**: 使用 LLM 理解消息意图
2. **上下文管理**: 自动注入对话历史到 Agent 上下文
3. **对话模板**: 预定义的协作模式
4. **可视化**: 实时显示 Agent 间的消息流
5. **性能优化**: 批量消息处理，减少延迟

## 📝 总结

虽然 ACP 协议本身不支持 Agent 间的直接通信，但通过在 Desktop 层实现智能中介，我们成功构建了一个可用的跨 Agent 对话系统。

**优势**：
- ✅ 不需要修改现有 Agent
- ✅ 利用现有的 ACP 基础设施
- ✅ 灵活的意图检测和路由
- ✅ 完整的对话历史追踪

**局限**：
- ⚠️ 不是真正的 P2P 通信
- ⚠️ 有一定的延迟
- ⚠️ 依赖意图检测的准确性

这是一个务实的解决方案，在现有技术和协议限制下，实现了多 Agent 协作的目标。

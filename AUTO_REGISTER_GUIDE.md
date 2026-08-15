# Ergatai 多 Agent 协作 - 自动注册方案

## 🎯 解决的问题

**之前的逻辑问题：**
```
❌ Agent 在 tmux 中运行，但没连接到 Ergatai
❌ Ergatai 不知道这些 agent 存在
❌ 无法路由消息给它们
```

**现在的解决方案：**
```
✅ Agent 在 tmux 中运行
✅ Ergatai 自动扫描 tmux，发现所有 pane
✅ 自动注册为 "agent"
✅ 可以互相发送消息
```

## 🚀 快速开始

### 方式 1: 一键启动（推荐）

```bash
./test-opencode-auto-register.sh
```

这个脚本会：
1. ✅ 启动 Ergatai
2. ✅ 创建 tmux session
3. ✅ 启动 3 个 OpenCode 实例
4. ✅ **自动扫描并注册所有 agent**
5. ✅ 显示已注册的 agent 列表

### 方式 2: 手动扫描

如果已经启动了 tmux session，可以手动扫描：

```bash
cargo run --bin scan-tmux-agents
```

## 📊 工作原理

```
┌─────────────────────────────────────────────┐
│  tmux session: ergatai-opencode             │
│  ┌─────────┐ ┌─────────┐ ┌─────────┐      │
│  │OpenCode │ │OpenCode │ │OpenCode │      │
│  │   1     │ │   2     │ │   3     │      │
│  │ pane 0  │ │ pane 1  │ │ pane 2  │      │
│  └─────────┘ └─────────┘ └─────────┘      │
└─────────────────────────────────────────────┘
              ↓ 扫描
┌─────────────────────────────────────────────┐
│  TmuxManager.scan_and_register_panes()      │
│  - 调用 tmux list-panes                     │
│  - 发现 3 个 pane                           │
│  - 注册为 agent:                            │
│    • opencode@0                             │
│    • opencode@1                             │
│    • opencode@2                             │
└─────────────────────────────────────────────┘
              ↓
┌─────────────────────────────────────────────┐
│  Ergatai 知道所有 agent                      │
│  - agent 可以互相发送消息                    │
│  - 通过 tmux 注入传递                        │
└─────────────────────────────────────────────┘
```

## 📨 测试消息传递

### 步骤 1: 启动环境

```bash
./test-opencode-auto-register.sh
```

### 步骤 2: 查看已注册的 agent

```bash
# 扫描并显示
cargo run --bin scan-tmux-agents
```

输出示例：
```
🔍 扫描 tmux session 中的 agent
================================

✅ 发现并注册了 3 个 agent

3. 已注册的 agent:
   - opencode@0 (pane: ergatai-opencode:0.0, command: opencode)
   - opencode@1 (pane: ergatai-opencode:0.1, command: opencode)
   - opencode@2 (pane: ergatai-opencode:0.2, command: opencode)
```

### 步骤 3: 注入消息

```bash
# 向 OpenCode 2 发送消息
./test-inject.sh 2 "请帮我写一个 hello world"

# 向 OpenCode 3 发送消息
./test-inject.sh 3 "什么是 Rust?"
```

### 步骤 4: 观察结果

```bash
# 连接到 tmux 查看
tmux attach -t ergatai-opencode

# 切换 pane 查看 agent 的响应
Ctrl+B 然后按方向键
```

## 🔧 MCP 工具集成

当 agent 连接到 Ergatai（通过 MCP）后，可以调用工具：

### `list_agents` - 列出所有 agent

```json
{
  "name": "list_agents",
  "arguments": {}
}
```

返回：
```
当前有 3 个 agent 连接：
agent_id         status
opencode@0       active
opencode@1       active
opencode@2       active
```

### `send_message` - 发送消息

```json
{
  "name": "send_message",
  "arguments": {
    "target_agent_id": "opencode@1",
    "message": "请帮我 review 这段代码",
    "message_type": "request"
  }
}
```

Ergatai 会：
1. 查找 `opencode@1` 在 tmux 中的位置
2. 通过 `tmux send-keys` 注入消息
3. OpenCode 2 收到消息（像用户输入一样）

## 🎯 关键改进

### 之前的问题

```
Agent 在 tmux 中运行
    ↓
没有连接到 Ergatai
    ↓
Ergatai 不知道它们存在
    ↓
❌ 无法路由消息
```

### 现在的方案

```
Agent 在 tmux 中运行
    ↓
TmuxManager 扫描 tmux
    ↓
自动注册到 TmuxManager
    ↓
✅ Ergatai 知道所有 agent
    ↓
✅ 可以路由消息
```

## 💡 技术细节

### TmuxManager 新增方法

```rust
/// 扫描 tmux session 中的所有 pane，注册为 agent
pub async fn scan_and_register_panes(&self) -> Result<Vec<String>>

/// 检查 agent 是否在 tmux 中
pub async fn is_agent_in_tmux(&self, agent_id: &str) -> bool
```

### 扫描原理

```bash
# tmux 命令
tmux list-panes -t ergatai -F "#{pane_id}:#{pane_current_command}:#{pane_pid}"

# 输出示例
%0:opencode:123456
%1:opencode:123457
%2:opencode:123458

# 解析后注册为
- opencode@0 (pane %0)
- opencode@1 (pane %1)
- opencode@2 (pane %2)
```

## 🧪 测试场景

### 场景 1: 自动发现

```bash
# 1. 启动 3 个 agent
./test-opencode-auto-register.sh

# 2. 自动扫描并注册
# （脚本会自动执行）

# 3. 查看注册的 agent
cargo run --bin scan-tmux-agents
```

### 场景 2: 动态添加

```bash
# 1. 在 tmux 中添加新 pane
tmux attach -t ergatai-opencode
# Ctrl+B, % (分割窗口)
# 启动新 agent

# 2. 重新扫描
cargo run --bin scan-tmux-agents

# 3. 新 agent 已注册
```

### 场景 3: 消息路由

```bash
# Agent A 调用 MCP 工具
send_message(to="opencode@1", message="Hello")

# Ergatai 处理：
1. 查找 opencode@1 在 TmuxManager 中
2. 找到 pane ergatai-opencode:0.1
3. 调用 tmux send-keys 注入
4. OpenCode 2 收到消息
```

## 🎉 总结

**核心改进：**
- ✅ 自动扫描 tmux，发现运行中的 agent
- ✅ 无需 agent 主动连接 MCP
- ✅ 简化了多 agent 协作的启动流程
- ✅ 保留了 tmux 注入的核心机制

**下一步：**
- 集成到 MCP 工具中（自动扫描）
- 支持动态添加/删除 agent
- 实现完整的消息路由流程
- 测试真实的 agent 间协作

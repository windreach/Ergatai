# Ergatai 使用指南

## 快速开始

### 1. 启动 Ergatai 服务器

```bash
cargo run --bin ergatai-api -- --port 3000
```

### 2. 创建 tmux session 并启动 agent

```bash
# 创建 session
tmux new-session -s ergatai -x 200 -y 50

# Pane 1: 启动 Agent A (例如 Claude Code)
# 按 Ctrl+B, % 水平分割
tmux split-window -h

# Pane 0: Agent A
claude  # 或其他 agent

# Pane 1: Agent B
# 按 Ctrl+B, 切换到 pane 1
opencode  # 或其他 agent
```

### 3. Agent 间通信

**方式 1: Agent 调用 MCP 工具**

Agent A 通过 MCP 调用 `send_message` 工具：

```json
{
  "name": "send_message",
  "arguments": {
    "target_agent_id": "agent-b",
    "message": "请帮我 review 这段代码",
    "message_type": "request"
  }
}
```

Ergatai 会：
1. 收到 MCP 调用
2. 通过 tmux 注入消息到 Agent B 的 pane
3. Agent B 收到消息（像用户输入一样）

**方式 2: 手动注入（测试用）**

```bash
# 在另一个终端
tmux send-keys -t ergatai:0.1 "来自 Agent A 的消息" Enter
```

### 4. 查看 agent

```bash
# 连接到 tmux session
tmux attach -t ergatai

# 在 pane 间切换
Ctrl+B, 方向键

# 断开（不关闭）
Ctrl+B, D
```

## 完整示例

### 场景：两个 Claude Code 实例协作

```bash
# 终端 1: 启动 Ergatai
cargo run --bin ergatai-api -- --port 3000

# 终端 2: 创建 tmux session
tmux new-session -s collab

# 启动第一个 Claude Code
claude --model opus

# 分割窗口，启动第二个
Ctrl+B, %
claude --model sonnet

# 现在两个 Claude Code 都在运行
# 它们可以通过 MCP 互相对话
```

### Agent 配置 MCP

每个 agent 需要配置连接到 Ergatai 的 MCP：

```json
{
  "mcpServers": {
    "ergatai": {
      "url": "http://localhost:3000/mcp"
    }
  }
}
```

## 工作原理

```
┌─────────────────────────────────────────┐
│  Agent A (Claude Code)                  │
│  - 运行在 tmux pane 0                   │
│  - 通过 MCP 连接到 Ergatai              │
└─────────────────────────────────────────┘
         ↓ MCP: send_message()
┌─────────────────────────────────────────┐
│  Ergatai (Middleware)                    │
│  - 收到 MCP 调用                         │
│  - 调用 TmuxManager.inject_message()    │
└─────────────────────────────────────────┘
         ↓ tmux send-keys
┌─────────────────────────────────────────┐
│  Agent B (OpenCode)                     │
│  - 运行在 tmux pane 1                   │
│  - 收到注入的消息（像用户输入）          │
└─────────────────────────────────────────┘
```

## 优势

✅ **无侵入** - Agent 不需要任何修改
✅ **保留 TUI** - 完整的终端界面体验
✅ **通用性强** - 任何接受键盘输入的 agent 都能用
✅ **简单直接** - 不需要 ACP、HTTP 端点等复杂协议

## 限制

⚠️ **消息看起来像用户输入** - Agent 无法区分
⚠️ **需要手动启动 agent** - 不能自动管理
⚠️ **依赖 tmux** - 必须安装 tmux

## 故障排除

### Agent 没有收到消息

1. 检查 tmux session 是否存在：`tmux ls`
2. 检查 agent 是否在正确的 pane：`tmux list-panes -t ergatai`
3. 查看 Ergatai 日志：`tail -f /tmp/ergatai.log`

### MCP 连接失败

1. 确认 Ergatai 正在运行：`curl http://localhost:3000/health`
2. 检查 agent 的 MCP 配置
3. 查看 agent 的日志

## 下一步

- 集成真实的 agent（Claude Code, OpenCode, Aider 等）
- 实现更智能的消息路由
- 添加 Web dashboard 监控
- 支持 DAG 工作流调度

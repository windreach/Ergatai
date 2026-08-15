# OpenCode 多 Agent 协作测试指南

## 🎯 目标

测试 Ergatai 中间件如何让多个真实的 OpenCode agent 通过 tmux 注入实现互相通信。

## 📋 前置条件

- ✅ tmux 已安装
- ✅ 3 个 OpenCode 启动脚本存在：
  - `/home/yubing/code/start-opencode-1.sh` (HK-05 proxy)
  - `/home/yubing/code/start-opencode-2.sh` (HK-04 proxy)
  - `/home/yubing/code/start-opencode-3.sh` (JP3 proxy)

## 🚀 快速开始

### 步骤 1: 启动测试环境

```bash
cd /home/yubing/code/ergatai
./test-opencode-collaboration.sh
```

这个脚本会：
1. ✅ 启动 Ergatai 服务器（如果未运行）
2. ✅ 创建 tmux session
3. ✅ 在 3 个 pane 中启动 3 个 OpenCode 实例
4. ✅ 显示布局和操作说明

### 步骤 2: 查看 agent

```bash
# 连接到 tmux session
tmux attach -t ergatai-opencode

# 在 pane 间切换
Ctrl+B 然后按方向键

# 退出 tmux（不关闭 agent）
Ctrl+B 然后按 D
```

### 步骤 3: 测试消息注入

**方式 1: 使用辅助脚本**

```bash
# 向 OpenCode 1 发送消息
./test-inject.sh 1 "请帮我写一个 hello world"

# 向 OpenCode 2 发送消息
./test-inject.sh 2 "什么是 Rust?"

# 向 OpenCode 3 发送消息
./test-inject.sh 3 "解释一下 async/await"
```

**方式 2: 直接使用 tmux 命令**

```bash
# 向 pane 0 (OpenCode 1) 发送
tmux send-keys -t ergatai-opencode:0.0 "你好，OpenCode 1!" Enter

# 向 pane 1 (OpenCode 2) 发送
tmux send-keys -t ergatai-opencode:0.1 "你好，OpenCode 2!" Enter

# 向 pane 2 (OpenCode 3) 发送
tmux send-keys -t ergatai-opencode:0.2 "你好，OpenCode 3!" Enter
```

### 步骤 4: 观察结果

```bash
# 连接到 tmux 查看 agent 的响应
tmux attach -t ergatai-opencode

# 切换到对应的 pane，查看 agent 是否处理了消息
Ctrl+B 然后按方向键
```

## 🧪 测试场景

### 场景 1: 简单的消息注入

```bash
# 向 OpenCode 2 发送简单问题
./test-inject.sh 2 "1+1等于几?"

# 观察 OpenCode 2 是否回答
```

**预期结果：** OpenCode 2 应该像收到用户输入一样回答问题。

### 场景 2: Agent 间协作

```bash
# OpenCode 1 请求 OpenCode 2 帮助
./test-inject.sh 1 "请帮我生成一个随机数"

# 等待 OpenCode 1 生成代码后...
# 把结果转发给 OpenCode 2 review
# （需要手动复制粘贴或通过 MCP 工具）
```

### 场景 3: 通过 MCP 工具通信（完整流程）

这需要 OpenCode 配置 MCP 连接到 Ergatai。

**OpenCode 的 MCP 配置：**

在每个 OpenCode 的配置目录中添加 MCP 配置：

```bash
# OpenCode 1
cat > /home/yubing/code/.opencode-instances/1/config.json << 'EOF'
{
  "mcpServers": {
    "ergatai": {
      "url": "http://localhost:3000/mcp"
    }
  }
}
EOF

# OpenCode 2
cat > /home/yubing/code/.opencode-instances/2/config.json << 'EOF'
{
  "mcpServers": {
    "ergatai": {
      "url": "http://localhost:3000/mcp"
    }
  }
}
EOF

# OpenCode 3
cat > /home/yubing/code/.opencode-instances/3/config.json << 'EOF'
{
  "mcpServers": {
    "ergatai": {
      "url": "http://localhost:3000/mcp"
    }
  }
}
EOF
```

然后 OpenCode 就可以调用 MCP 工具：

```json
{
  "name": "send_message",
  "arguments": {
    "target_agent_id": "opencode-2",
    "message": "请 review 这段代码",
    "message_type": "request"
  }
}
```

Ergatai 会通过 tmux 注入消息到 OpenCode 2。

## 🔍 验证要点

### 1. 消息注入是否成功

```bash
# 发送消息
./test-inject.sh 2 "测试消息"

# 查看 pane 内容
tmux capture-pane -t ergatai-opencode:0.1 -p | tail -10
```

**应该看到：** "测试消息" 出现在 OpenCode 2 的输入区域。

### 2. Agent 是否处理了消息

观察 OpenCode 的 TUI：
- ✅ 消息显示在输入区域
- ✅ Agent 开始处理（显示思考过程）
- ✅ Agent 给出响应

### 3. Ergatai 日志

```bash
# 查看 Ergatai 日志
tail -f /tmp/ergatai.log

# 应该看到消息路由的日志
```

## 🛠️ 故障排除

### 问题 1: OpenCode 没有收到消息

**检查：**
```bash
# 1. 确认 tmux session 存在
tmux ls

# 2. 确认 pane 存在
tmux list-panes -t ergatai-opencode

# 3. 手动测试注入
tmux send-keys -t ergatai-opencode:0.1 "手动测试" Enter
```

### 问题 2: Ergatai 启动失败

**检查：**
```bash
# 查看日志
cat /tmp/ergatai.log

# 检查端口是否被占用
lsof -i :3000

# 手动启动
cargo run --bin ergatai-api -- --port 3000
```

### 问题 3: OpenCode 启动失败

**检查：**
```bash
# 检查启动脚本
cat /home/yubing/code/start-opencode-1.sh

# 手动运行
/home/yubing/code/start-opencode-1.sh

# 检查 proxy 是否可用
curl -x http://127.0.0.1:7990 https://api.github.com
```

## 🧹 清理

```bash
# 停止 tmux session
tmux kill-session -t ergatai-opencode

# 停止 Ergatai（如果需要）
pkill -f ergatai-api
```

## 📊 预期结果

如果一切正常，你应该看到：

1. ✅ 3 个 OpenCode 实例在 tmux 中运行
2. ✅ 每个 OpenCode 都有完整的 TUI
3. ✅ 可以通过 tmux send-keys 注入消息
4. ✅ OpenCode 会处理注入的消息（像用户输入一样）
5. ✅ Ergatai 可以路由消息（通过 MCP 工具）

## 🎯 下一步

1. **配置 MCP** - 让 OpenCode 连接到 Ergatai
2. **测试 MCP 工具** - OpenCode 调用 send_message
3. **实现完整流程** - Agent A → Ergatai → Agent B
4. **测试多轮对话** - Agent 间来回通信

## 💡 关键洞察

这个测试证明了：

- ✅ **Tmux 注入方案可行** - 可以向运行中的 agent 注入消息
- ✅ **不需要 ACP** - 直接用 tmux send-keys 就够了
- ✅ **保留 TUI** - Agent 的原生界面完全可用
- ✅ **通用性强** - 任何接受键盘输入的 agent 都能用

这就是 Ergatai 的核心技术基础！🚀

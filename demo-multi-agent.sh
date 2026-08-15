#!/bin/bash
# Ergatai 多 Agent 协作演示
# 演示如何通过 tmux 注入实现 agent 间消息传递

set -e

echo "🚀 Ergatai 多 Agent 协作演示"
echo "=============================="
echo ""

# 配置
SESSION_NAME="ergatai-demo"
ERGATAI_PORT=3000

# 清理函数
cleanup() {
    echo ""
    echo "🧹 清理资源..."
    tmux kill-session -t "$SESSION_NAME" 2>/dev/null || true
    echo "✅ 清理完成"
}

# 设置 trap 确保退出时清理
trap cleanup EXIT

# 检查 tmux
if ! command -v tmux &> /dev/null; then
    echo "❌ 错误: tmux 未安装"
    exit 1
fi

# 清理旧的 session
if tmux has-session -t "$SESSION_NAME" 2>/dev/null; then
    echo "⚠️  发现旧 session，正在清理..."
    tmux kill-session -t "$SESSION_NAME"
    sleep 1
fi

echo "📋 步骤 1: 启动 Ergatai 服务器"
echo "-------------------------------"
# 在后台启动 Ergatai
cargo run --bin ergatai-api -- --port $ERGATAI_PORT > /tmp/ergatai.log 2>&1 &
ERGATAI_PID=$!
echo "✅ Ergatai 启动中 (PID: $ERGATAI_PID)"
echo "   日志: /tmp/ergatai.log"
echo ""

# 等待 Ergatai 启动
echo "⏳ 等待 Ergatai 启动..."
for i in {1..10}; do
    if curl -s http://localhost:$ERGATAI_PORT/health > /dev/null 2>&1; then
        echo "✅ Ergatai 已就绪"
        break
    fi
    if [ $i -eq 10 ]; then
        echo "❌ Ergatai 启动超时"
        exit 1
    fi
    sleep 1
done
echo ""

echo "📋 步骤 2: 创建 tmux session"
echo "-----------------------------"
tmux new-session -d -s "$SESSION_NAME" -x 200 -y 50
echo "✅ Session 创建成功: $SESSION_NAME"
echo ""

echo "📋 步骤 3: 启动模拟 Agent"
echo "-------------------------"
# 启动两个简单的 "agent"（用 bash 脚本模拟）
# 实际使用时，这里应该是真正的 agent（如 claude, opencode 等）

# Agent A - 监听 MCP 消息并响应
tmux send-keys -t "$SESSION_NAME:0.0" "bash -c 'echo \"🤖 Agent A 已启动\"; echo \"等待消息...\"; while true; do sleep 1; done'" Enter
sleep 2
echo "✅ Agent A 启动在 pane 0"

# Agent B - 另一个 agent
tmux split-window -h -t "$SESSION_NAME"
tmux send-keys -t "$SESSION_NAME:0.1" "bash -c 'echo \"🤖 Agent B 已启动\"; echo \"等待消息...\"; while true; do sleep 1; done'" Enter
sleep 2
echo "✅ Agent B 启动在 pane 1"
echo ""

echo "📋 步骤 4: 查看当前状态"
echo "-----------------------"
echo "Tmux session: $SESSION_NAME"
echo "  Pane 0: Agent A"
echo "  Pane 1: Agent B"
echo ""
echo "查看 agent: tmux attach -t $SESSION_NAME"
echo ""

echo "📋 步骤 5: 测试消息注入"
echo "-----------------------"
echo "模拟 Agent A 向 Agent B 发送消息..."
echo ""

# 使用 tmux send-keys 模拟消息注入
# 在实际场景中，这会通过 MCP 工具调用触发
tmux send-keys -t "$SESSION_NAME:0.1" "echo '📨 收到来自 Agent A 的消息: Hello from Agent A!'" Enter
sleep 2

echo "✅ 消息已注入到 Agent B"
echo ""

echo "📋 步骤 6: 验证结果"
echo "-------------------"
echo "查看 Agent B 的输出："
echo ""
tmux capture-pane -t "$SESSION_NAME:0.1" -p | tail -5
echo ""

echo "🎉 演示完成！"
echo ""
echo "💡 关键点："
echo "  1. Ergatai 作为中间件协调 agent"
echo "  2. Agent 在 tmux pane 中运行，保留原生 TUI"
echo "  3. 消息通过 tmux send-keys 注入（模拟用户输入）"
echo "  4. Agent 不需要特殊实现，只要能接收键盘输入即可"
echo ""
echo "🔧 下一步："
echo "  - 用真实的 agent（claude, opencode）替换模拟脚本"
echo "  - Agent 通过 MCP 调用 send_message 工具"
echo "  - Ergatai 自动通过 tmux 注入消息"
echo ""
echo "按 Ctrl+C 退出（会自动清理资源）"
echo ""

# 保持运行直到用户中断
wait $ERGATAI_PID

#!/bin/bash
# Ergatai PoC - 终端复用器消息注入验证
# 验证：通过 tmux 向运行中的 agent 注入消息

set -e

# 配置
SESSION_NAME="ergatai-poc"
AGENT_COMMAND="${1:-claude}"  # 默认使用 claude，可以通过参数指定其他 agent

echo "🚀 Ergatai PoC - 终端复用器消息注入测试"
echo "=========================================="
echo "Agent: $AGENT_COMMAND"
echo "Session: $SESSION_NAME"
echo ""

# 清理旧的 session（如果存在）
if tmux has-session -t "$SESSION_NAME" 2>/dev/null; then
    echo "⚠️  发现旧 session，正在清理..."
    tmux kill-session -t "$SESSION_NAME"
    sleep 1
fi

# 创建新的 tmux session（后台运行）
echo "📦 创建 tmux session..."
tmux new-session -d -s "$SESSION_NAME" -x 200 -y 50

# 启动 agent
echo "🤖 启动 agent: $AGENT_COMMAND"
tmux send-keys -t "$SESSION_NAME:0.0" "$AGENT_COMMAND" Enter

echo ""
echo "✅ PoC 已启动！"
echo ""
echo "📋 操作步骤："
echo "1. 连接到 tmux session 查看 agent: tmux attach -t $SESSION_NAME"
echo "2. 在另一个终端运行注入脚本: ./poc-inject.sh \"你的消息\""
echo "3. 观察 agent 是否接收到注入的消息"
echo ""
echo "💡 提示："
echo "- 按 Ctrl+B 然后按 D 可以断开 tmux（不会关闭 agent）"
echo "- 运行 ./poc-cleanup.sh 清理所有资源"
echo ""

# 等待 agent 启动
echo "⏳ 等待 agent 启动 (5秒)..."
sleep 5

echo ""
echo "🎯 现在你可以："
echo "1. tmux attach -t $SESSION_NAME  (查看 agent)"
echo "2. ./poc-inject.sh \"Hello from Ergatai\"  (注入消息)"
echo ""

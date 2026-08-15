#!/bin/bash
# 消息注入脚本 - 向 tmux 中的 agent 注入消息

set -e

SESSION_NAME="ergatai-poc"
PANE_TARGET="$SESSION_NAME:0.0"
MESSAGE="${1:-Hello from Ergatai!}"

# 检查 session 是否存在
if ! tmux has-session -t "$SESSION_NAME" 2>/dev/null; then
    echo "❌ 错误: tmux session '$SESSION_NAME' 不存在"
    echo "请先运行: ./poc-tmux-injection.sh"
    exit 1
fi

echo "📨 注入消息到 agent..."
echo "消息内容: $MESSAGE"
echo ""

# 注入消息到 agent
# send-keys 会模拟键盘输入
tmux send-keys -t "$PANE_TARGET" "$MESSAGE" Enter

echo "✅ 消息已注入！"
echo ""
echo "💡 提示: 运行 'tmux attach -t $SESSION_NAME' 查看 agent 的响应"
echo ""

# 可选：捕获 pane 内容（用于验证）
if [ "$2" = "--capture" ]; then
    echo "📸 捕获 pane 当前内容:"
    echo "======================"
    tmux capture-pane -t "$PANE_TARGET" -p | tail -20
    echo "======================"
fi

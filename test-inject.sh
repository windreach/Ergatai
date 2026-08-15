#!/bin/bash
# 向指定的 OpenCode 实例注入消息

set -e

SESSION_NAME="ergatai-opencode"
AGENT_NUM="${1:-1}"
MESSAGE="${2:-Hello from Ergatai!}"

# 验证参数
if [[ ! "$AGENT_NUM" =~ ^[123]$ ]]; then
    echo "❌ 错误: Agent 编号必须是 1, 2, 或 3"
    echo "用法: $0 <1|2|3> '消息内容'"
    exit 1
fi

# 检查 session 是否存在
if ! tmux has-session -t "$SESSION_NAME" 2>/dev/null; then
    echo "❌ 错误: tmux session '$SESSION_NAME' 不存在"
    echo "请先运行: ./test-opencode-auto-register.sh"
    exit 1
fi

# 计算 pane 索引（0, 1, 2）
PANE_INDEX=$((AGENT_NUM - 1))
PANE_TARGET="$SESSION_NAME:0.$PANE_INDEX"

echo "📨 向 OpenCode Instance $AGENT_NUM 注入消息"
echo "   Pane: $PANE_TARGET"
echo "   消息: $MESSAGE"
echo ""

# 注入消息
tmux send-keys -t "$PANE_TARGET" "$MESSAGE" Enter

echo "✅ 消息已注入！"
echo ""
echo "💡 查看结果: tmux attach -t $SESSION_NAME"
echo ""
echo "📊 已注册的 agent："
tmux list-panes -t "$SESSION_NAME" -F "   - Pane #{pane_index}: #{pane_current_command} (#{pane_id})"


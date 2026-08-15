#!/bin/bash
# 超简单 tmux 教程 - 无需手动操作

echo "🎯 自动化 tmux 设置（无需手动分割窗口）"
echo "========================================"
echo ""

SESSION_NAME="ergatai-test"

# 清理旧的
tmux kill-session -t "$SESSION_NAME" 2>/dev/null || true

# 一步到位：创建 session 并启动第一个程序
echo "📦 创建 tmux session..."
tmux new-session -d -s "$SESSION_NAME" "echo 'Agent A 已启动'; sleep 100"

# 自动分割窗口
echo "🔀 自动分割窗口..."
tmux split-window -h -t "$SESSION_NAME" "echo 'Agent B 已启动'; sleep 100"

# 自动再分割一个
tmux split-window -v -t "$SESSION_NAME" "echo 'Agent C 已启动'; sleep 100"

echo "✅ 完成！"
echo ""
echo "📺 查看结果："
echo "   tmux attach -t $SESSION_NAME"
echo ""
echo "🎮 在 tmux 中的操作："
echo "   - 切换 pane: Ctrl+B, 然后按方向键"
echo "   - 退出 tmux: Ctrl+B, 然后按 D"
echo "   - 关闭 session: ./poc-cleanup.sh"
echo ""
echo "💡 提示："
echo "   - 现在你有 3 个 pane，每个运行一个程序"
echo "   - 可以用 tmux send-keys 向任意 pane 注入消息"
echo "   - 示例: tmux send-keys -t $SESSION_NAME:0.1 'Hello' Enter"
echo ""

# 显示当前 pane 布局
echo "📐 当前布局："
tmux list-panes -t "$SESSION_NAME" -F "  Pane #{pane_index}: #{pane_current_command}"
echo ""

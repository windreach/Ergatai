#!/bin/bash
# 手动测试 tmux 注入
# 这个脚本模拟 Ergatai 向 tmux pane 注入消息

PANE=%1  # Agent 2 的 pane

echo "📨 向 pane $PANE 注入测试消息"
tmux send-keys -t "$PANE" "Hello! This message was injected by Ergatai via tmux." Enter

echo "✅ 消息已注入！"
echo ""
echo "查看结果："
echo "  tmux attach -t ergatai-opencode"
echo ""
echo "在 tmux 中，切换到 pane 1 查看消息"

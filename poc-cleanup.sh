#!/bin/bash
# 清理脚本 - 关闭 tmux session 和 agent

SESSION_NAME="ergatai-poc"

echo "🧹 清理 Ergatai PoC..."

if tmux has-session -t "$SESSION_NAME" 2>/dev/null; then
    echo "关闭 tmux session: $SESSION_NAME"
    tmux kill-session -t "$SESSION_NAME"
    echo "✅ 清理完成"
else
    echo "ℹ️  Session 不存在，无需清理"
fi

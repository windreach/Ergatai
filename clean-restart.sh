#!/bin/bash
# 完全清理并重启测试环境

echo "🧹 完全清理测试环境"
echo "===================="
echo ""

# 1. 停止 Ergatai
echo "1. 停止 Ergatai..."
pkill -9 -f ergatai-api 2>/dev/null || true
sleep 2
if pgrep -f ergatai-api > /dev/null; then
    echo "❌ 无法停止 Ergatai，尝试强制停止..."
    pkill -9 -f ergatai 2>/dev/null || true
    sleep 1
fi
echo "✅ Ergatai 已停止"
echo ""

# 2. 清理 tmux session
echo "2. 清理 tmux session..."
tmux kill-session -t ergatai-opencode 2>/dev/null || true
tmux kill-session -t ergatai 2>/dev/null || true
tmux kill-session -t ergatai-test 2>/dev/null || true
sleep 1
echo "✅ tmux session 已清理"
echo ""

# 3. 清理日志
echo "3. 清理日志..."
rm -f /tmp/ergatai.log
echo "✅ 日志已清理"
echo ""

# 4. 验证清理
echo "4. 验证清理结果..."
if pgrep -f ergatai-api > /dev/null; then
    echo "❌ 警告: Ergatai 仍在运行"
    exit 1
fi
if tmux has-session -t ergatai-opencode 2>/dev/null; then
    echo "❌ 警告: tmux session 仍存在"
    exit 1
fi
echo "✅ 环境已完全清理"
echo ""

echo "✅ 清理完成！"
echo ""
echo "🚀 现在可以运行："
echo "   ./test-opencode-auto-register.sh"
echo ""
echo "这个脚本会："
echo "  1. 启动新的 Ergatai（干净的 agent 列表）"
echo "  2. 创建新的 tmux session"
echo "  3. 启动 3 个 OpenCode 实例"
echo "  4. 每个 agent 只注册一次"
echo ""

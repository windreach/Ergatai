#!/bin/bash
# 手动映射 MCP agent ID 到 tmux pane
# 用法: ./map-agents.sh

SESSION_NAME="ergatai-opencode"

echo "🔗 映射 MCP Agent 到 tmux Pane"
echo "================================"
echo ""

# 获取 tmux pane 列表
echo "📺 Tmux Panes:"
tmux list-panes -t "$SESSION_NAME" -F "  Pane #{pane_index}: #{pane_current_command} (#{pane_id})"
echo ""

# 获取 MCP agent 列表（通过调用 Ergatai API）
echo "🤖 MCP Agents:"
if curl -s http://localhost:3000/health > /dev/null 2>&1; then
    # 这里需要调用 MCP 工具 list_agents
    # 简化版本：提示用户手动查看
    echo "  请在 agent 中调用: ergatai_list_agents"
    echo "  或者查看 Ergatai 日志: tail -f /tmp/ergatai.log"
else
    echo "  ❌ Ergatai 未运行"
    exit 1
fi
echo ""

echo "💡 映射说明："
echo "  由于 MCP agent ID 是随机生成的（如 opencode@9c15c5e4），"
echo "  我们需要手动建立映射关系。"
echo ""
echo "  临时解决方案："
echo "  1. 使用 ./test-inject.sh 直接注入消息（绕过 MCP）"
echo "  2. 或者等待自动映射功能实现"
echo ""
echo "  示例："
echo "    ./test-inject.sh 2 'Hello from Agent 1'"
echo "    ./test-inject.sh 3 'What is Rust?'"
echo ""

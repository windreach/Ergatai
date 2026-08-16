#!/bin/bash
# 真实 Agent 协作测试
# 使用真实的 LLM agent（如 claude）在 tmux 中运行

set -e

echo "🚀 真实 Agent 协作测试"
echo "======================"
echo ""

# 配置
SESSION_NAME="ergatai-real-test"
ERGATAI_PORT=3000

# 清理函数
cleanup() {
    echo ""
    echo "🧹 清理资源..."
    pkill -f "ergatai-api" 2>/dev/null || true
    tmux kill-session -t "$SESSION_NAME" 2>/dev/null || true
    sleep 1
    echo "✅ 清理完成"
}

trap cleanup EXIT

# 检查依赖
if ! command -v tmux &> /dev/null; then
    echo "❌ 错误: tmux 未安装"
    exit 1
fi

if ! command -v claude &> /dev/null; then
    echo "⚠️  警告: claude 命令未找到"
    echo "   将使用模拟 agent 代替"
    USE_MOCK=true
else
    USE_MOCK=false
fi

# 清理旧 session
if tmux has-session -t "$SESSION_NAME" 2>/dev/null; then
    tmux kill-session -t "$SESSION_NAME"
fi

echo "📋 步骤 1: 启动 Ergatai 服务器"
echo "-------------------------------"
cargo run --bin ergatai-api -- --port $ERGATAI_PORT > /tmp/ergatai-real.log 2>&1 &
ERGATAI_PID=$!
echo "✅ Ergatai 启动中 (PID: $ERGATAI_PID)"

# 等待启动
for i in {1..15}; do
    if curl -s http://localhost:$ERGATAI_PORT/health > /dev/null 2>&1; then
        echo "✅ Ergatai 已就绪"
        break
    fi
    if [ $i -eq 15 ]; then
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

echo "📋 步骤 3: 启动真实 Agent"
echo "-------------------------"

if [ "$USE_MOCK" = true ]; then
    echo "⚠️  使用模拟 agent（claude 未安装）"

    # Agent A - 模拟
    tmux send-keys -t "$SESSION_NAME:0.0" "echo '🤖 Agent A (Mock) 已启动'; echo '等待任务...'; exec bash" Enter
    sleep 2
    echo "✅ Agent A 启动在 pane 0"

    # Agent B - 模拟
    tmux split-window -h -t "$SESSION_NAME"
    tmux send-keys -t "$SESSION_NAME:0.1" "echo '🤖 Agent B (Mock) 已启动'; echo '等待任务...'; exec bash" Enter
    sleep 2
    echo "✅ Agent B 启动在 pane 1"
else
    echo "🚀 启动真实 Claude agent"

    # Agent A - 真实 Claude
    tmux send-keys -t "$SESSION_NAME:0.0" "cd /tmp && claude --dangerously-skip-permissions" Enter
    sleep 3
    echo "✅ Agent A (Claude) 启动在 pane 0"

    # Agent B - 真实 Claude
    tmux split-window -h -t "$SESSION_NAME"
    tmux send-keys -t "$SESSION_NAME:0.1" "cd /tmp && claude --dangerously-skip-permissions" Enter
    sleep 3
    echo "✅ Agent B (Claude) 启动在 pane 1"
fi
echo ""

echo "📋 步骤 4: 查看 tmux session"
echo "----------------------------"
echo "Tmux session: $SESSION_NAME"
echo ""
echo "查看 agent: tmux attach -t $SESSION_NAME"
echo ""

echo "📋 步骤 5: 测试消息注入（通过 Ergatai API）"
echo "-------------------------------------------"

# 获取 agent 列表
echo "获取连接的 agent 列表..."
AGENTS=$(curl -s http://localhost:$ERGATAI_PORT/api/agents | jq -r '.agents[].agent_id' 2>/dev/null || echo "")

if [ -z "$AGENTS" ]; then
    echo "⚠️  没有检测到连接的 agent"
    echo "   （真实 agent 需要通过 MCP 连接到 Ergatai）"
else
    echo "✅ 检测到 agent:"
    echo "$AGENTS" | while read agent; do
        echo "   - $agent"
    done
    echo ""

    # 发送测试消息
    FIRST_AGENT=$(echo "$AGENTS" | head -1)
    echo "向 $FIRST_AGENT 发送测试消息..."

    curl -X POST http://localhost:$ERGATAI_PORT/api/message \
        -H "Content-Type: application/json" \
        -d "{
            \"target_agent_id\": \"$FIRST_AGENT\",
            \"message\": \"请列出当前目录的文件\",
            \"message_type\": \"request\"
        }" | jq .
fi
echo ""

echo "📋 步骤 6: 验证结果"
echo "-------------------"
echo "查看 Agent A 的输出："
tmux capture-pane -t "$SESSION_NAME:0.0" -p | tail -10
echo ""
echo "查看 Agent B 的输出："
tmux capture-pane -t "$SESSION_NAME:0.1" -p | tail -10
echo ""

echo "🎉 测试完成！"
echo ""
echo "💡 真实环境测试要点："
echo "  1. Agent 在 tmux pane 中运行，保留原生 TUI"
echo "  2. Ergatai 通过 tmux send-keys 注入消息"
echo "  3. Agent 执行真实任务（文件操作、代码生成等）"
echo "  4. 可以通过 tmux attach 实时查看 agent 工作"
echo ""
echo "🔧 下一步："
echo "  - 使用真实的 LLM agent（claude, opencode, cursor）"
echo "  - Agent 通过 MCP 连接到 Ergatai"
echo "  - Ergatai 协调多个 agent 协作完成任务"
echo ""
echo "按 Ctrl+C 退出（会自动清理资源）"
echo ""

# 保持运行
wait $ERGATAI_PID

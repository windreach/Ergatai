#!/bin/bash
# Ergatai + OpenCode 多 Agent 协作测试（自动注册版）
# 自动扫描 tmux 并注册 agent

set -e

SESSION_NAME="ergatai-opencode"
ERGATAI_PORT=3000

echo "🚀 Ergatai + OpenCode 多 Agent 协作测试（自动注册版）"
echo "===================================================="
echo ""

# 清理函数
cleanup() {
    echo ""
    echo "💡 提示：tmux session 仍在运行"
    echo "   查看: tmux attach -t $SESSION_NAME"
    echo "   清理: tmux kill-session -t $SESSION_NAME"
}

trap cleanup EXIT

# 检查依赖
if ! command -v tmux &> /dev/null; then
    echo "❌ 错误: tmux 未安装"
    exit 1
fi

# 检查启动脚本
for i in 1 2 3; do
    script="/home/yubing/code/start-opencode-$i.sh"
    if [ ! -f "$script" ]; then
        echo "❌ 错误: 找不到 $script"
        exit 1
    fi
done

echo "✅ 所有启动脚本都存在"
echo ""

# 清理旧的 session
if tmux has-session -t "$SESSION_NAME" 2>/dev/null; then
    echo "⚠️  发现旧 session，正在清理..."
    tmux kill-session -t "$SESSION_NAME"
    sleep 1
fi

echo "📋 步骤 1: 启动 Ergatai 服务器"
echo "-------------------------------"
if curl -s http://localhost:$ERGATAI_PORT/health > /dev/null 2>&1; then
    echo "✅ Ergatai 已在运行 (port $ERGATAI_PORT)"
else
    echo "启动 Ergatai..."
    cargo run --bin ergatai-api -- --port $ERGATAI_PORT > /tmp/ergatai.log 2>&1 &
    ERGATAI_PID=$!
    echo "✅ Ergatai 启动中 (PID: $ERGATAI_PID)"
    echo "   日志: /tmp/ergatai.log"

    echo "⏳ 等待 Ergatai 就绪..."
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
fi
echo ""

echo "📋 步骤 2: 创建 tmux session"
echo "-----------------------------"
tmux new-session -d -s "$SESSION_NAME" -x 200 -y 50
echo "✅ Session 创建成功: $SESSION_NAME"
echo ""

echo "📋 步骤 3: 启动 3 个 OpenCode 实例"
echo "-----------------------------------"

# Pane 0: OpenCode 1
echo "启动 OpenCode Instance 1 (HK-05 proxy)..."
tmux send-keys -t "$SESSION_NAME:0.0" "/home/yubing/code/start-opencode-1.sh" Enter
sleep 3
echo "✅ OpenCode 1 启动在 pane 0"

# 分割窗口（水平）
tmux split-window -h -t "$SESSION_NAME"

# Pane 1: OpenCode 2
echo "启动 OpenCode Instance 2 (HK-04 proxy)..."
tmux send-keys -t "$SESSION_NAME:0.1" "/home/yubing/code/start-opencode-2.sh" Enter
sleep 3
echo "✅ OpenCode 2 启动在 pane 1"

# 分割窗口（垂直）
tmux split-window -v -t "$SESSION_NAME:0.1"

# Pane 2: OpenCode 3
echo "启动 OpenCode Instance 3 (JP3 proxy)..."
tmux send-keys -t "$SESSION_NAME:0.2" "/home/yubing/code/start-opencode-3.sh" Enter
sleep 3
echo "✅ OpenCode 3 启动在 pane 2"
echo ""

echo "📋 步骤 4: 扫描 tmux pane"
echo "-------------------------"
echo "扫描 tmux session 中的 pane，为自动映射做准备..."
cargo run --bin scan-tmux-agents 2>&1 | grep -v "^\s*$" | head -20
echo ""

echo "📋 步骤 5: 显示布局"
echo "-------------------"
tmux list-panes -t "$SESSION_NAME" -F "  Pane #{pane_index}: #{pane_current_command} (PID: #{pane_pid})"
echo ""

echo "🎉 设置完成！"
echo ""
echo "📺 查看 agent："
echo "   tmux attach -t $SESSION_NAME"
echo ""
echo "🎮 在 tmux 中："
echo "   - 切换 pane: Ctrl+B 然后按方向键"
echo "   - 退出 tmux: Ctrl+B 然后按 D"
echo ""
echo "📨 测试消息注入（直接 tmux，不通过 MCP）："
echo "   ./test-inject.sh 1 '消息内容'  # 向 OpenCode 1 发送"
echo "   ./test-inject.sh 2 '消息内容'  # 向 OpenCode 2 发送"
echo "   ./test-inject.sh 3 '消息内容'  # 向 OpenCode 3 发送"
echo ""
echo "🔍 查看已注册的 MCP agent："
echo "   在任一 OpenCode 中调用: ergatai_list_agents"
echo ""
echo "💡 提示："
echo "   - 3 个 OpenCode 已启动，会自动通过 MCP 连接到 Ergatai"
echo "   - tmux pane 已扫描，MCP agent 会自动映射到对应的 pane"
echo "   - 使用 ./test-inject.sh 可以直接注入消息（绕过 MCP）"
echo "   - 或者在 OpenCode 中使用 ergatai_send_message 工具"
echo ""
echo "✅ 脚本执行完成，tmux session 已保留"
echo ""

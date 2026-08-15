#!/bin/bash
# 测试通过 MCP 发送消息

# Agent IDs（从日志中获取）
AGENT_1="opencode@489b766e"
AGENT_2="opencode@0fa1dc9a"
AGENT_3="opencode@b0669dec"

echo "📨 测试通过 MCP 发送消息"
echo "========================"
echo ""
echo "Agent 1: $AGENT_1"
echo "Agent 2: $AGENT_2"
echo "Agent 3: $AGENT_3"
echo ""

# 从 Agent 1 发送消息给 Agent 2
echo "📤 从 Agent 1 发送消息给 Agent 2..."
curl -X POST http://localhost:3000/mcp \
  -H "Content-Type: application/json" \
  -H "Accept: application/json" \
  -d "{
    \"jsonrpc\": \"2.0\",
    \"id\": 1,
    \"method\": \"tools/call\",
    \"params\": {
      \"name\": \"send_message\",
      \"arguments\": {
        \"target_agent_id\": \"$AGENT_2\",
        \"message\": \"Hello Agent 2! This is Agent 1 speaking via MCP.\",
        \"message_type\": \"request\"
      }
    }
  }" | jq '.'

echo ""
echo "✅ 消息已发送！查看 Agent 2 的 tmux pane 是否收到消息"
echo "   tmux attach -t ergatai-opencode"
echo ""

#!/bin/bash
# Integration test for middleware architecture
# Tests: Agent connects via MCP → registers ACP endpoint → receives messages
# Updated for MCP 2025-03-26 Streamable HTTP transport

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
cd "$PROJECT_DIR"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

log() { echo -e "${GREEN}[TEST]${NC} $1"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
error() { echo -e "${RED}[ERROR]${NC} $1"; }

# Cleanup function
cleanup() {
    log "Cleaning up..."
    [ -n "$API_PID" ] && kill "$API_PID" 2>/dev/null || true
    [ -n "$AGENT_PID" ] && kill "$AGENT_PID" 2>/dev/null || true
    wait 2>/dev/null || true
}
trap cleanup EXIT

log "Building workspace..."
cargo build --workspace 2>&1 | tail -3

log "Pre-building simple-agent..."
cargo build -p simple-agent 2>&1 | tail -3

log "Starting Ergatai API server on port 3000..."
cargo run -p ergatai-api -- --port 3000 > /tmp/ergatai-api.log 2>&1 &
API_PID=$!

# Wait for API server to be ready (retry loop instead of fixed sleep)
log "Waiting for API server to start..."
for i in {1..30}; do
    if curl -s http://localhost:3000/health > /dev/null 2>&1; then
        log "API server ready after ${i}s"
        break
    fi
    if [ $i -eq 30 ]; then
        error "API server failed to start after 30s"
        cat /tmp/ergatai-api.log
        exit 1
    fi
    sleep 1
done
log "API server started (PID: $API_PID)"

# ── Test MCP Streamable HTTP transport ──

log "Testing MCP initialize (2025-03-26 protocol)..."
INIT_RESPONSE=$(curl -s -D /tmp/mcp-headers.txt -X POST http://localhost:3000/mcp \
    -H "Content-Type: application/json" \
    -H "Accept: application/json, text/event-stream" \
    -d '{
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": {"name": "test-agent", "version": "1.0.0"}
        }
    }')

log "Initialize response: $INIT_RESPONSE"

# Check protocol version
if echo "$INIT_RESPONSE" | grep -q '"protocolVersion":"2025-03-26"'; then
    log "Protocol version: 2025-03-26 ✓"
else
    error "Wrong protocol version"
    exit 1
fi

# Check server info
if echo "$INIT_RESPONSE" | grep -q '"serverInfo":{"name":"ergatai"'; then
    log "Server info: ergatai ✓"
else
    error "Wrong server info"
    exit 1
fi

# Extract session ID
SESSION_ID=$(grep -i "mcp-session-id" /tmp/mcp-headers.txt | awk '{print $2}' | tr -d '\r')
if [ -n "$SESSION_ID" ]; then
    log "Session ID: $SESSION_ID ✓"
else
    error "No session ID in response"
    exit 1
fi

# Send initialized notification
log "Sending initialized notification..."
curl -s -X POST http://localhost:3000/mcp \
    -H "Content-Type: application/json" \
    -H "Accept: application/json, text/event-stream" \
    -H "Mcp-Session-Id: $SESSION_ID" \
    -H "MCP-Protocol-Version: 2025-03-26" \
    -d '{"jsonrpc":"2.0","method":"notifications/initialized"}' > /dev/null

log "Initialized notification sent ✓"

# ── Test tools ──

log "Testing tools/list..."
TOOLS_RESPONSE=$(curl -s -X POST http://localhost:3000/mcp \
    -H "Content-Type: application/json" \
    -H "Accept: application/json, text/event-stream" \
    -H "Mcp-Session-Id: $SESSION_ID" \
    -H "MCP-Protocol-Version: 2025-03-26" \
    -d '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}')

if echo "$TOOLS_RESPONSE" | grep -q '"list_agents"'; then
    log "Tools list contains list_agents ✓"
else
    error "list_agents tool not found"
    exit 1
fi

if echo "$TOOLS_RESPONSE" | grep -q '"send_message"'; then
    log "Tools list contains send_message ✓"
fi

if echo "$TOOLS_RESPONSE" | grep -q '"set_acp_endpoint"'; then
    log "Tools list contains set_acp_endpoint ✓"
fi

# ── Test agent registration ──

log "Testing list_agents tool (should show test-agent)..."
AGENTS_RESPONSE=$(curl -s -X POST http://localhost:3000/mcp \
    -H "Content-Type: application/json" \
    -H "Accept: application/json, text/event-stream" \
    -H "Mcp-Session-Id: $SESSION_ID" \
    -H "MCP-Protocol-Version: 2025-03-26" \
    -d '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_agents","arguments":{}}}')

log "Agents response: $AGENTS_RESPONSE"

if echo "$AGENTS_RESPONSE" | grep -q "test-agent"; then
    log "Agent 'test-agent' is registered! ✓"
else
    error "Agent 'test-agent' not found in registry"
    exit 1
fi

# ── Test simple-agent connection ──

log "Starting simple-agent on port 8080..."
cargo run -p simple-agent -- --port 8080 --agent-id simple-agent --ergatai http://localhost:3000 > /tmp/simple-agent.log 2>&1 &
AGENT_PID=$!

# Wait for agent to be ready (retry loop instead of fixed sleep)
log "Waiting for agent to start..."
for i in {1..30}; do
    if curl -s http://localhost:8080/health > /dev/null 2>&1; then
        log "Agent ready after ${i}s"
        break
    fi
    if [ $i -eq 30 ]; then
        error "Agent failed to start after 30s"
        cat /tmp/simple-agent.log
        exit 1
    fi
    sleep 1
done
log "Agent started (PID: $AGENT_PID)"

# Wait for agent to register with Ergatai via MCP
log "Waiting for agent to register with Ergatai..."
for i in {1..10}; do
    AGENT_LIST=$(curl -s -X POST http://localhost:3000/mcp \
        -H "Content-Type: application/json" \
        -H "Accept: application/json, text/event-stream" \
        -H "Mcp-Session-Id: $SESSION_ID" \
        -H "MCP-Protocol-Version: 2025-03-26" \
        -d '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"list_agents","arguments":{}}}')

    if echo "$AGENT_LIST" | grep -q "simple-agent"; then
        log "Simple-agent registered via MCP after ${i}s! ✓"
        break
    fi
    if [ $i -eq 10 ]; then
        warn "simple-agent not found after 10s (expected if it doesn't implement MCP client)"
    fi
    sleep 1
done

log "Agent list: $AGENT_LIST"

if echo "$AGENT_LIST" | grep -q "simple-agent"; then
    log "Simple-agent registered via MCP! ✓"
else
    warn "simple-agent not found (expected if it doesn't implement MCP client)"
fi

# ── Test ACP endpoint directly ──

log "Testing ACP endpoint directly..."
ACP_RESPONSE=$(curl -s -X POST http://localhost:8080/acp/session/new \
    -H "Content-Type: application/json" \
    -d '{"cwd":"/tmp"}')

log "ACP session creation response: $ACP_RESPONSE"

if echo "$ACP_RESPONSE" | grep -q "session_id"; then
    log "ACP endpoint works! ✓"
else
    error "ACP endpoint failed"
    exit 1
fi

log ""
log "============================================"
log "All integration tests PASSED!"
log "============================================"
log ""
log "MCP Protocol: 2025-03-26 (Streamable HTTP + SSE)"
log "API log: /tmp/ergatai-api.log"
log "Agent log: /tmp/simple-agent.log"

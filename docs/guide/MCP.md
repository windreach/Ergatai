# MCP Configuration Guide

## Overview

Ergatai exposes an MCP (Model Context Protocol) server that agents connect to. Each agent needs its own unique URL path.

## Basic Configuration

Add Ergatai to your agent's MCP config:

```json
{
  "mcpServers": {
    "ergatai": {
      "url": "http://localhost:3000/mcp/alice"
    }
  }
}
```

## Agent Names

The URL path suffix is the **agent's conversation name** — used for messaging, discovery, and rmux pane binding.

### Rules

- Names must be **unique** across all agents in the same Ergatai instance
- Do NOT reuse names between agents
- Valid characters: alphanumeric, `-`, `_`, `/`
- Max 64 characters

### Examples

```
http://localhost:3000/mcp/alice
http://localhost:3000/mcp/bob
http://localhost:3000/mcp/claude-code
http://localhost:3000/mcp/cursor-agent-1
```

### Built-in Paths

| Path | Description |
|------|-------------|
| `/mcp/agent-1` | Pre-configured, fixed name |
| `/mcp/agent-2` | Pre-configured, fixed name |
| `/mcp/agent-3` | Pre-configured, fixed name |
| `/mcp` | Shared fallback — auto-assigns names |

> ⚠️ **Do not reuse agent names.** If two agents connect to the same name (e.g., both use `/mcp/alice`), they will share session state and their messages will collide.

## Agent-Specific Configs

### Claude Code

In `~/.claude/claude_desktop_config.json` or project `.mcp.json`:

```json
{
  "mcpServers": {
    "ergatai": {
      "url": "http://localhost:3000/mcp/claude-code"
    }
  }
}
```

### Cursor

In `.cursor/mcp.json`:

```json
{
  "mcpServers": {
    "ergatai": {
      "url": "http://localhost:3000/mcp/cursor"
    }
  }
}
```

### Codex

In `~/.codex/config.json`:

```json
{
  "mcpServers": {
    "ergatai": {
      "url": "http://localhost:3000/mcp/codex"
    }
  }
}
```

## MCP Tools

Once connected, agents can use these tools:

### list_agents

List all connected agents.

```json
// Input
{ "include_capabilities": true }

// Response
{
  "agents": [
    {
      "agent_id": "%15",
      "display_name": "alice",
      "status": "active"
    }
  ],
  "total": 1
}
```

### register_agent_name

Register a human-readable name for the calling agent.

```json
// Input
{ "display_name": "alice" }
```

### send_message

Send a message to another agent.

```json
// Input
{
  "target_agent_id": "cursor",
  "message": "Please refactor src/auth.rs",
  "message_type": "request"
}

// Response
{
  "status": "queued",
  "target_agent": "cursor",
  "delivery_method": "nats_jetstream"
}
```

### submit_orchestration

Submit a DAG workflow for multi-agent execution.

```json
// Input
{
  "dag_definition": "## Task A\n- agent: claude-code\n- task: Analyze code\n\n## Task B\n- agent: cursor\n- task: Write tests\n- depends_on: [Task A]"
}
```

### check_dag_status

Check DAG execution progress.

```json
// Input
{ "dag_id": "dag-abc123" }
```

## Authentication

If the server requires authentication:

```json
{
  "mcpServers": {
    "ergatai": {
      "url": "http://localhost:3000/mcp/alice",
      "headers": {
        "Authorization": "Bearer your-api-token"
      }
    }
  }
}
```

Start server with token:
```bash
ergatai-server --port 3000 --api-token your-secret-token
```

## Troubleshooting

### Agent not appearing in list

1. Check server is running: `ergatai status`
2. Verify URL is correct and server port matches
3. Check agent name is unique

### Messages not delivering

1. Check target agent is active: `ergatai agent list`
2. Verify NATS is running (check server logs)
3. Check message delivery logs

### Connection refused

1. Verify server is running on the expected port
2. Check firewall settings
3. Verify URL scheme (http vs https)

## Next Steps

- [CLI Guide](CLI.md) — manage workspaces and agents
- [Architecture Overview](../architecture/OVERVIEW.md) — understand message flow

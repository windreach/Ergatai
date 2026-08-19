# CLI Guide

## Overview

Ergatai CLI (`ergatai` or `ega`) provides commands to manage workspaces and agents.

```bash
# Full name
ergatai <command>

# Short alias
ega <command>
```

## Quick Start Command

The simplest way to start an agent:

```bash
ega claude
```

This command:
1. Creates a workspace named `claude` (in current directory)
2. Spawns a `claude` agent
3. Attaches to the terminal session

You're immediately in the agent's terminal, ready to work.

### Options

```bash
# Specify working directory
ega claude --work-dir /path/to/project

# Equivalent to:
ergatai start claude --work-dir /path/to/project
```

## Workspace Commands

Workspaces are rmux sessions. Each workspace can contain multiple agents as panes.

```bash
# List all workspaces
ergatai workspace list

# Create a workspace
ergatai workspace create <name> [--work-dir <path>]

# Delete a workspace
ergatai workspace delete <name>
```

## Agent Commands

```bash
# List all agents
ergatai agent list

# Spawn an agent in a workspace
ergatai agent spawn --workspace <name> --command <cmd> [--instruction <text>]

# Stop an agent
ergatai agent kill <agent-id>

# Send message to an agent
ergatai agent message <agent-id> <message>
```

## Status Command

```bash
# Show system status
ergatai status

# Real-time updates via WebSocket
ergatai status --watch
```

## Global Options

```bash
# API server URL (default: http://localhost:3000)
ergatai --api-url http://custom:3000 <command>

# Or use environment variable
export ERGATAI_API_URL=http://localhost:3000

# API token for authentication
ergatai --token <token> <command>

# Or use environment variable
export ERGATAI_API_TOKEN=your-token
```

## Advanced: Multiple Agents in One Workspace

For multi-agent collaboration in the same workspace:

```bash
# Create workspace
ergatai workspace create my-project

# Spawn multiple agents (each gets a pane)
ergatai agent spawn --workspace my-project --command claude
ergatai agent spawn --workspace my-project --command opencode
ergatai agent spawn --workspace my-project --command cursor

# Attach to see all panes
rmux attach -t ergatai-my-project
```

Layout:
```
┌──────────────┬──────────────┬──────────────┐
│   claude     │   opencode   │   cursor     │
│   (pane 0)   │   (pane 1)   │   (pane 2)   │
└──────────────┴──────────────┴──────────────┘
```

## Examples

### Start a single agent

```bash
cd ~/projects/my-app
ega claude
# Now in claude's terminal, working on my-app
```

### Start multiple agents for collaboration

```bash
ergatai workspace create collab-project --work-dir ~/projects/collab
ergatai agent spawn --workspace collab-project --command claude
ergatai agent spawn --workspace collab-project --command cursor
rmux attach -t ergatai-collab-project
```

### Send message between agents

```bash
# List agents to get IDs
ergatai agent list

# Send message
ergatai agent message %15 "Please review my changes in src/auth.rs"
```

## Next Steps

- [MCP Configuration](MCP.md) — configure your agents to connect to Ergatai
- [Architecture Overview](../architecture/OVERVIEW.md) — understand how it works

/**
 * Ergatai MCP Server
 *
 * A Model Context Protocol (MCP) server that exposes Ergatai's multi-agent
 * collaboration primitives as tools. Runs as a stdio subprocess injected
 * into ACP agent sessions by Rust's `build_ergatai_mcp_servers()`.
 *
 * Architecture:
 *   ACP Agent  ←→  (stdio JSON-RPC)  ←→  This MCP server  ←→  NAPI  ←→  Rust
 *
 * Env vars (set by Rust McpServerConfig):
 *   ERGATAI_NATIVE_BINDING   - path to native-binding.js
 *   ERGATAI_PROJECT_ROOT     - project working directory
 *   ERGATAI_SESSION_MODE     - "main" | "sub"
 *   ERGATAI_AGENT_ID         - (sub mode) current agent id
 *   ERGATAI_NODE_ID          - (sub mode) DAG node id
 *   ERGATAI_DAG_ID           - (sub mode) DAG id
 *   ERGATAI_AVAILABLE_AGENTS - comma-separated agent list
 */

import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js"
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js"
import { z } from "zod"

// ── Load native binding ────────────────────────────────────

const nativeBindingPath = process.env.ERGATAI_NATIVE_BINDING
if (!nativeBindingPath) {
  console.error("[ergatai-mcp] ERGATAI_NATIVE_BINDING not set, exiting")
  process.exit(1)
}

let native: any
try {
  native = require(nativeBindingPath)
} catch (err: any) {
  console.error(
    `[ergatai-mcp] Failed to load native binding at ${nativeBindingPath}: ${err?.message}`,
  )
  process.exit(1)
}

// ── Read context from env ──────────────────────────────────

const sessionMode = process.env.ERGATAI_SESSION_MODE || "main"
const agentId = process.env.ERGATAI_AGENT_ID || ""
const nodeId = process.env.ERGATAI_NODE_ID || ""
const dagId = process.env.ERGATAI_DAG_ID || ""
const availableAgents = process.env.ERGATAI_AVAILABLE_AGENTS || ""
const projectRoot = process.env.ERGATAI_PROJECT_ROOT || ""

// ── Create MCP server ──────────────────────────────────────

const server = new McpServer({
  name: "ergatai",
  version: "1.0.0",
})

// ── Helper: wrap NAPI call with error handling ─────────────

async function callNapi<T>(label: string, fn: () => Promise<T>): Promise<T> {
  try {
    return await fn()
  } catch (err: any) {
    const msg = err?.message ?? String(err)
    console.error(`[ergatai-mcp] ${label} failed: ${msg}`)
    throw new Error(`${label}: ${msg}`)
  }
}

// ── DAG Orchestration Tools ────────────────────────────────

server.registerTool(
  "ergatai_dag_submit",
  {
    title: "Submit DAG",
    description:
      "Submit a DAG markdown spec for execution. The markdown should contain " +
      "task blocks with agent assignments, dependencies, and input/output bindings. " +
      "Returns the list of submitted task IDs.",
    inputSchema: {
      markdown: z.string().describe("DAG markdown specification"),
    },
  },
  async ({ markdown }) => {
    const result: string = await callNapi("dagSubmit", () =>
      native.dagSubmit(markdown),
    )
    const submittedTaskIds = JSON.parse(result)
    return {
      content: [
        {
          type: "text" as const,
          text: `Submitted ${submittedTaskIds.length} task(s): ${submittedTaskIds.join(", ")}`,
        },
      ],
    }
  },
)

server.registerTool(
  "ergatai_dag_status",
  {
    title: "DAG Status",
    description:
      "Get a human-readable summary of the current DAG execution status. " +
      "AI-friendly text suitable for including in responses.",
  },
  async () => {
    const status: string = await callNapi("dagStatus", () => native.dagStatus())
    return { content: [{ type: "text" as const, text: status }] }
  },
)

server.registerTool(
  "ergatai_dag_progress",
  {
    title: "DAG Progress",
    description: "Get DAG execution progress as a fraction between 0.0 and 1.0.",
  },
  async () => {
    const progress: number = await callNapi("dagProgress", () =>
      native.dagProgress(),
    )
    return {
      content: [{ type: "text" as const, text: `Progress: ${(progress * 100).toFixed(1)}%` }],
    }
  },
)

server.registerTool(
  "ergatai_dag_is_complete",
  {
    title: "DAG Complete?",
    description: "Check if the current DAG execution has completed (all tasks done).",
  },
  async () => {
    const complete: boolean = await callNapi("dagIsComplete", () =>
      native.dagIsComplete(),
    )
    return {
      content: [{ type: "text" as const, text: complete ? "DAG complete" : "DAG in progress" }],
    }
  },
)

server.registerTool(
  "ergatai_dag_get_state",
  {
    title: "DAG Full State",
    description:
      "Get the full DAG state as JSON, including all nodes, their statuses, " +
      "outputs, and dependency graph. Useful for detailed inspection.",
  },
  async () => {
    const raw: string = await callNapi("dagGetState", () => native.dagGetState())
    const state = JSON.parse(raw)
    return {
      content: [{ type: "text" as const, text: JSON.stringify(state, null, 2) }],
    }
  },
)

// ── Agent Status Tools ─────────────────────────────────────

server.registerTool(
  "ergatai_agents_status",
  {
    title: "Agent Status",
    description:
      "Get the status of all running agents, including their current task, " +
      "session state, and resource usage.",
  },
  async () => {
    try {
      const raw: string = await native.taskGetAgentsStatus()
      const agents = JSON.parse(raw)
      return {
        content: [{ type: "text" as const, text: JSON.stringify(agents, null, 2) }],
      }
    } catch {
      return { content: [{ type: "text" as const, text: "[]" }] }
    }
  },
)

server.registerTool(
  "ergatai_agents_list",
  {
    title: "List Available Agents",
    description:
      "List all available agents (built-in + user-defined hosted agents). " +
      "Use this to discover which agents can be assigned to DAG tasks.",
  },
  async () => {
    const agents = native.scanLocalAgents()
    return {
      content: [{ type: "text" as const, text: JSON.stringify(agents, null, 2) }],
    }
  },
)

// ── Inter-Agent Messaging Tools ────────────────────────────

server.registerTool(
  "ergatai_message_send",
  {
    title: "Send Agent Message",
    description:
      "Send a message from the current agent to another agent via NATS. " +
      "Use this for direct coordination between agents in a DAG.",
    inputSchema: {
      fromAgent: z.string().describe("Source agent name"),
      toAgent: z.string().describe("Target agent name"),
      content: z.string().describe("Message content"),
      threadId: z.string().optional().describe("Optional conversation thread ID"),
    },
  },
  async ({ fromAgent, toAgent, content, threadId }) => {
    await callNapi("natsRouteAgentMessage", () =>
      native.natsRouteAgentMessage(fromAgent, toAgent, content, threadId ?? null),
    )
    return {
      content: [{ type: "text" as const, text: `Message sent from ${fromAgent} to ${toAgent}` }],
    }
  },
)

server.registerTool(
  "ergatai_message_scan_mentions",
  {
    title: "Scan & Route @Mentions",
    description:
      "Scan text for @agent mentions and automatically route messages to them. " +
      "E.g., '@codex please review this' routes a message to the codex agent.",
    inputSchema: {
      fromAgent: z.string().describe("Source agent name"),
      text: z.string().describe("Text to scan for @mentions"),
      threadId: z.string().optional().describe("Optional conversation thread ID"),
    },
  },
  async ({ fromAgent, text, threadId }) => {
    const count: number = await callNapi("natsScanAndRouteMentions", () =>
      native.natsScanAndRouteMentions(fromAgent, text, threadId ?? null),
    )
    return {
      content: [
        { type: "text" as const, text: `Routed ${count} mention(s)` },
      ],
    }
  },
)

// ── Context Tool (returns env info to the agent) ───────────

server.registerTool(
  "ergatai_context",
  {
    title: "Ergatai Context",
    description:
      "Returns the current Ergatai execution context: session mode, agent ID, " +
      "DAG ID, node ID, and available agents. Useful for the agent to understand " +
      "its role in the collaboration.",
  },
  async () => {
    const ctx = {
      sessionMode,
      agentId,
      nodeId,
      dagId,
      availableAgents: availableAgents.split(",").filter(Boolean),
      projectRoot,
    }
    return {
      content: [{ type: "text" as const, text: JSON.stringify(ctx, null, 2) }],
    }
  },
)

// ── Start the server ───────────────────────────────────────

async function main() {
  const transport = new StdioServerTransport()
  await server.connect(transport)
  console.error(
    `[ergatai-mcp] Server started (mode=${sessionMode}, agent=${agentId || "none"})`,
  )
}

main().catch((err) => {
  console.error("[ergatai-mcp] Fatal:", err)
  process.exit(1)
})

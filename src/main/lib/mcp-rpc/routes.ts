/**
 * MCP RPC Routes
 *
 * Maps NATS subject suffixes to NAPI function calls.
 * Each route is a function: (payload) => Promise<result>
 *
 * Subject pattern: ergatai.mcp.<operation>
 * Example: ergatai.mcp.dag.submit → routes["dag.submit"]
 */

import { join } from "path"
import { app } from "electron"

/** Lazy-load native binding (same pattern as tRPC routers). */
let _native: any = null
function native(): any {
  if (!_native) {
    const appRoot = app.getAppPath()
    const candidates = [
      join(appRoot, "src/native-binding"),
      join(appRoot, "out/main/native-binding"),
      join(appRoot, "native-binding"),
    ]
    for (const p of candidates) {
      try {
        _native = require(p)
        break
      } catch {}
    }
    if (!_native) throw new Error("Cannot find native-binding module")
  }
  return _native
}

/** Route handler type. */
export type RouteHandler = (payload: any) => Promise<unknown>

/**
 * Route table.
 *
 * Keys are subject suffixes (after "ergatai.mcp.").
 * Values are async functions that call NAPI and return results.
 *
 * ── DAG orchestration ────────────────────────────────────
 */
export const routes: Record<string, RouteHandler> = {
  /** Submit a DAG markdown spec for execution. */
  "dag.submit": async (body) => {
    const { dagSubmit } = native()
    const result: string = await dagSubmit(body.markdown)
    return { submittedTaskIds: JSON.parse(result) }
  },

  /** Get DAG progress (0.0–1.0). */
  "dag.progress": async () => {
    const { dagProgress } = native()
    return { progress: await dagProgress() }
  },

  /** Check if DAG is complete. */
  "dag.isComplete": async () => {
    const { dagIsComplete } = native()
    return { complete: await dagIsComplete() }
  },

  /** AI-friendly DAG status text. */
  "dag.status": async () => {
    const { dagStatus } = native()
    return { status: await dagStatus() }
  },

  /** Full DAG state as JSON. */
  "dag.getState": async () => {
    const { dagGetState } = native()
    const raw: string = await dagGetState()
    return JSON.parse(raw)
  },

  // ── Agent status ───────────────────────────────────────

  /** Status of all running agents. */
  "agents.status": async () => {
    const { taskGetAgentsStatus } = native()
    try {
      const raw: string = await taskGetAgentsStatus()
      return JSON.parse(raw)
    } catch {
      return []
    }
  },

  /** List available agents (built-in + hosted). */
  "agents.list": async () => {
    const { scanLocalAgents } = native()
    return scanLocalAgents()
  },

  // ── Inter-agent messaging (via NATS) ──────────────────

  /** Route a message from one agent to another. */
  "message.send": async (body) => {
    const { natsRouteAgentMessage } = native()
    await natsRouteAgentMessage(
      body.fromAgent,
      body.toAgent,
      body.content,
      body.threadId ?? null,
    )
    return { ok: true }
  },

  /** Scan text for @mentions and route them. */
  "message.scanMentions": async (body) => {
    const { natsScanAndRouteMentions } = native()
    const count: number = await natsScanAndRouteMentions(
      body.fromAgent,
      body.text,
      body.threadId ?? null,
    )
    return { routed: count }
  },

  // ── NATS lifecycle ─────────────────────────────────────

  /** Initialize NATS (idempotent). Returns port. */
  "nats.init": async () => {
    const { natsInit } = native()
    const port: number = await natsInit()
    return { port }
  },

  /** Check if NATS is initialized. */
  "nats.isInitialized": async () => {
    const { natsIsInitialized } = native()
    return { initialized: await natsIsInitialized() }
  },
}

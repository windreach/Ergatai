/**
 * MCP RPC Handler
 *
 * Subscribes to NATS subject "ergatai.mcp.*" and dispatches incoming
 * request/reply messages to the route table defined in routes.ts.
 *
 * Protocol (NATS request/reply):
 *   Subject:  ergatai.mcp.<operation>   (e.g. ergatai.mcp.dag.submit)
 *   Payload:  JSON { ...operation args }
 *   Reply:    JSON { ok: true, data: ... } on success
 *             JSON { ok: false, error: "..." } on failure
 *
 * Uses the `nats` npm package to connect to the same NATS server
 * that the Rust backend manages.
 */

import { connect, type NatsConnection, type Msg } from "nats"
import { routes } from "./routes"

const SUBJECT_PREFIX = "ergatai.mcp."

let connection: NatsConnection | null = null

/**
 * Start the MCP RPC handler.
 *
 * Connects to NATS and subscribes to all `ergatai.mcp.>` subjects.
 * Returns the NATS connection (useful for tests / shutdown).
 */
export async function startMcpRpcHandler(
  natsUrl: string,
): Promise<NatsConnection> {
  if (connection) {
    console.log("[MCP RPC] Already running, skipping")
    return connection
  }

  console.log(`[MCP RPC] Connecting to NATS at ${natsUrl}...`)
  connection = await connect({ servers: natsUrl })
  console.log("[MCP RPC] Connected, subscribing to ergatai.mcp.>")

  // Subscribe to all MCP RPC requests (queue group ensures single processing)
  const subscription = connection.subscribe(`${SUBJECT_PREFIX}>`, {
    queue: "mcp-rpc-handler",
  })

  // Process messages in background
  ;(async () => {
    for await (const msg of subscription) {
      handleRequest(msg).catch((err) => {
        console.error("[MCP RPC] Unhandled error:", err)
        replyError(msg, `Internal error: ${err?.message ?? String(err)}`)
      })
    }
    console.log("[MCP RPC] Subscription closed")
  })()

  console.log(
    `[MCP RPC] Handler started. Registered routes: ${Object.keys(routes).join(", ")}`,
  )

  return connection
}

/**
 * Stop the MCP RPC handler.
 */
export async function stopMcpRpcHandler(): Promise<void> {
  if (connection) {
    await connection.drain()
    await connection.close()
    connection = null
    console.log("[MCP RPC] Handler stopped")
  }
}

/**
 * Handle a single NATS request message.
 */
async function handleRequest(msg: Msg): Promise<void> {
  const subject = msg.subject

  // Extract operation from subject: "ergatai.mcp.dag.submit" → "dag.submit"
  if (!subject.startsWith(SUBJECT_PREFIX)) {
    replyError(msg, `Invalid subject: ${subject}`)
    return
  }
  const operation = subject.slice(SUBJECT_PREFIX.length)

  // Look up route
  const handler = routes[operation]
  if (!handler) {
    replyError(msg, `Unknown operation: ${operation}`)
    return
  }

  // Parse payload
  let payload: any = {}
  try {
    const raw = msg.string()
    if (raw) payload = JSON.parse(raw)
  } catch (err: any) {
    replyError(msg, `Invalid JSON payload: ${err?.message ?? String(err)}`)
    return
  }

  // Execute handler
  try {
    const result = await handler(payload)
    replyOk(msg, result)
  } catch (err: any) {
    console.error(`[MCP RPC] ${operation} failed:`, err)
    replyError(msg, err?.message ?? String(err))
  }
}

/** Send a success reply. */
function replyOk(msg: Msg, data: unknown): void {
  const payload = JSON.stringify({ ok: true, data })
  msg.respond(new TextEncoder().encode(payload)).catch((err) => {
    console.error("[MCP RPC] Failed to send reply:", err)
  })
}

/** Send an error reply. */
function replyError(msg: Msg, error: string): void {
  const payload = JSON.stringify({ ok: false, error })
  msg.respond(new TextEncoder().encode(payload)).catch((err) => {
    console.error("[MCP RPC] Failed to send error reply:", err)
  })
}

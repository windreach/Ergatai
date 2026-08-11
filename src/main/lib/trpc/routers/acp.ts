import { observable } from "@trpc/server/observable"
import { z } from "zod"
import { publicProcedure, router } from "../index"
import { join } from "path"
import { app } from "electron"
import { dagDetector } from "../../dag-detector"

// Load native binding — resolve from app root
function loadNativeBinding(): any {
  const appRoot = app.getAppPath()
  // Try multiple locations
  const candidates = [
    join(appRoot, "src/native-binding"),
    join(appRoot, "out/main/native-binding"),
    join(appRoot, "native-binding"),
  ]
  for (const p of candidates) {
    try {
      return require(p)
    } catch {}
  }
  throw new Error("Cannot find native-binding module")
}

const nativeBinding = loadNativeBinding()
const {
  acpCreateSession,
  acpSendPrompt,
  acpCloseSession,
  acpPollEvents,
  acpRespondPermission,
  acpListSessions,
  acpListAgentSessions,
  acpResumeSession,
  acpDeleteSession,
  acpGetPersistedSessions,
  acpSaveSessionMeta,
  acpUpdateSessionTitle,
  acpSetSessionMode,
  acpSetConfigOption,
  acpPoolCreate,
  acpPoolSubmitTask,
  acpPoolCancelTask,
  acpPoolStatus,
  acpPoolShutdown,
  acpPoolList,
} = nativeBinding

// Types from native-binding.d.ts
interface NapiSessionEvent {
  sessionId: string
  eventType: string
  data: string
}

/**
 * ACP event polling interval (ms)
 */
const POLL_INTERVAL = 100

/**
 * Active polling timers per subChatId
 */
const activePollers = new Map<string, NodeJS.Timeout>()

/**
 * Map from subChatId → ACP sessionId
 */
const sessionMap = new Map<string, string>()

/**
 * Map from requestId → { subChatId, sessionId } for permission routing
 */
const permissionMap = new Map<string, { subChatId: string; sessionId: string }>()

/**
 * Translate ACP event → UIMessageChunk[] for frontend
 */
function translateEvent(event: NapiSessionEvent): any[] {
  const chunks: any[] = []
  let data: any
  try {
    data = typeof event.data === "string" ? JSON.parse(event.data) : event.data
  } catch {
    data = event.data
  }

  switch (event.eventType) {
    case "agent_message_chunk": {
      const content = data?.content || data?.ContentBlock || data
      if (typeof content === "string") {
        chunks.push({
          type: "text-delta",
          id: `text-${event.sessionId}`,
          delta: content,
        })
      } else if (content?.text) {
        chunks.push({
          type: "text-delta",
          id: `text-${event.sessionId}`,
          delta: content.text,
        })
      }
      break
    }

    case "agent_thought_chunk": {
      const content = data?.content || data?.ContentBlock || data
      const text = typeof content === "string" ? content : content?.text || ""
      if (text) {
        chunks.push({
          type: "reasoning-delta",
          id: `reasoning-${event.sessionId}`,
          delta: text,
        })
      }
      break
    }

    case "tool_call": {
      const toolName = data?.tool_name || data?.toolName || "unknown"
      const toolInput = data?.tool_input || data?.toolInput || data?.input || {}
      const toolCallId = data?.tool_call_id || data?.toolCallId || `tc-${Date.now()}`
      chunks.push({
        type: "tool-input-start",
        toolName,
        toolCallId,
      })
      chunks.push({
        type: "tool-input-available",
        toolName,
        toolCallId,
        args: typeof toolInput === "string" ? tryParseJSON(toolInput) : toolInput,
      })
      break
    }

    case "tool_call_update": {
      const toolName = data?.tool_name || data?.toolName || "unknown"
      const result = data?.result || data?.output || ""
      const toolCallId = data?.tool_call_id || data?.toolCallId || ""
      chunks.push({
        type: "tool-output",
        toolName,
        toolCallId,
        output: typeof result === "string" ? result : JSON.stringify(result),
      })
      break
    }

    case "permission_request": {
      const requestId = data?.requestId || data?.request_id || `perm-${Date.now()}`
      const options = data?.options || []
      const toolName = data?.toolName || data?.tool_name || "tool"

      permissionMap.set(requestId, {
        subChatId: "", // filled by caller
        sessionId: event.sessionId,
      })

      chunks.push({
        type: "ask-user-question",
        toolUseId: requestId,
        questions: options.map((opt: any) => ({
          id: opt.optionId || opt.option_id,
          label: opt.label,
        })),
        toolName,
      })
      break
    }

    case "usage_update": {
      const usage = data?.usage || data
      chunks.push({
        type: "message-metadata",
        messageMetadata: {
          usage: {
            inputTokens: usage?.input_tokens || usage?.inputTokens || 0,
            outputTokens: usage?.output_tokens || usage?.outputTokens || 0,
          },
        },
      })
      break
    }

    case "closed": {
      chunks.push({
        type: "finish",
        finishReason: "stop" as const,
      })
      break
    }

    // ── Pool task events ──
    case "task_dispatched": {
      chunks.push({
        type: "pool-task-dispatched",
        taskId: data?.task_id,
        agentIndex: data?.agent_index,
      })
      break
    }

    case "task_completed": {
      chunks.push({
        type: "pool-task-completed",
        taskId: data?.task_id,
        stopReason: data?.stop_reason,
      })
      break
    }

    case "task_failed": {
      chunks.push({
        type: "pool-task-failed",
        taskId: data?.task_id,
        error: data?.error,
      })
      break
    }

    // ── Observer lifecycle events ──
    case "turn_started": {
      chunks.push({
        type: "turn-started",
        sessionId: data?.session_id,
        agentIndex: data?.agent_index,
      })
      break
    }

    case "turn_completed": {
      chunks.push({
        type: "turn-completed",
        sessionId: data?.session_id,
        agentIndex: data?.agent_index,
      })
      break
    }

    case "model_switched": {
      chunks.push({
        type: "model-switched",
        sessionId: data?.session_id,
        model: data?.model,
      })
      break
    }

    case "pool_stopped": {
      chunks.push({
        type: "pool-stopped",
        agentName: data?.agent_name,
      })
      break
    }

    case "available_commands_update": {
      const commands: Array<{ name: string; description: string; input?: string }> =
        Array.isArray(data?.commands)
          ? data.commands.map((c: any) => ({
              name: c.name ?? "",
              description: c.description ?? "",
              input: c.input,
            }))
          : []
      chunks.push({
        type: "available-commands",
        commands,
      })
      break
    }

    default:
      break
  }

  return chunks
}

function tryParseJSON(str: string): any {
  try {
    return JSON.parse(str)
  } catch {
    return str
  }
}

function stopPolling(subChatId: string) {
  const timer = activePollers.get(subChatId)
  if (timer) {
    clearInterval(timer)
    activePollers.delete(subChatId)
  }
}

export const acpRouter = router({
  /**
   * Main chat subscription — replaces claude.chat
   */
  chat: publicProcedure
    .input(
      z.object({
        subChatId: z.string(),
        chatId: z.string(),
        prompt: z.string(),
        cwd: z.string(),
        projectPath: z.string().optional(),
        mode: z
          .union([z.literal("agent"), z.literal("team"), z.enum(["auto", "plan"])])
          .transform((v): "auto" | "plan" => (v === "agent" || v === "team" ? "auto" : v))
          .default("auto"),
        sessionId: z.string().optional(),
        model: z.string().optional(),
        maxThinkingTokens: z.number().optional(),
        agentName: z.string().optional(),
        images: z.array(z.any()).optional(),
        historyEnabled: z.boolean().optional(),
        offlineModeEnabled: z.boolean().optional(),
        enableTasks: z.boolean().optional(),
      })
    )
    .subscription(({ input }) => {
      const { subChatId, prompt, cwd, sessionId, agentName } = input
      const agent = agentName || "claude-code"

      console.log(`[ACP] Chat start: sub=${subChatId.slice(-8)} agent=${agent} cwd=${cwd}`)

      return observable<any>((emit) => {
        let cancelled = false
        let started = false
        // Track which text/reasoning stream IDs have had their *-start chunk emitted
        // AI SDK's processUIMessageStream requires text-start/reasoning-start before
        // text-delta/reasoning-delta, otherwise it throws UIMessageStreamError which
        // kills the TransformStream and closes the reader.
        const startedTextStreams = new Set<string>()
        const startedReasoningStreams = new Set<string>()

        const start = async () => {
          // Prevent multiple start() calls (e.g., from React Strict Mode)
          if (started) {
            console.log(`[ACP] start() already called, skipping duplicate call`)
            return
          }
          started = true

          let acpSessionId: string

          console.log(`[ACP] Starting session for agent=${agent}, cwd=${cwd}`)

          try {
            if (sessionId) {
              console.log(`[ACP] Trying to resume session: ${sessionId}`)
              try {
                acpSessionId = await acpResumeSession(agent, sessionId, cwd)
                console.log(`[ACP] Resumed session: ${acpSessionId}`)
              } catch (e) {
                console.log(`[ACP] Resume failed, creating new: ${e}`)
                acpSessionId = await acpCreateSession(agent, cwd)
              }
            } else {
              console.log(`[ACP] Creating new session for agent=${agent}`)
              acpSessionId = await acpCreateSession(agent, cwd)
              console.log(`[ACP] Session created: ${acpSessionId}`)
            }

            sessionMap.set(subChatId, acpSessionId)
            acpSaveSessionMeta(acpSessionId, agent, cwd)

            console.log(`[ACP] Sending prompt (length=${prompt.length}): ${prompt.slice(0, 100)}...`)
            await acpSendPrompt(acpSessionId, prompt)
            console.log(`[ACP] Prompt sent successfully`)

            console.log(`[ACP] Session ready: ${acpSessionId}`)
          } catch (err) {
            console.error("[ACP] Failed to start session:", err)
            emit.next({ type: "error", errorText: `Failed to start ACP session: ${err}` })
            emit.complete()
            return
          }

          // Poll events
          let pollCount = 0
          let isPolling = false
          const timer = setInterval(async () => {
            // Prevent overlapping poll iterations
            if (isPolling) {
              return
            }
            isPolling = true

            try {
              pollCount++
              if (pollCount % 50 === 0) {
                console.log(`[ACP] Polling... ${pollCount} iterations`)
              }
              if (cancelled) return

              const events = acpPollEvents()
              if (events.length > 0) {
                console.log(`[ACP] Got ${events.length} events, polling iteration ${pollCount}, types: ${events.map(e => e.eventType).join(", ")}`)
              }
              const acpSessionId = sessionMap.get(subChatId)
              if (!acpSessionId) {
                console.log(`[ACP] No session mapped for subChatId=${subChatId}`)
                return
              }

              for (const event of events) {
                if (event.sessionId !== acpSessionId) continue

                // Handle file access approval requests separately
                if (event.eventType === "file_access_approval_request") {
                  try {
                    const data = typeof event.data === "string" ? JSON.parse(event.data) : event.data
                    const { addPendingApproval } = await import("./fileAccess")

                    addPendingApproval({
                      id: data.request_id,
                      agentId: data.agent_id,
                      sessionId: data.session_id,
                      scope: data.scope,
                      filePaths: data.file_paths || [],
                      mode: "ADMIN",
                      reason: data.reason || "Agent requests ADMIN access",
                      createdAt: Date.now(),
                    })

                    console.log(`[ACP] File access approval request added: ${data.request_id}`)
                  } catch (err) {
                    console.error("[ACP] Failed to handle file access approval request:", err)
                  }
                  continue
                }

                const chunks = translateEvent(event)
                for (const chunk of chunks) {
                  // AI SDK requires *-start before *-delta chunks.
                  // Inject text-start/reasoning-start on first occurrence per stream ID.
                  if (chunk.type === "text-delta") {
                    if (!startedTextStreams.has(chunk.id)) {
                      startedTextStreams.add(chunk.id)
                      emit.next({ type: "text-start", id: chunk.id })
                    }
                  } else if (chunk.type === "reasoning-delta") {
                    if (!startedReasoningStreams.has(chunk.id)) {
                      startedReasoningStreams.add(chunk.id)
                      emit.next({ type: "reasoning-start", id: chunk.id })
                    }
                  }

                  // Update permission map with subChatId
                  if (chunk.type === "ask-user-question") {
                    const mapping = permissionMap.get(chunk.toolUseId)
                    if (mapping) mapping.subChatId = subChatId
                  }

                  // Accumulate text for DAG detection
                  if (chunk.type === "text-delta" && chunk.textDelta) {
                    dagDetector.appendChunk(acpSessionId, chunk.textDelta)
                  }

                  // Add small delay between chunks to avoid overwhelming the stream
                  await new Promise(resolve => setTimeout(resolve, 1))

                  emit.next(chunk)
                  console.log(`[ACP] Emitted chunk: type=${chunk.type} subChatId=${subChatId.slice(-8)}`)

                  if (chunk.type === "finish") {
                    console.log(`[ACP] Finish chunk received, completing subscription`)
                    // Close any open text/reasoning streams so the AI SDK finalizes them
                    for (const id of startedTextStreams) {
                      emit.next({ type: "text-end", id })
                    }
                    for (const id of startedReasoningStreams) {
                      emit.next({ type: "reasoning-end", id })
                    }
                    // Check for DAG before finishing
                    dagDetector.checkAndSubmit(acpSessionId).catch((err) => {
                      console.error("[ACP] DAG auto-submit failed:", err)
                    })

                    clearInterval(timer)
                    activePollers.delete(subChatId)
                    sessionMap.delete(subChatId)
                    dagDetector.clearSession(acpSessionId)
                    emit.complete()
                    return
                  }
                }
              }
            } catch (err) {
              console.error("[ACP] Poll error:", err)
              // Close any open text/reasoning streams
              for (const id of startedTextStreams) {
                emit.next({ type: "text-end", id })
              }
              for (const id of startedReasoningStreams) {
                emit.next({ type: "reasoning-end", id })
              }
              emit.next({ type: "error", errorText: String(err) })
              clearInterval(timer)
              activePollers.delete(subChatId)
              emit.complete()
            } finally {
              isPolling = false
            }
          }, POLL_INTERVAL)

          activePollers.set(subChatId, timer)
        }

        start()

        return () => {
          console.log(`[ACP] Subscription cleanup called for subChatId=${subChatId.slice(-8)}`)
          cancelled = true
          stopPolling(subChatId)
          const sid = sessionMap.get(subChatId)
          if (sid) {
            acpCloseSession(sid).catch(() => {})
            sessionMap.delete(subChatId)
          }
        }
      })
    }),

  /**
   * List active sessions
   */
  listSessions: publicProcedure.query(async () => {
    try {
      return await acpListSessions()
    } catch (err) {
      console.error("[ACP] Failed to list sessions:", err)
      return []
    }
  }),

  /**
   * Get persisted sessions
   */
  getPersistedSessions: publicProcedure.query(async () => {
    try {
      return acpGetPersistedSessions()
    } catch (err) {
      return []
    }
  }),

  /**
   * Respond to permission request
   */
  respondPermission: publicProcedure
    .input(z.object({
      requestId: z.string(),
      optionId: z.string().optional(),
    }))
    .mutation(async ({ input }) => {
      const mapping = permissionMap.get(input.requestId)
      if (!mapping) {
        return { success: false, error: "Permission request not found" }
      }
      try {
        await acpRespondPermission(mapping.sessionId, input.requestId, input.optionId || "allow")
        permissionMap.delete(input.requestId)
        return { success: true }
      } catch (err) {
        return { success: false, error: String(err) }
      }
    }),

  /**
   * Close session
   */
  closeSession: publicProcedure
    .input(z.object({ subChatId: z.string() }))
    .mutation(async ({ input }) => {
      stopPolling(input.subChatId)
      const sid = sessionMap.get(input.subChatId)
      if (sid) {
        await acpCloseSession(sid).catch(() => {})
        sessionMap.delete(input.subChatId)
      }
      return { success: true }
    }),

  /**
   * Delete session
   */
  deleteSession: publicProcedure
    .input(z.object({ agentName: z.string(), sessionId: z.string() }))
    .mutation(async ({ input }) => {
      try {
        await acpDeleteSession(input.agentName, input.sessionId)
        return { success: true }
      } catch (err) {
        return { success: false, error: String(err) }
      }
    }),

  /**
   * Update session title
   */
  updateSessionTitle: publicProcedure
    .input(z.object({ sessionId: z.string(), title: z.string() }))
    .mutation(async ({ input }) => {
      try {
        acpUpdateSessionTitle(input.sessionId, input.title)
        return { success: true }
      } catch (err) {
        return { success: false, error: String(err) }
      }
    }),

  /**
   * Cancel current operation (for compatibility with claude.cancel)
   */
  cancel: publicProcedure
    .input(z.object({ subChatId: z.string() }))
    .mutation(({ input }) => {
      stopPolling(input.subChatId)
      const sid = sessionMap.get(input.subChatId)
      if (sid) {
        acpCloseSession(sid).catch(() => {})
        sessionMap.delete(input.subChatId)
      }
      return { success: true }
    }),

  /**
   * Check if session is active (for compatibility)
   */
  isActive: publicProcedure
    .input(z.object({ subChatId: z.string() }))
    .query(({ input }) => {
      return activePollers.has(input.subChatId)
    }),

  /**
   * Respond to tool approval (alias for respondPermission, for compatibility)
   */
  respondToolApproval: publicProcedure
    .input(z.object({
      requestId: z.string(),
      optionId: z.string().optional(),
    }))
    .mutation(async ({ input }) => {
      const mapping = permissionMap.get(input.requestId)
      if (!mapping) {
        return { success: false, error: "Request not found" }
      }
      try {
        await acpRespondPermission(mapping.sessionId, input.requestId, input.optionId || "allow")
        permissionMap.delete(input.requestId)
        return { success: true }
      } catch (err) {
        return { success: false, error: String(err) }
      }
    }),

  // ── Session Configuration ──

  /**
   * Set session mode (e.g., "plan", "auto")
   */
  setSessionMode: publicProcedure
    .input(z.object({
      sessionId: z.string(),
      modeId: z.string(),
    }))
    .mutation(async ({ input }) => {
      try {
        await acpSetSessionMode(input.sessionId, input.modeId)
        return { success: true }
      } catch (err) {
        return { success: false, error: String(err) }
      }
    }),

  /**
   * Set a session config option
   */
  setConfigOption: publicProcedure
    .input(z.object({
      sessionId: z.string(),
      configId: z.string(),
      valueId: z.string(),
    }))
    .mutation(async ({ input }) => {
      try {
        await acpSetConfigOption(input.sessionId, input.configId, input.valueId)
        return { success: true }
      } catch (err) {
        return { success: false, error: String(err) }
      }
    }),

  // ── Agent Pool Management ──

  /**
   * Create an agent pool with N concurrent instances
   * Pool size is limited to 10 to prevent resource exhaustion.
   * Adjust this limit based on your system's capabilities and agent requirements.
   */
  poolCreate: publicProcedure
    .input(z.object({
      agentName: z.string(),
      poolSize: z.number().min(1).max(10),
    }))
    .mutation(async ({ input }) => {
      try {
        await acpPoolCreate(input.agentName, input.poolSize)
        return { success: true }
      } catch (err) {
        return { success: false, error: String(err) }
      }
    }),

  /**
   * Submit a task to an agent pool
   */
  poolSubmitTask: publicProcedure
    .input(z.object({
      agentName: z.string(),
      prompt: z.string(),
      cwd: z.string(),
    }))
    .mutation(async ({ input }) => {
      try {
        const taskId = await acpPoolSubmitTask(input.agentName, input.prompt, input.cwd)
        return { success: true, taskId }
      } catch (err) {
        return { success: false, error: String(err) }
      }
    }),

  /**
   * Cancel a pool task
   */
  poolCancelTask: publicProcedure
    .input(z.object({
      agentName: z.string(),
      taskId: z.string(),
    }))
    .mutation(async ({ input }) => {
      try {
        await acpPoolCancelTask(input.agentName, input.taskId)
        return { success: true }
      } catch (err) {
        return { success: false, error: String(err) }
      }
    }),

  /**
   * Get pool status
   */
  poolStatus: publicProcedure
    .input(z.object({
      agentName: z.string(),
    }))
    .query(async ({ input }) => {
      try {
        return await acpPoolStatus(input.agentName)
      } catch (err) {
        throw new Error(String(err))
      }
    }),

  /**
   * Shutdown an agent pool
   */
  poolShutdown: publicProcedure
    .input(z.object({
      agentName: z.string(),
    }))
    .mutation(async ({ input }) => {
      try {
        await acpPoolShutdown(input.agentName)
        return { success: true }
      } catch (err) {
        return { success: false, error: String(err) }
      }
    }),

  /**
   * List all agent pools
   */
  poolList: publicProcedure.query(async () => {
    try {
      return await acpPoolList()
    } catch (err) {
      throw new Error(String(err))
    }
  }),

  /**
   * List all active ACP agent sessions with status
   * Returns session info including agent name, status, and metadata
   */
  listAgentSessions: publicProcedure.query(async () => {
    try {
      const sessions = await acpListAgentSessions()
      return sessions
    } catch (err) {
      throw new Error(String(err))
    }
  }),
})

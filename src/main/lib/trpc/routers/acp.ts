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
        chunks.push({ type: "text-delta", textDelta: content })
      } else if (content?.text) {
        chunks.push({ type: "text-delta", textDelta: content.text })
      }
      break
    }

    case "agent_thought_chunk": {
      const content = data?.content || data?.ContentBlock || data
      const text = typeof content === "string" ? content : content?.text || ""
      if (text) {
        chunks.push({ type: "reasoning-delta", textDelta: text })
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
        usage: {
          inputTokens: usage?.input_tokens || usage?.inputTokens || 0,
          outputTokens: usage?.output_tokens || usage?.outputTokens || 0,
        },
      })
      break
    }

    case "closed": {
      chunks.push({ type: "finish" })
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
        mode: z.enum(["plan", "agent"]).default("agent"),
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

        const start = async () => {
          let acpSessionId: string

          try {
            if (sessionId) {
              try {
                acpSessionId = await acpResumeSession(agent, sessionId, cwd)
              } catch {
                acpSessionId = await acpCreateSession(agent, cwd)
              }
            } else {
              acpSessionId = await acpCreateSession(agent, cwd)
            }

            sessionMap.set(subChatId, acpSessionId)
            acpSaveSessionMeta(acpSessionId, agent, cwd)
            await acpSendPrompt(acpSessionId, prompt)

            console.log(`[ACP] Session ready: ${acpSessionId}`)
          } catch (err) {
            console.error("[ACP] Failed to start session:", err)
            emit.next({ type: "error", errorText: `Failed to start ACP session: ${err}` })
            emit.complete()
            return
          }

          // Poll events
          const timer = setInterval(() => {
            if (cancelled) return

            try {
              const events = acpPollEvents()
              const acpSessionId = sessionMap.get(subChatId)
              if (!acpSessionId) return

              for (const event of events) {
                if (event.sessionId !== acpSessionId) continue

                const chunks = translateEvent(event)
                for (const chunk of chunks) {
                  // Update permission map with subChatId
                  if (chunk.type === "ask-user-question") {
                    const mapping = permissionMap.get(chunk.toolUseId)
                    if (mapping) mapping.subChatId = subChatId
                  }

                  // Accumulate text for DAG detection
                  if (chunk.type === "text-delta" && chunk.textDelta) {
                    dagDetector.appendChunk(acpSessionId, chunk.textDelta)
                  }

                  emit.next(chunk)

                  if (chunk.type === "finish") {
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
              emit.next({ type: "error", errorText: String(err) })
              clearInterval(timer)
              activePollers.delete(subChatId)
              emit.complete()
            }
          }, POLL_INTERVAL)

          activePollers.set(subChatId, timer)
        }

        start()

        return () => {
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
   * Set session mode (e.g., "plan", "agent")
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
})

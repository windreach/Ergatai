import { create } from "zustand"
import { trpc } from "../../../lib/trpc"
import { useAgentSubChatStore } from "./sub-chat-store"
import type { AgentSession, AgentSessionStatus, AgentRuntime } from "../types/agent-session"

/**
 * Agent session sync state
 */
interface AgentSessionSyncState {
  /** Active agent sessions keyed by subChatId */
  sessions: Map<string, AgentSession>

  /** Whether sync is active */
  isSyncing: boolean

  /** Last sync timestamp */
  lastSyncAt?: number

  /** Sync error */
  error?: string

  /** Actions */
  startSync: () => void
  stopSync: () => void
  syncSessions: () => Promise<void>
  bindSession: (subChatId: string, acpSessionId: string, runtime?: AgentRuntime) => void
  unbindSession: (subChatId: string) => void
  updateSessionStatus: (subChatId: string, status: AgentSessionStatus) => void
  detectOrphans: () => void
}

/**
 * Store for syncing frontend sub-chats with backend ACP sessions
 */
export const useAgentSessionSyncStore = create<AgentSessionSyncState>((set, get) => ({
  sessions: new Map(),
  isSyncing: false,
  lastSyncAt: undefined,
  error: undefined,

  startSync: () => {
    set({ isSyncing: true, error: undefined })
    // Initial sync
    get().syncSessions()
  },

  stopSync: () => {
    set({ isSyncing: false })
  },

  syncSessions: async () => {
    const { sessions, isSyncing } = get()
    if (!isSyncing) return

    try {
      // Fetch all active ACP sessions from backend
      const backendSessions = await trpc.acp.listAgentSessions.query()

      const subChatStore = useAgentSubChatStore.getState()
      const allSubChats = subChatStore.allSubChats

      // Build updated session map
      const updatedSessions = new Map<string, AgentSession>()

      for (const subChat of allSubChats) {
        // Find matching backend session
        const backendSession = backendSessions.find(
          (s) => s.sessionId === subChat.acpSessionId
        )

        if (backendSession) {
          // Session exists in backend - update status
          updatedSessions.set(subChat.id, {
            subChatId: subChat.id,
            chatId: subChatStore.chatId || "",
            acpSessionId: subChat.acpSessionId,
            agentRuntime: (subChat.runtimeId as AgentRuntime) || "claude",
            status: mapBackendStatus(backendSession.status),
            cwd: backendSession.cwd,
            createdAt: backendSession.createdAt,
            updatedAt: backendSession.updatedAt,
            fileTokenId: subChat.fileTokenId,
            tokenExpiresAt: subChat.tokenExpiresAt,
            lastHeartbeatAt: subChat.lastHeartbeatAt,
            isLegacy: false,
          })
        } else if (subChat.acpSessionId) {
          // Session was in backend but now gone - mark as disconnected
          updatedSessions.set(subChat.id, {
            subChatId: subChat.id,
            chatId: subChatStore.chatId || "",
            acpSessionId: subChat.acpSessionId,
            agentRuntime: (subChat.runtimeId as AgentRuntime) || "claude",
            status: "disconnected",
            isLegacy: false,
          })
        }
        // If no acpSessionId, skip (not yet created or legacy)
      }

      set({
        sessions: updatedSessions,
        lastSyncAt: Date.now(),
        error: undefined,
      })

      // Detect orphaned backend sessions
      get().detectOrphans()
    } catch (err) {
      console.error("[AgentSessionSync] Sync failed:", err)
      set({
        error: err instanceof Error ? err.message : String(err),
      })
    }
  },

  bindSession: (subChatId, acpSessionId, runtime = "claude") => {
    const subChatStore = useAgentSubChatStore.getState()

    // Update sub-chat store
    subChatStore.updateSubChatAcpSession(subChatId, acpSessionId)

    // Update sync store
    const { sessions } = get()
    const existing = sessions.get(subChatId)
    if (existing) {
      sessions.set(subChatId, {
        ...existing,
        acpSessionId,
        agentRuntime: runtime,
        status: "idle",
        isLegacy: false,
      })
    } else {
      sessions.set(subChatId, {
        subChatId,
        chatId: subChatStore.chatId || "",
        acpSessionId,
        agentRuntime: runtime,
        status: "idle",
        isLegacy: false,
      })
    }

    set({ sessions: new Map(sessions) })
  },

  unbindSession: (subChatId) => {
    const { sessions } = get()
    sessions.delete(subChatId)
    set({ sessions: new Map(sessions) })
  },

  updateSessionStatus: (subChatId, status) => {
    const { sessions } = get()
    const session = sessions.get(subChatId)
    if (session) {
      sessions.set(subChatId, { ...session, status })
      set({ sessions: new Map(sessions) })
    }
  },

  detectOrphans: () => {
    // Orphan detection: backend sessions that don't map to any sub-chat
    // This is informational - we log but don't auto-clean
    const { sessions } = get()
    const subChatStore = useAgentSubChatStore.getState()
    const allSubChatIds = new Set(subChatStore.allSubChats.map((sc) => sc.id))

    const orphans: string[] = []
    for (const [subChatId, session] of sessions) {
      if (!allSubChatIds.has(subChatId)) {
        orphans.push(session.acpSessionId || subChatId)
      }
    }

    if (orphans.length > 0) {
      console.warn(
        `[AgentSessionSync] Detected ${orphans.length} orphaned ACP sessions:`,
        orphans
      )
    }
  },
}))

/**
 * Map backend status string to frontend AgentSessionStatus
 */
function mapBackendStatus(status: string): AgentSessionStatus {
  switch (status.toLowerCase()) {
    case "running":
    case "active":
      return "running"
    case "idle":
      return "idle"
    case "waiting_approval":
    case "waiting":
      return "waiting_approval"
    case "error":
    case "failed":
      return "error"
    default:
      return "idle"
  }
}

/**
 * Hook to access agent session sync state
 */
export function useAgentSession(subChatId?: string) {
  const sessions = useAgentSessionSyncStore((s) => s.sessions)
  const syncSessions = useAgentSessionSyncStore((s) => s.syncSessions)

  if (subChatId) {
    return {
      session: sessions.get(subChatId),
      refresh: syncSessions,
    }
  }

  return {
    sessions: Array.from(sessions.values()),
    refresh: syncSessions,
  }
}

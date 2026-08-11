/**
 * Agent Session Types
 *
 * Bridges frontend sub-chat concept with backend ACP agent session.
 * Provides unified type for tracking agent state across the application.
 */

/**
 * Agent session status from ACP backend
 */
export type AgentSessionStatus =
  | "idle"
  | "running"
  | "waiting_approval"
  | "error"
  | "disconnected"

/**
 * Agent runtime identifier (matches backend runtimeId)
 */
export type AgentRuntime = "claude" | "goose" | "codex" | string

/**
 * Agent session information from ACP backend
 * Maps to Rust's AgentSessionInfo struct
 */
export interface AgentSessionInfo {
  sessionId: string
  agentName: string
  cwd: string
  status: string
  createdAt?: string
  updatedAt?: string
}

/**
 * Bridged agent session - connects frontend sub-chat with backend ACP session
 */
export interface AgentSession {
  /** Frontend sub-chat ID (tab identifier) */
  subChatId: string

  /** Parent chat ID */
  chatId: string

  /** Backend ACP session ID (Rust side) */
  acpSessionId?: string

  /** Agent runtime (claude, goose, codex, etc.) */
  agentRuntime: AgentRuntime

  /** Current session status */
  status: AgentSessionStatus

  /** File access token ID (if acquired) */
  fileTokenId?: string

  /** Token expiration timestamp (ms) */
  tokenExpiresAt?: number

  /** Last heartbeat timestamp (ms) */
  lastHeartbeatAt?: number

  /** Working directory */
  cwd?: string

  /** Creation timestamp */
  createdAt?: string

  /** Last update timestamp */
  updatedAt?: string

  /** Whether this is a legacy sub-chat (pre-ACP integration) */
  isLegacy?: boolean
}

/**
 * Agent session creation parameters
 */
export interface CreateAgentSessionParams {
  chatId: string
  subChatId: string
  agentRuntime?: AgentRuntime
  cwd?: string
}

/**
 * Agent session update parameters
 */
export interface UpdateAgentSessionParams {
  subChatId: string
  acpSessionId?: string
  status?: AgentSessionStatus
  fileTokenId?: string
  tokenExpiresAt?: number
  lastHeartbeatAt?: number
}

/**
 * Helper to convert backend AgentSessionInfo to frontend AgentSession
 */
export function fromAgentSessionInfo(
  info: AgentSessionInfo,
  subChatId: string,
  chatId: string
): AgentSession {
  return {
    subChatId,
    chatId,
    acpSessionId: info.sessionId,
    agentRuntime: "claude", // Default, should be overridden from sub-chat meta
    status: mapBackendStatus(info.status),
    cwd: info.cwd,
    createdAt: info.createdAt,
    updatedAt: info.updatedAt,
  }
}

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

import { cn } from "../../../lib/utils"
import type { AgentRuntime, AgentSessionStatus } from "../types/agent-session"

interface RuntimeIndicatorProps {
  runtime?: AgentRuntime
  status?: AgentSessionStatus
  className?: string
  showLabel?: boolean
}

/**
 * Get runtime display name
 */
function getRuntimeLabel(runtime?: AgentRuntime): string {
  switch (runtime?.toLowerCase()) {
    case "claude":
      return "Claude"
    case "goose":
      return "Goose"
    case "codex":
      return "Codex"
    default:
      return runtime || "Agent"
  }
}

/**
 * Get status color class
 */
function getStatusColor(status?: AgentSessionStatus): string {
  switch (status) {
    case "running":
      return "bg-green-500"
    case "idle":
      return "bg-gray-400"
    case "waiting_approval":
      return "bg-amber-500"
    case "error":
      return "bg-red-500"
    case "disconnected":
      return "bg-gray-500"
    default:
      return "bg-gray-400"
  }
}

/**
 * Small runtime indicator badge
 * Shows a colored dot for status + runtime label
 */
export function RuntimeIndicator({
  runtime,
  status,
  className,
  showLabel = false,
}: RuntimeIndicatorProps) {
  const statusColor = getStatusColor(status)
  const label = getRuntimeLabel(runtime)

  return (
    <div
      className={cn(
        "flex items-center gap-1 text-xs",
        className
      )}
      title={`${label}${status ? ` - ${status}` : ""}`}
    >
      {/* Status dot */}
      <div className={cn("w-1.5 h-1.5 rounded-full", statusColor)} />

      {/* Optional label */}
      {showLabel && (
        <span className="text-muted-foreground">{label}</span>
      )}
    </div>
  )
}

/**
 * Legacy badge for pre-ACP sub-chats
 */
export function LegacyBadge({ className }: { className?: string }) {
  return (
    <span
      className={cn(
        "text-[10px] px-1 py-0.5 rounded bg-muted text-muted-foreground",
        className
      )}
      title="Legacy sub-chat (pre-ACP integration)"
    >
      Legacy
    </span>
  )
}

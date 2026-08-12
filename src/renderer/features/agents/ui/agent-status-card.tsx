import { cn } from "../../../lib/utils"
import { Badge } from "../../../components/ui/badge"
import { Bot, Clock, Folder, Hash } from "lucide-react"
import type { AgentSessionInfo } from "../hooks/use-agent-status"

interface AgentStatusCardProps {
  session: AgentSessionInfo
  className?: string
}

/**
 * Get status badge variant based on session status
 */
function getStatusVariant(status: string) {
  switch (status.toLowerCase()) {
    case "running":
    case "active":
      return "default"
    case "idle":
      return "secondary"
    case "waiting_approval":
    case "waiting":
      return "destructive"
    case "error":
    case "failed":
      return "destructive"
    default:
      return "outline"
  }
}

/**
 * Get status label for display
 */
function getStatusLabel(status: string) {
  switch (status.toLowerCase()) {
    case "running":
    case "active":
      return "运行中"
    case "idle":
      return "空闲"
    case "waiting_approval":
    case "waiting":
      return "等待审批"
    case "error":
    case "failed":
      return "错误"
    default:
      return status
  }
}

/**
 * Single agent session card displaying status and metadata
 */
export function AgentStatusCard({ session, className }: AgentStatusCardProps) {
  const statusVariant = getStatusVariant(session.status)
  const statusLabel = getStatusLabel(session.status)

  return (
    <div
      className={cn(
        "rounded-lg border bg-card p-4 shadow-sm hover:shadow-md transition-shadow",
        className
      )}
    >
      {/* Header: Agent name + status badge */}
      <div className="flex items-center justify-between mb-3">
        <div className="flex items-center gap-2">
          <Bot className="h-5 w-5 text-primary" />
          <span className="font-semibold text-sm">{session.agentName}</span>
        </div>
        <Badge variant={statusVariant} className="text-xs">
          {statusLabel}
        </Badge>
      </div>

      {/* Session ID */}
      <div className="flex items-center gap-2 text-xs text-muted-foreground mb-2">
        <Hash className="h-3 w-3" />
        <span className="truncate font-mono">{session.sessionId.slice(0, 12)}...</span>
      </div>

      {/* Working directory */}
      <div className="flex items-center gap-2 text-xs text-muted-foreground mb-2">
        <Folder className="h-3 w-3" />
        <span className="truncate">{session.cwd}</span>
      </div>

      {/* Last updated */}
      {session.updatedAt && (
        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          <Clock className="h-3 w-3" />
          <span>{new Date(session.updatedAt).toLocaleTimeString()}</span>
        </div>
      )}
    </div>
  )
}

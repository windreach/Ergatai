import { useAgentStatus } from "../hooks/use-agent-status"
import { AgentStatusCard } from "./agent-status-card"
import { AlertCircle, Bot, RefreshCw } from "lucide-react"
import { Button } from "../../../components/ui/button"
import { cn } from "../../../lib/utils"

interface AgentStatusDashboardProps {
  className?: string
}

/**
 * Dashboard showing all active ACP agent sessions
 * Polls for updates every 2 seconds
 */
export function AgentStatusDashboard({ className }: AgentStatusDashboardProps) {
  const { sessions, isLoading, error, refetch } = useAgentStatus(2000)

  return (
    <div className={cn("flex flex-col h-full", className)}>
      {/* Header */}
      <div className="flex items-center justify-between px-4 py-3 border-b">
        <div className="flex items-center gap-2">
          <Bot className="h-5 w-5 text-primary" />
          <h2 className="font-semibold">Agent 状态</h2>
          <span className="text-xs text-muted-foreground">
            ({sessions.length} 个活跃)
          </span>
        </div>
        <Button
          variant="ghost"
          size="sm"
          onClick={() => refetch()}
          disabled={isLoading}
          className="h-8 w-8 p-0"
        >
          <RefreshCw className={cn("h-4 w-4", isLoading && "animate-spin")} />
        </Button>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-y-auto p-4">
        {error ? (
          <div className="flex flex-col items-center justify-center h-full text-center">
            <AlertCircle className="h-12 w-12 text-destructive mb-4" />
            <p className="text-sm font-medium mb-1">加载失败</p>
            <p className="text-xs text-muted-foreground mb-4">
              {error.message || "无法获取 Agent 状态"}
            </p>
            <Button variant="outline" size="sm" onClick={() => refetch()}>
              重试
            </Button>
          </div>
        ) : isLoading && sessions.length === 0 ? (
          <div className="flex items-center justify-center h-full">
            <RefreshCw className="h-8 w-8 animate-spin text-muted-foreground" />
          </div>
        ) : sessions.length === 0 ? (
          <div className="flex flex-col items-center justify-center h-full text-center">
            <Bot className="h-12 w-12 text-muted-foreground mb-4" />
            <p className="text-sm font-medium mb-1">无活跃 Agent</p>
            <p className="text-xs text-muted-foreground">
              当前没有运行中的 Agent 会话
            </p>
          </div>
        ) : (
          <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
            {sessions.map((session) => (
              <AgentStatusCard key={session.sessionId} session={session} />
            ))}
          </div>
        )}
      </div>
    </div>
  )
}

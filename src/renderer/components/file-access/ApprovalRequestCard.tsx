import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"
import { Lock, User, Folder, FileText } from "lucide-react"

export interface ApprovalRequest {
  id: string
  agentId: string
  sessionId: string
  scope: string
  filePaths: string[]
  mode: "ADMIN"
  reason: string
  createdAt: number
}

interface ApprovalRequestCardProps {
  request: ApprovalRequest
  onApprove: (reason?: string) => void
  onReject: (reason?: string) => void
  isPending?: boolean
}

export function ApprovalRequestCard({
  request,
  onApprove,
  onReject,
  isPending = false,
}: ApprovalRequestCardProps) {
  return (
    <div
      className={cn(
        "p-4 border-b hover:bg-accent/50 transition-colors",
        isPending && "opacity-50 pointer-events-none"
      )}
    >
      <div className="flex items-start gap-3">
        {/* Icon */}
        <div className="flex-shrink-0 mt-1">
          <div className="w-8 h-8 rounded-full bg-destructive/10 flex items-center justify-center">
            <Lock className="w-4 h-4 text-destructive" />
          </div>
        </div>

        {/* Content */}
        <div className="flex-1 min-w-0">
          {/* Header */}
          <div className="flex items-center gap-2 flex-wrap">
            <Badge variant="destructive" className="text-xs">
              ADMIN
            </Badge>
            <span className="text-sm font-medium flex items-center gap-1">
              <User className="w-3 h-3" />
              {request.agentId}
            </span>
            <span className="text-xs text-muted-foreground">
              {new Date(request.createdAt).toLocaleTimeString()}
            </span>
          </div>

          {/* Details */}
          <div className="mt-2 space-y-1.5 text-sm text-muted-foreground">
            <div className="flex items-center gap-2">
              <Folder className="w-3.5 h-3.5 flex-shrink-0" />
              <span className="truncate">
                Scope: <code className="text-xs bg-muted px-1 py-0.5 rounded">{request.scope}</code>
              </span>
            </div>
            <div className="flex items-center gap-2">
              <FileText className="w-3.5 h-3.5 flex-shrink-0" />
              <span>{request.filePaths.length} 个文件</span>
            </div>
            {request.reason && (
              <div className="mt-2 p-2 bg-muted/50 rounded text-xs">
                <span className="font-medium">原因：</span>
                {request.reason}
              </div>
            )}
          </div>

          {/* Actions */}
          <div className="mt-3 flex gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() => onReject("Rejected by user")}
              disabled={isPending}
              className="text-xs"
            >
              拒绝
            </Button>
            <Button
              size="sm"
              onClick={() => onApprove("Approved by user")}
              disabled={isPending}
              className="text-xs"
            >
              {isPending ? "处理中..." : "批准"}
            </Button>
          </div>
        </div>
      </div>
    </div>
  )
}

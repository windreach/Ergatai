import { useAtom } from "jotai"
import { ResizableBottomPanel } from "@/components/ui/resizable-bottom-panel"
import { Button } from "@/components/ui/button"
import { X, ShieldAlert } from "lucide-react"
import { ApprovalRequestCard, type ApprovalRequest } from "./ApprovalRequestCard"
import { atom } from "jotai"
import { useState } from "react"
import { trpc } from "@/lib/trpc"

// 审批面板高度 atom
export const approvalBottomHeightAtom = atom(250)

interface ApprovalBottomPanelProps {
  isOpen: boolean
  onClose: () => void
  chatId: string
}

export function ApprovalBottomPanel({
  isOpen,
  onClose,
  chatId,
}: ApprovalBottomPanelProps) {
  const [processingIds, setProcessingIds] = useState<Set<string>>(new Set())

  // 获取审批请求列表
  const { data: requests, refetch } = trpc.fileAccess.getApprovalRequests.useQuery(
    { chatId },
    {
      refetchInterval: 2000, // 每 2 秒轮询一次
      enabled: isOpen,
    }
  )

  // 批准请求
  const approveMutation = trpc.fileAccess.approveRequest.useMutation({
    onSuccess: () => {
      refetch()
    },
  })

  // 拒绝请求
  const rejectMutation = trpc.fileAccess.rejectRequest.useMutation({
    onSuccess: () => {
      refetch()
    },
  })

  const handleApprove = async (requestId: string, reason?: string) => {
    setProcessingIds((prev) => new Set(prev).add(requestId))
    try {
      await approveMutation.mutateAsync({ requestId, reason })
    } finally {
      setProcessingIds((prev) => {
        const next = new Set(prev)
        next.delete(requestId)
        return next
      })
    }
  }

  const handleReject = async (requestId: string, reason?: string) => {
    setProcessingIds((prev) => new Set(prev).add(requestId))
    try {
      await rejectMutation.mutateAsync({ requestId, reason })
    } finally {
      setProcessingIds((prev) => {
        const next = new Set(prev)
        next.delete(requestId)
        return next
      })
    }
  }

  const pendingCount = requests?.length ?? 0

  return (
    <ResizableBottomPanel
      isOpen={isOpen}
      onClose={onClose}
      heightAtom={approvalBottomHeightAtom}
      minHeight={200}
      maxHeight={500}
      showResizeTooltip={true}
      closeHotkey="Esc"
      className="bg-background border-t"
      style={{ borderTopWidth: "0.5px" }}
    >
      <div className="h-full flex flex-col">
        {/* Header */}
        <div className="px-4 py-3 border-b flex items-center justify-between flex-shrink-0">
          <div className="flex items-center gap-2">
            <ShieldAlert className="w-4 h-4 text-destructive" />
            <h3 className="text-sm font-medium">权限审批</h3>
            {pendingCount > 0 && (
              <span className="text-xs text-muted-foreground">
                ({pendingCount} 个待处理)
              </span>
            )}
          </div>
          <Button
            variant="ghost"
            size="sm"
            onClick={onClose}
            className="h-6 w-6 p-0"
          >
            <X className="h-4 w-4" />
          </Button>
        </div>

        {/* Request List */}
        <div className="flex-1 overflow-y-auto">
          {!requests || requests.length === 0 ? (
            <div className="h-full flex items-center justify-center text-sm text-muted-foreground">
              暂无待审批请求
            </div>
          ) : (
            requests.map((request: ApprovalRequest) => (
              <ApprovalRequestCard
                key={request.id}
                request={request}
                isPending={processingIds.has(request.id)}
                onApprove={(reason) => handleApprove(request.id, reason)}
                onReject={(reason) => handleReject(request.id, reason)}
              />
            ))
          )}
        </div>
      </div>
    </ResizableBottomPanel>
  )
}

import { z } from "zod"
import { router, publicProcedure } from "../index"
import { fileAccessRespondApproval } from "@/file-access-napi"

// 待审批请求（从 Rust 端接收）
export interface PendingApprovalRequest {
  id: string
  agentId: string
  sessionId: string
  scope: string
  filePaths: string[]
  mode: "ADMIN"
  reason: string
  createdAt: number
}

// 全局待审批请求存储（从 SessionEvent 中收集）
const pendingApprovals = new Map<string, PendingApprovalRequest>()

export const fileAccessRouter = router({
  /**
   * 获取待审批请求列表
   */
  getApprovalRequests: publicProcedure
    .input(
      z.object({
        chatId: z.string(),
      })
    )
    .query(async ({ input }) => {
      // 返回所有待审批请求
      // 未来可以根据 chatId 过滤
      return Array.from(pendingApprovals.values())
    }),

  /**
   * 批准权限请求
   */
  approveRequest: publicProcedure
    .input(
      z.object({
        requestId: z.string(),
        reason: z.string().optional(),
      })
    )
    .mutation(async ({ input }) => {
      try {
        // 调用 Rust NAPI 函数发送审批响应
        await fileAccessRespondApproval(
          input.requestId,
          true, // approved
          "user",
          input.reason ?? "Approved by user"
        )

        console.log(
          `[FileAccess] Request approved: ${input.requestId}`,
          input.reason
        )
      } catch (error) {
        console.error("[FileAccess] Failed to approve request:", error)
        throw error
      }

      // 从待审批列表中移除
      pendingApprovals.delete(input.requestId)

      return { success: true }
    }),

  /**
   * 拒绝权限请求
   */
  rejectRequest: publicProcedure
    .input(
      z.object({
        requestId: z.string(),
        reason: z.string().optional(),
      })
    )
    .mutation(async ({ input }) => {
      try {
        // 调用 Rust NAPI 函数发送审批响应
        await fileAccessRespondApproval(
          input.requestId,
          false, // rejected
          "user",
          input.reason ?? "Rejected by user"
        )

        console.log(
          `[FileAccess] Request rejected: ${input.requestId}`,
          input.reason
        )
      } catch (error) {
        console.error("[FileAccess] Failed to reject request:", error)
        throw error
      }

      // 从待审批列表中移除
      pendingApprovals.delete(input.requestId)

      return { success: true }
    }),
})

/**
 * 添加待审批请求（从 SessionEvent 调用）
 */
export function addPendingApproval(request: PendingApprovalRequest): void {
  pendingApprovals.set(request.id, request)
}

/**
 * 移除待审批请求
 */
export function removePendingApproval(requestId: string): void {
  pendingApprovals.delete(requestId)
}

/**
 * 获取待审批请求数量
 */
export function getPendingApprovalCount(): number {
  return pendingApprovals.size
}

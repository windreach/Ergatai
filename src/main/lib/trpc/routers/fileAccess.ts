import { z } from "zod"
import { router, publicProcedure } from "../index"
import {
  fileAccessRespondApproval,
  fileAccessInit,
  fileAccessRegisterSystemToken,
  fileAccessRequestToken,
  fileAccessAcquireLock,
  fileAccessReleaseLock,
  fileAccessUpgradeLock,
  fileAccessDowngradeLock,
  fileAccessReadLatest,
  fileAccessCreateSnapshot,
  fileAccessMarkBusy,
  fileAccessClearBusy,
  fileAccessShutdown,
  fileAccessIsSensitivePath,
  fileAccessIsForbiddenPath,
  fileAccessReloadConfig,
} from "@/file-access-napi"

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

  // ===== Phase 1: File Access Control NAPI Wrappers =====

  /**
   * 初始化文件访问控制系统
   */
  init: publicProcedure
    .input(
      z.object({
        projectId: z.string(),
        projectRoot: z.string(),
      })
    )
    .mutation(async ({ input }) => {
      await fileAccessInit(input.projectId, input.projectRoot)
      console.log(`[FileAccess] Initialized for project: ${input.projectId}`)
      return { success: true }
    }),

  /**
   * 注册系统 token（Agent 准入凭证）
   */
  registerSystemToken: publicProcedure
    .input(
      z.object({
        projectId: z.string(),
        agentId: z.string(),
        sessionId: z.string(),
        projectRoot: z.string(),
        ttlSecs: z.number(),
        heartbeatIntervalSecs: z.number(),
      })
    )
    .mutation(async ({ input }) => {
      const tokenId = await fileAccessRegisterSystemToken(
        input.projectId,
        input.agentId,
        input.sessionId,
        input.projectRoot,
        input.ttlSecs,
        input.heartbeatIntervalSecs
      )
      return { tokenId }
    }),

  /**
   * 请求文件访问 token
   */
  requestToken: publicProcedure
    .input(
      z.object({
        projectId: z.string(),
        agentId: z.string(),
        sessionId: z.string(),
        scope: z.string(),
        mode: z.enum(["READ", "WRITE", "ADMIN"]),
        reason: z.string().nullable(),
        ttlSecs: z.number(),
        heartbeatIntervalSecs: z.number(),
      })
    )
    .mutation(async ({ input }) => {
      const tokenId = await fileAccessRequestToken(
        input.projectId,
        input.agentId,
        input.sessionId,
        input.scope,
        input.mode,
        input.reason,
        input.ttlSecs,
        input.heartbeatIntervalSecs
      )
      return { tokenId }
    }),

  /**
   * 获取文件锁
   */
  acquireLock: publicProcedure
    .input(
      z.object({
        projectId: z.string(),
        tokenId: z.string(),
        filePath: z.string(),
      })
    )
    .mutation(async ({ input }) => {
      await fileAccessAcquireLock(input.projectId, input.tokenId, input.filePath)
      return { success: true }
    }),

  /**
   * 释放文件锁
   */
  releaseLock: publicProcedure
    .input(
      z.object({
        projectId: z.string(),
        tokenId: z.string(),
        filePath: z.string(),
      })
    )
    .mutation(async ({ input }) => {
      await fileAccessReleaseLock(input.projectId, input.tokenId, input.filePath)
      return { success: true }
    }),

  /**
   * 升级锁（READ → WRITE）
   */
  upgradeLock: publicProcedure
    .input(
      z.object({
        projectId: z.string(),
        tokenId: z.string(),
        filePath: z.string(),
      })
    )
    .mutation(async ({ input }) => {
      await fileAccessUpgradeLock(input.projectId, input.tokenId, input.filePath)
      return { success: true }
    }),

  /**
   * 降级锁（WRITE → READ）
   */
  downgradeLock: publicProcedure
    .input(
      z.object({
        projectId: z.string(),
        tokenId: z.string(),
        filePath: z.string(),
      })
    )
    .mutation(async ({ input }) => {
      await fileAccessDowngradeLock(input.projectId, input.tokenId, input.filePath)
      return { success: true }
    }),

  /**
   * 读取文件最新内容（等待 WRITE 完成）
   */
  readLatest: publicProcedure
    .input(
      z.object({
        projectId: z.string(),
        filePath: z.string(),
      })
    )
    .query(async ({ input }) => {
      const buffer = await fileAccessReadLatest(input.projectId, input.filePath)
      return { content: buffer.toString("utf-8") }
    }),

  /**
   * 创建文件快照（Copy-on-Write）
   */
  createSnapshot: publicProcedure
    .input(
      z.object({
        projectId: z.string(),
        filePath: z.string(),
        agentId: z.string(),
      })
    )
    .mutation(async ({ input }) => {
      const gitHash = await fileAccessCreateSnapshot(
        input.projectId,
        input.filePath,
        input.agentId
      )
      return { gitHash }
    }),

  /**
   * 标记 session 为忙碌状态（延长心跳超时）
   */
  markBusy: publicProcedure
    .input(
      z.object({
        projectId: z.string(),
        sessionId: z.string(),
        durationSecs: z.number(),
      })
    )
    .mutation(async ({ input }) => {
      await fileAccessMarkBusy(input.projectId, input.sessionId, input.durationSecs)
      return { success: true }
    }),

  /**
   * 清除 session 忙碌状态
   */
  clearBusy: publicProcedure
    .input(
      z.object({
        projectId: z.string(),
        sessionId: z.string(),
      })
    )
    .mutation(async ({ input }) => {
      await fileAccessClearBusy(input.projectId, input.sessionId)
      return { success: true }
    }),

  /**
   * 关闭文件访问控制系统
   */
  shutdown: publicProcedure
    .input(
      z.object({
        projectId: z.string(),
      })
    )
    .mutation(async ({ input }) => {
      await fileAccessShutdown(input.projectId)
      console.log(`[FileAccess] Shutdown for project: ${input.projectId}`)
      return { success: true }
    }),

  /**
   * 检查路径是否为敏感路径（需要 ADMIN 权限）
   */
  isSensitivePath: publicProcedure
    .input(
      z.object({
        projectId: z.string(),
        filePath: z.string(),
      })
    )
    .query(async ({ input }) => {
      const isSensitive = await fileAccessIsSensitivePath(input.projectId, input.filePath)
      return { isSensitive }
    }),

  /**
   * 检查路径是否为禁止路径
   */
  isForbiddenPath: publicProcedure
    .input(
      z.object({
        projectId: z.string(),
        filePath: z.string(),
      })
    )
    .query(async ({ input }) => {
      const isForbidden = await fileAccessIsForbiddenPath(input.projectId, input.filePath)
      return { isForbidden }
    }),

  /**
   * 重新加载项目配置
   */
  reloadConfig: publicProcedure
    .input(
      z.object({
        projectId: z.string(),
      })
    )
    .mutation(async ({ input }) => {
      await fileAccessReloadConfig(input.projectId)
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

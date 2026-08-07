import { z } from "zod"
import { publicProcedure, router } from "../index"

/**
 * Stub router — Codex replaced by ACP protocol.
 * Codex is now an ACP agent like any other.
 */
export const codexRouter = router({
  getIntegration: publicProcedure.query(() => null),
  startLogin: publicProcedure.mutation(() => ({ success: true })),
  cancelLogin: publicProcedure.mutation(() => ({ success: true })),
  logout: publicProcedure.mutation(() => ({ success: true })),
  addMcpServer: publicProcedure.input(z.any()).mutation(() => ({ success: true })),
  getAllMcpConfig: publicProcedure.query(() => []),
  refreshMcpConfig: publicProcedure.mutation(() => ({ success: true })),
  startMcpOAuth: publicProcedure.input(z.any()).mutation(() => ({ success: true })),
  logoutMcpServer: publicProcedure.input(z.any()).mutation(() => ({ success: true })),
  removeMcpServer: publicProcedure.input(z.any()).mutation(() => ({ success: true })),
})

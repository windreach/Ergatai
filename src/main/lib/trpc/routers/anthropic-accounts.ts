import { z } from "zod"
import { publicProcedure, router } from "../index"

/**
 * Stub router — Anthropic accounts replaced by ACP agent auth.
 */
export const anthropicAccountsRouter = router({
  getAccounts: publicProcedure.query(() => []),
  getActiveAccount: publicProcedure.query(() => null),
  setActiveAccount: publicProcedure.input(z.any()).mutation(() => ({ success: true })),
  logout: publicProcedure.mutation(() => ({ success: true })),
})

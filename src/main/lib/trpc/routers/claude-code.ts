import { publicProcedure, router } from "../index"
import { z } from "zod"

/**
 * Stub router — Claude Code login replaced by ACP agent auth.
 * Frontend calls these during onboarding; they return safe defaults.
 */
export const claudeCodeRouter = router({
  hasExistingCliConfig: publicProcedure.query(() => false),
  startAuth: publicProcedure.mutation(() => ({ success: true })),
  submitCode: publicProcedure.input(z.any()).mutation(() => ({ success: true })),
  openOAuthUrl: publicProcedure.input(z.any()).mutation(() => ({ success: true })),
  pollStatus: publicProcedure.query(() => ({ status: "not_needed" as const })),
  logout: publicProcedure.mutation(() => ({ success: true })),
})

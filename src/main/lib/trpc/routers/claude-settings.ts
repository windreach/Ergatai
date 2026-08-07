import { z } from "zod"
import { publicProcedure, router } from "../index"

/**
 * Stub router — Claude settings replaced by ACP agent config.
 * Some functions are still imported by other modules.
 */

// Stub — no enabled plugins in ACP mode
export const getEnabledPlugins = async () => []

export const claudeSettingsRouter = router({
  getSettings: publicProcedure.query(() => ({})),
  updateSettings: publicProcedure.input(z.any()).mutation(() => ({ success: true })),
})

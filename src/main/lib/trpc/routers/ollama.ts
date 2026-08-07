import { publicProcedure, router } from "../index"

/**
 * Stub router — Ollama replaced by ACP agents.
 * Ollama can be accessed as a regular ACP agent if needed.
 */
export const ollamaRouter = router({
  getStatus: publicProcedure.query(() => ({
    available: false,
    models: [],
  })),
  listModels: publicProcedure.query(() => []),
})

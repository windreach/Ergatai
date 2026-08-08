import { z } from "zod"
import { join } from "path"
import { app } from "electron"
import { router, publicProcedure } from "../index"

function loadNativeBinding(): any {
  const appRoot = app.getAppPath()
  const candidates = [
    join(appRoot, "src/native-binding"),
    join(appRoot, "out/main/native-binding"),
    join(appRoot, "native-binding"),
  ]
  for (const p of candidates) {
    try {
      return require(p)
    } catch {}
  }
  throw new Error("Cannot find native-binding module")
}

const nativeBinding = loadNativeBinding()

/**
 * ACP Agent Management Router
 *
 * Manages ACP (Agent Client Protocol) agents with:
 * - Discovery of installed agents (builtin + custom)
 * - Global configuration (provider, model, env vars)
 * - Custom harness definitions
 */
export const agentsRouter = router({
  /**
   * List all available ACP runtimes (builtin + custom).
   *
   * Returns catalog entries with availability status, auth status, and install hints.
   */
  listRuntimes: publicProcedure.query(async () => {
    return nativeBinding.discoverAcpRuntimes()
  }),

  /**
   * Get the global agent configuration.
   *
   * Returns env_vars, provider, model, and preferred_runtime.
   */
  getGlobalConfig: publicProcedure.query(async () => {
    return nativeBinding.getGlobalAgentConfig()
  }),

  /**
   * Save the global agent configuration.
   *
   * Validates and persists to disk with restricted permissions (0o600).
   */
  setGlobalConfig: publicProcedure
    .input(
      z.object({
        env_vars: z.record(z.string()).optional(),
        provider: z.string().nullable().optional(),
        model: z.string().nullable().optional(),
        preferred_runtime: z.string().nullable().optional(),
      }),
    )
    .mutation(async ({ input }) => {
      const config = {
        env_vars: input.env_vars ?? {},
        provider: input.provider ?? null,
        model: input.model ?? null,
        preferred_runtime: input.preferred_runtime ?? null,
      }
      await nativeBinding.setGlobalAgentConfig(config)
      return { success: true }
    }),

  /**
   * List custom harness definitions.
   *
   * Filters discoverAcpRuntimes() by source === "custom".
   */
  listCustomHarnesses: publicProcedure.query(async () => {
    const runtimes = await nativeBinding.discoverAcpRuntimes()
    return runtimes.filter((r: any) => r.source === "custom")
  }),

  /**
   * Save a custom harness definition.
   *
   * Creates or updates a custom agent harness.
   */
  saveCustomHarness: publicProcedure
    .input(
      z.object({
        id: z.string().min(1),
        label: z.string().min(1),
        command: z.string().min(1),
        args: z.array(z.string()).optional(),
        env: z.record(z.string()).optional(),
        install_instructions_url: z.string().optional(),
        install_hint: z.string().optional(),
      }),
    )
    .mutation(async ({ input }) => {
      const harness = {
        id: input.id,
        label: input.label,
        command: input.command,
        args: input.args ?? [],
        env: input.env ?? {},
        install_instructions_url: input.install_instructions_url ?? "",
        install_hint: input.install_hint ?? "",
      }
      const entry = await nativeBinding.saveCustomHarness(harness)
      return { success: true, entry }
    }),

  /**
   * Delete a custom harness by id.
   */
  deleteCustomHarness: publicProcedure
    .input(z.string())
    .mutation(async ({ input }) => {
      await nativeBinding.deleteCustomHarness(input)
      return { success: true }
    }),

  /**
   * Install an ACP runtime by executing its predefined install command.
   *
   * Runs the install command (e.g., `npm install -g @block/goose`) via shell.
   * The command must be in the whitelist to prevent injection attacks.
   */
  installRuntime: publicProcedure
    .input(z.object({ runtimeId: z.string() }))
    .mutation(async ({ input }) => {
      const output = await nativeBinding.installAcpRuntime(input.runtimeId)
      return { success: true, output }
    }),
})

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
    const raw = await nativeBinding.discoverAcpRuntimes()
    // NAPI #[napi(object)] converts Rust snake_case → JS camelCase.
    // Normalize to snake_case for consistent frontend types.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any -- NAPI output casing varies by version
    return raw.map((r: any) => ({
      id: r.id,
      label: r.label,
      avatar_url: r.avatarUrl ?? r.avatar_url ?? "",
      availability: r.availability ?? "not_installed",
      command: r.command ?? null,
      binary_path: r.binaryPath ?? r.binary_path ?? null,
      install_hint: r.installHint ?? r.install_hint ?? "",
      install_instructions_url: r.installInstructionsUrl ?? r.install_instructions_url ?? "",
      has_install_command: r.hasInstallCommand ?? r.has_install_command ?? false,
      auth_status: r.authStatus ?? r.auth_status ?? "unknown",
      login_hint: r.loginHint ?? r.login_hint ?? null,
      source: r.source ?? "builtin",
    }))
  }),

  /**
   * Get the global agent configuration.
   *
   * Returns env_vars, provider, model, and preferred_runtime.
   */
  getGlobalConfig: publicProcedure.query(async () => {
    const raw = await nativeBinding.getGlobalAgentConfig()
    // NAPI #[napi(object)] converts Rust snake_case → JS camelCase on output.
    // Normalize back to snake_case so the rest of the stack is consistent.
    return {
      env_vars: raw.envVars ?? raw.env_vars ?? {},
      provider: raw.provider ?? null,
      model: raw.model ?? null,
      preferred_runtime: raw.preferredRuntime ?? raw.preferred_runtime ?? null,
    }
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

  /**
   * Get agent configuration by name.
   *
   * Returns the full agent config (command, args, env, model, etc.)
   */
  getAgentConfig: publicProcedure
    .input(z.object({ name: z.string() }))
    .query(async ({ input }) => {
      try {
        const config = await nativeBinding.getAgentConfig(input.name)
        return config
      } catch (error) {
        console.error(`[Agents] Failed to get config for ${input.name}:`, error)
        return null
      }
    }),

  /**
   * Save agent configuration.
   *
   * Creates or updates an agent config file.
   */
  saveAgentConfig: publicProcedure
    .input(
      z.object({
        name: z.string(),
        command: z.string(),
        args: z.array(z.string()).optional(),
        env: z.record(z.string()).optional(),
        display_name: z.string().nullable().optional(),
        model: z.string().nullable().optional(),
        provider: z.string().nullable().optional(),
        agent_type: z.string().nullable().optional(),
        base_url: z.string().nullable().optional(),
        api_key: z.string().nullable().optional(),
        proxy: z.string().nullable().optional(),
        persona: z.string().nullable().optional(),
        avatar: z.string().nullable().optional(),
      }),
    )
    .mutation(async ({ input }) => {
      const config = {
        name: input.name,
        command: input.command,
        args: input.args ?? [],
        env: input.env ?? {},
        display_name: input.display_name ?? null,
        model: input.model ?? null,
        provider: input.provider ?? null,
        agent_type: input.agent_type ?? null,
        base_url: input.base_url ?? null,
        api_key: input.api_key ?? null,
        proxy: input.proxy ?? null,
        persona: input.persona ?? null,
        avatar: input.avatar ?? null,
      }
      await nativeBinding.saveAgentConfig(config)
      return { success: true }
    }),
})

import { useState, useMemo } from "react"
import { trpc } from "../../../lib/trpc"
import { cn } from "../../../lib/utils"
import { Button } from "../../ui/button"
import {
  Check,
  ChevronDown,
  ExternalLink,
  RefreshCw,
} from "lucide-react"
import { toast } from "sonner"

// Types from Rust backend
interface AcpRuntimeCatalogEntry {
  id: string
  label: string
  avatar_url: string
  availability: "available" | "not_installed" | "auth_required"
  command: string | null
  binary_path: string | null
  install_hint: string
  install_instructions_url: string
  has_install_command: boolean
  auth_status: "logged_in" | "logged_out" | "not_applicable" | "unknown"
  login_hint: string | null
  source: "builtin" | "custom"
}

interface GlobalAgentConfig {
  env_vars: Record<string, string>
  provider: string | null
  model: string | null
  preferred_runtime: string | null
}

// Normalize NAPI enum output (camelCase) to frontend format (snake_case)
function normalizeAvailability(status: string): AcpRuntimeCatalogEntry["availability"] {
  if (status === "available" || status === "Available") return "available"
  if (status === "notInstalled" || status === "NotInstalled") return "not_installed"
  if (status === "authRequired" || status === "AuthRequired") return "auth_required"
  return "not_installed"
}

function normalizeAuthStatus(status: string): AcpRuntimeCatalogEntry["auth_status"] {
  if (status === "loggedIn" || status === "LoggedIn") return "logged_in"
  if (status === "loggedOut" || status === "LoggedOut") return "logged_out"
  if (status === "notApplicable" || status === "NotApplicable") return "not_applicable"
  return "unknown"
}

// Default agent pill button
function DefaultAgentPill({
  active,
  onClick,
  children,
}: {
  active: boolean
  onClick: () => void
  children: React.ReactNode
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        "inline-flex items-center gap-2 rounded-md border px-3 py-1.5 text-sm outline-none transition-colors",
        active
          ? "border-muted-foreground/40 bg-accent font-medium text-accent-foreground"
          : "border-border bg-background/50 text-muted-foreground hover:border-muted-foreground/35 hover:text-foreground"
      )}
    >
      {children}
    </button>
  )
}

// Agent icon (avatar or fallback letter)
function AgentIcon({
  runtime,
  size = 16,
}: {
  runtime: AcpRuntimeCatalogEntry
  size?: number
}) {
  if (runtime.avatar_url) {
    return (
      <img
        src={runtime.avatar_url}
        alt={runtime.label}
        width={size}
        height={size}
        className="rounded object-cover"
      />
    )
  }
  return (
    <div
      className="flex items-center justify-center rounded bg-muted font-medium text-muted-foreground"
      style={{ width: size, height: size, fontSize: size * 0.6 }}
    >
      {runtime.label[0]}
    </div>
  )
}

// Compact agent row for installed agents
function InstalledAgentRow({
  runtime,
  isDefault,
  onSetDefault,
  onInstall,
  isInstalling,
}: {
  runtime: AcpRuntimeCatalogEntry
  isDefault: boolean
  onSetDefault: () => void
  onInstall?: () => void
  isInstalling?: boolean
}) {
  const [cmdOpen, setCmdOpen] = useState(false)
  const availability = normalizeAvailability(runtime.availability as string)
  const authStatus = normalizeAuthStatus(runtime.auth_status as string)

  const isReady = availability === "available"
  const needsAuth = authStatus === "logged_out"

  return (
    <div className={cn("py-3", !isReady && "opacity-70")}>
      <div className="flex flex-wrap items-start gap-3">
        {/* Icon */}
        <div className="flex size-7 shrink-0 items-center justify-center rounded-md border border-border/50 bg-background/50">
          <AgentIcon runtime={runtime} size={16} />
        </div>

        {/* Label + command */}
        <div className="min-w-0 flex-1 sm:min-w-[12rem]">
          <div className="flex items-center gap-2">
            <span className="text-sm font-medium leading-none">{runtime.label}</span>
            {!isReady && (
              <span className="inline-flex items-center rounded-md bg-muted px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground">
                {needsAuth ? "Auth Required" : "Not Installed"}
              </span>
            )}
            {runtime.source === "custom" && (
              <span className="inline-flex items-center rounded-md bg-muted px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground">
                Custom
              </span>
            )}
          </div>
          {runtime.command && (
            <div className="mt-1 truncate font-mono text-[11px] text-muted-foreground">
              {runtime.command}
            </div>
          )}
        </div>

        {/* Actions */}
        <div className="ml-auto grid shrink-0 grid-cols-[max-content_6.5rem_1.75rem_1.75rem] items-center gap-1.5">
          {/* Set default */}
          <div className="flex justify-start">
            {isReady && (
              <Button
                type="button"
                variant={isDefault ? "secondary" : "ghost"}
                size="xs"
                onClick={onSetDefault}
                className="h-7 w-full justify-center gap-1 text-xs"
              >
                {isDefault && <Check className="size-3" />}
                {isDefault ? "Default" : "Set default"}
              </Button>
            )}
          </div>

          {/* Install / Docs link */}
          {!isReady && runtime.install_instructions_url && (
            <a
              href={runtime.install_instructions_url}
              target="_blank"
              rel="noopener noreferrer"
              title="Install"
              className="flex size-7 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted/50 hover:text-foreground"
            >
              <ExternalLink className="size-3.5" />
            </a>
          )}
          {isReady && runtime.install_instructions_url && (
            <a
              href={runtime.install_instructions_url}
              target="_blank"
              rel="noopener noreferrer"
              title="Docs"
              className="flex size-7 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted/50 hover:text-foreground"
            >
              <ExternalLink className="size-3.5" />
            </a>
          )}

          {/* Expand toggle (only for installed agents with command) */}
          <div className="flex size-7 items-center justify-center">
            {isReady && runtime.command && (
              <Button
                type="button"
                variant="ghost"
                size="icon-sm"
                onClick={() => setCmdOpen((prev) => !prev)}
                className="size-7 text-muted-foreground hover:text-foreground"
              >
                <ChevronDown
                  className={cn("size-3.5 transition-transform", cmdOpen && "rotate-180")}
                />
              </Button>
            )}
          </div>
        </div>
      </div>

      {/* Expanded command info */}
      {isReady && cmdOpen && runtime.command && (
        <div className="mt-3 pl-10 space-y-2">
          <div>
            <span className="text-[11px] text-muted-foreground">Command</span>
            <code className="ml-2 text-[11px] font-mono bg-muted px-1.5 py-0.5 rounded">
              {runtime.command}
            </code>
          </div>
          {runtime.binary_path && (
            <div>
              <span className="text-[11px] text-muted-foreground">Binary</span>
              <code className="ml-2 text-[11px] font-mono bg-muted px-1.5 py-0.5 rounded break-all">
                {runtime.binary_path}
              </code>
            </div>
          )}
          <p className="text-[11px] text-muted-foreground">
            {runtime.install_hint || "No additional configuration available."}
          </p>
        </div>
      )}
    </div>
  )
}

// Main Component
export function AcpAgentManager() {
  const utils = trpc.useUtils()

  const { data: runtimes = [], isLoading, refetch } = trpc.agents.listRuntimes.useQuery()
  const { data: globalConfig } = trpc.agents.getGlobalConfig.useQuery()

  const setConfigMutation = trpc.agents.setGlobalConfig.useMutation({
    onSuccess: () => {
      utils.agents.getGlobalConfig.invalidate()
      toast.success("Configuration saved")
    },
    onError: (error) => toast.error(`Failed to save: ${error.message}`),
  })

  const installRuntimeMutation = trpc.agents.installRuntime.useMutation({
    onSuccess: () => {
      utils.agents.listRuntimes.invalidate()
      toast.success("Runtime installed successfully")
    },
    onError: (error) => toast.error(`Install failed: ${error.message}`),
  })

  // Normalize and split runtimes
  const normalizedRuntimes = useMemo(
    () =>
      runtimes.map((r) => ({
        ...r,
        availability: normalizeAvailability(r.availability as string),
        auth_status: normalizeAuthStatus(r.auth_status as string),
      })),
    [runtimes]
  )

  const installedRuntimes = normalizedRuntimes.filter(
    (r) => r.availability !== "not_installed"
  )
  const availableRuntimes = normalizedRuntimes.filter(
    (r) => r.availability === "not_installed"
  )

  const defaultRuntimeId = globalConfig?.preferred_runtime ?? null
  const isAutoDefault = !defaultRuntimeId || !installedRuntimes.find((r) => r.id === defaultRuntimeId)

  const handleSetDefault = (runtimeId: string) => {
    setConfigMutation.mutate({
      env_vars: globalConfig?.env_vars ?? {},
      provider: globalConfig?.provider ?? null,
      model: globalConfig?.model ?? null,
      preferred_runtime: runtimeId,
    })
  }

  const handleSetAuto = () => {
    setConfigMutation.mutate({
      env_vars: globalConfig?.env_vars ?? {},
      provider: globalConfig?.provider ?? null,
      model: globalConfig?.model ?? null,
      preferred_runtime: null,
    })
  }

  const handleInstall = (runtimeId: string) => {
    installRuntimeMutation.mutate({ runtimeId })
  }

  if (isLoading) {
    return (
      <div className="h-full overflow-y-auto flex items-center justify-center">
        <div className="text-muted-foreground">Loading agents…</div>
      </div>
    )
  }

  return (
    <div className="h-full overflow-y-auto space-y-8 p-6">
      {/* Default Agent Selector */}
      <section className="space-y-4">
        <div>
          <h3 className="text-sm font-semibold">Default Agent</h3>
          <p className="text-xs text-muted-foreground mt-0.5">
            Choose the default agent for new chats.
          </p>
        </div>

        <div className="flex flex-wrap gap-2">
          {/* Auto pill */}
          <DefaultAgentPill active={isAutoDefault} onClick={handleSetAuto}>
            {isAutoDefault && <Check className="size-3.5" />}
            Auto
          </DefaultAgentPill>

          {/* Each installed agent as a pill */}
          {installedRuntimes.map((runtime) => {
            const isActive = defaultRuntimeId === runtime.id
            return (
              <DefaultAgentPill
                key={runtime.id}
                active={isActive}
                onClick={() => handleSetDefault(runtime.id)}
              >
                <AgentIcon runtime={runtime} size={14} />
                {runtime.label}
                {isActive && <Check className="size-3.5" />}
              </DefaultAgentPill>
            )
          })}
        </div>
      </section>

      {/* Installed Agents */}
      {installedRuntimes.length > 0 && (
        <section className="space-y-3">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-2">
              <h3 className="text-sm font-semibold text-muted-foreground">Installed</h3>
              <span className="inline-flex items-center rounded-md bg-accent px-1.5 py-0.5 text-[10px] font-medium text-accent-foreground">
                {installedRuntimes.length} detected
              </span>
            </div>
            <Button
              type="button"
              variant="ghost"
              size="xs"
              onClick={() => refetch()}
              className="h-7 gap-1.5 text-xs text-muted-foreground hover:text-foreground"
            >
              <RefreshCw className="size-3" />
              Refresh
            </Button>
          </div>

          <div className="divide-y divide-border/40">
            {installedRuntimes.map((runtime) => (
              <InstalledAgentRow
                key={runtime.id}
                runtime={runtime}
                isDefault={defaultRuntimeId === runtime.id}
                onSetDefault={() => handleSetDefault(runtime.id)}
              />
            ))}
          </div>
        </section>
      )}

      {/* Available to Install */}
      {availableRuntimes.length > 0 && (
        <section className="space-y-3">
          <div className="flex items-center gap-2">
            <h3 className="text-sm font-semibold text-muted-foreground">Available to install</h3>
            <span className="inline-flex items-center rounded-md bg-muted px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground">
              {availableRuntimes.length} agents
            </span>
          </div>

          <div className="divide-y divide-border/40">
            {availableRuntimes.map((runtime) => (
              <div key={runtime.id} className="py-3 opacity-70">
                <div className="flex flex-wrap items-start gap-3">
                  <div className="flex size-7 shrink-0 items-center justify-center rounded-md border border-border/50 bg-background/50">
                    <AgentIcon runtime={runtime} size={16} />
                  </div>

                  <div className="min-w-0 flex-1 sm:min-w-[12rem]">
                    <div className="flex items-center gap-2">
                      <span className="text-sm font-medium leading-none">{runtime.label}</span>
                      <span className="inline-flex items-center rounded-md bg-muted px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground">
                        Not Installed
                      </span>
                    </div>
                    {runtime.install_hint && (
                      <div className="mt-1 text-[11px] text-muted-foreground">
                        {runtime.install_hint}
                      </div>
                    )}
                  </div>

                  <div className="ml-auto flex items-center gap-2">
                    {runtime.install_instructions_url && (
                      <a
                        href={runtime.install_instructions_url}
                        target="_blank"
                        rel="noopener noreferrer"
                        title="Install docs"
                        className="flex size-7 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted/50 hover:text-foreground"
                      >
                        <ExternalLink className="size-3.5" />
                      </a>
                    )}
                    {runtime.has_install_command && (
                      <Button
                        size="xs"
                        variant="outline"
                        onClick={() => handleInstall(runtime.id)}
                        disabled={installRuntimeMutation.isPending}
                        className="h-7 gap-1 text-xs"
                      >
                        {installRuntimeMutation.isPending ? (
                          <div className="size-3 animate-spin rounded-full border-2 border-current border-t-transparent" />
                        ) : null}
                        Install
                      </Button>
                    )}
                  </div>
                </div>
              </div>
            ))}
          </div>
        </section>
      )}

      {/* Empty state */}
      {installedRuntimes.length === 0 && availableRuntimes.length === 0 && (
        <div className="flex items-center justify-center rounded-md border border-dashed border-border/50 py-6 text-sm text-muted-foreground">
          No agents detected. Install one to get started.
        </div>
      )}
    </div>
  )
}

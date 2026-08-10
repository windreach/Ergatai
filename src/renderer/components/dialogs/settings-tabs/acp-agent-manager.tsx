import { useState, useMemo } from "react"
import { trpc } from "../../../lib/trpc"
import { cn } from "../../../lib/utils"
import { Button } from "../../ui/button"
import { AcpAgentConfigDialog } from "./acp-agent-config-dialog"
import {
  ChevronDown,
  Plus,
  Settings,
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
  onConfig,
  onInstall,
  isInstalling,
}: {
  runtime: AcpRuntimeCatalogEntry
  onConfig: () => void
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
          {/* Set config button - opens config dialog */}
          <div className="flex justify-start">
            {isReady && (
              <Button
                type="button"
                variant="ghost"
                size="xs"
                onClick={onConfig}
                className="h-7 w-full justify-center gap-1 text-xs"
              >
                <Settings className="size-3" />
                Set config
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

  const installRuntimeMutation = trpc.agents.installRuntime.useMutation({
    onSuccess: () => {
      utils.agents.listRuntimes.invalidate()
      toast.success("Runtime installed successfully")
    },
    onError: (error) => toast.error(`Install failed: ${error.message}`),
  })

  // Agent config dialog state
  const [configDialogOpen, setConfigDialogOpen] = useState(false)
  const [configDialogRuntime, setConfigDialogRuntime] = useState<AcpRuntimeCatalogEntry | null>(null)

  const openConfigDialog = (runtime: AcpRuntimeCatalogEntry | null) => {
    setConfigDialogRuntime(runtime)
    setConfigDialogOpen(true)
  }

  const closeConfigDialog = () => {
    setConfigDialogOpen(false)
  }

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
      {/* Installed Agent Display */}
      <section className="space-y-4">
        <div>
          <h3 className="text-sm font-semibold">Installed Agent</h3>
          <p className="text-xs text-muted-foreground mt-0.5">
            Agents detected on this machine.
          </p>
        </div>

        <div className="flex flex-wrap gap-2">
          {installedRuntimes.map((runtime) => (
            <div
              key={runtime.id}
              className="inline-flex items-center gap-1.5 rounded-full border border-border/60 bg-background/80 px-3 py-1.5 text-xs font-medium text-muted-foreground"
            >
              <AgentIcon runtime={runtime} size={14} />
              {runtime.label}
            </div>
          ))}
        </div>
      </section>

      {/* Config */}
      {installedRuntimes.length > 0 && (
        <section className="space-y-3">
          <div className="flex items-start justify-between">
            <div>
              <h3 className="text-sm font-semibold text-muted-foreground">Config</h3>
              <p className="text-[11px] text-muted-foreground/70 mt-0.5">Uses user config by default</p>
            </div>
            <Button
              type="button"
              variant="outline"
              size="xs"
              onClick={() => openConfigDialog(null)}
              className="h-7 gap-1.5 text-xs"
            >
              <Plus className="size-3" />
              Create Agent
            </Button>
          </div>

          <div className="divide-y divide-border/40">
            {installedRuntimes.map((runtime) => (
              <InstalledAgentRow
                key={runtime.id}
                runtime={runtime}
                onConfig={() => openConfigDialog(runtime)}
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

      {/* Agent Config Dialog */}
      <AcpAgentConfigDialog
        open={configDialogOpen}
        onOpenChange={closeConfigDialog}
        runtime={configDialogRuntime}
        onSuccess={() => {
          refetch()
        }}
      />
    </div>
  )
}

import { useState } from "react"
import { trpc } from "../../../lib/trpc"
import { cn } from "../../../lib/utils"
import { Button } from "../../ui/button"
import { Input } from "../../ui/input"
import { Label } from "../../ui/label"
import { Textarea } from "../../ui/textarea"
import { Badge } from "../../ui/badge"
import {
  CheckCircle2,
  XCircle,
  AlertCircle,
  Plus,
  Trash2,
  Settings,
  ExternalLink,
  Download,
  Wrench,
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

interface HarnessDefinition {
  id: string
  label: string
  command: string
  args: string[]
  env: Record<string, string>
  install_instructions_url: string
  install_hint: string
}

// Top Section: Installed Agent Row
function InstalledAgentRow({
  runtime,
  globalConfig,
  onConfigClick,
}: {
  runtime: AcpRuntimeCatalogEntry
  globalConfig: GlobalAgentConfig | undefined
  onConfigClick: () => void
}) {
  const getStatusBadge = () => {
    if (runtime.availability === "not_installed") {
      return (
        <Badge variant="secondary" className="text-xs">
          Not Installed
        </Badge>
      )
    }
    if (runtime.auth_status === "logged_out") {
      return (
        <Badge variant="outline" className="text-xs border-yellow-500 text-yellow-600">
          Auth Required
        </Badge>
      )
    }
    if (runtime.availability === "available") {
      return (
        <Badge variant="default" className="text-xs">
          <CheckCircle2 className="h-3 w-3 mr-1" />
          Ready
        </Badge>
      )
    }
    return (
      <Badge variant="secondary" className="text-xs">
        Unknown
      </Badge>
    )
  }

  const hasConfig = globalConfig?.env_vars && Object.keys(globalConfig.env_vars).length > 0

  return (
    <div className="flex items-center justify-between p-3 border border-border rounded-lg hover:bg-accent/50 transition-colors">
      <div className="flex items-center gap-3 flex-1 min-w-0">
        {runtime.avatar_url ? (
          <img
            src={runtime.avatar_url}
            alt={runtime.label}
            className="w-10 h-10 rounded-md object-cover flex-shrink-0"
          />
        ) : (
          <div className="w-10 h-10 rounded-md bg-muted flex items-center justify-center flex-shrink-0">
            <Settings className="h-5 w-5 text-muted-foreground" />
          </div>
        )}
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <span className="font-medium text-sm truncate">{runtime.label}</span>
            {runtime.source === "custom" && (
              <Badge variant="secondary" className="text-xs">
                Custom
              </Badge>
            )}
            {getStatusBadge()}
          </div>
          {runtime.command && (
            <code className="text-xs text-muted-foreground mt-0.5 block truncate">
              {runtime.command}
            </code>
          )}
        </div>
      </div>
      <Button
        size="sm"
        variant="outline"
        onClick={onConfigClick}
        className="ml-2 flex-shrink-0"
      >
        <Wrench className="h-4 w-4 mr-1" />
        Config
        {hasConfig && (
          <span className="ml-1 text-xs text-muted-foreground">•</span>
        )}
      </Button>
    </div>
  )
}

// Bottom Section: Available Agent Row
function AvailableAgentRow({
  runtime,
  onInstall,
}: {
  runtime: AcpRuntimeCatalogEntry
  onInstall: () => void
}) {
  return (
    <div className="flex items-center justify-between p-3 border border-dashed border-border rounded-lg hover:bg-accent/30 transition-colors">
      <div className="flex items-center gap-3 flex-1 min-w-0">
        {runtime.avatar_url ? (
          <img
            src={runtime.avatar_url}
            alt={runtime.label}
            className="w-10 h-10 rounded-md object-cover flex-shrink-0 opacity-70"
          />
        ) : (
          <div className="w-10 h-10 rounded-md bg-muted flex items-center justify-center flex-shrink-0 opacity-70">
            <Settings className="h-5 w-5 text-muted-foreground" />
          </div>
        )}
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <span className="font-medium text-sm truncate">{runtime.label}</span>
            <Badge variant="secondary" className="text-xs">
              Not Installed
            </Badge>
          </div>
          {runtime.install_hint && (
            <p className="text-xs text-muted-foreground mt-0.5 line-clamp-1">
              {runtime.install_hint}
            </p>
          )}
        </div>
      </div>
      <div className="flex items-center gap-2 flex-shrink-0">
        {runtime.install_instructions_url && (
          <a
            href={runtime.install_instructions_url}
            target="_blank"
            rel="noopener noreferrer"
            className="inline-flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground transition-colors"
          >
            Docs
            <ExternalLink className="h-3 w-3" />
          </a>
        )}
        <Button size="sm" variant="outline" onClick={onInstall}>
          <Download className="h-4 w-4 mr-1" />
          Install
        </Button>
      </div>
    </div>
  )
}

// Main Component: Two-section layout
export function AcpAgentManager() {
  const [configOpenId, setConfigOpenId] = useState<string | null>(null)
  const [showCustomForm, setShowCustomForm] = useState(false)
  const utils = trpc.useUtils()

  // Fetch runtimes
  const { data: runtimes, isLoading: isLoadingRuntimes } = trpc.agents.listRuntimes.useQuery()

  // Fetch global config (for env vars, provider, model per runtime)
  const { data: globalConfig, isLoading: isLoadingConfig } = trpc.agents.getGlobalConfig.useQuery()

  // Mutations
  const setConfigMutation = trpc.agents.setGlobalConfig.useMutation({
    onSuccess: () => {
      utils.agents.getGlobalConfig.invalidate()
      toast.success("Configuration saved")
    },
    onError: (error) => {
      toast.error(`Failed to save configuration: ${error.message}`)
    },
  })

  const saveCustomHarnessMutation = trpc.agents.saveCustomHarness.useMutation({
    onSuccess: () => {
      utils.agents.listRuntimes.invalidate()
      setShowCustomForm(false)
      toast.success("Custom agent saved")
    },
    onError: (error) => {
      toast.error(`Failed to save custom agent: ${error.message}`)
    },
  })

  const deleteCustomHarnessMutation = trpc.agents.deleteCustomHarness.useMutation({
    onSuccess: () => {
      utils.agents.listRuntimes.invalidate()
      setConfigOpenId(null)
      toast.success("Custom agent deleted")
    },
    onError: (error) => {
      toast.error(`Failed to delete custom agent: ${error.message}`)
    },
  })

  // TODO(Plan 7): Backend installRuntime not yet implemented
  // Placeholder handler for Plan 7
  const handleInstallRuntime = (runtimeId: string, runtimeLabel: string) => {
    // Plan 7 will use runtimeId to call trpc.agents.installRuntime
    void runtimeId
    toast.info(`Install flow for ${runtimeLabel} coming in Plan 7`)
  }

  // Split runtimes into installed vs available
  const installedRuntimes = runtimes?.filter(
    (r: AcpRuntimeCatalogEntry) => r.availability !== "not_installed"
  ) ?? []
  const availableRuntimes = runtimes?.filter(
    (r: AcpRuntimeCatalogEntry) => r.availability === "not_installed"
  ) ?? []

  const configOpenRuntime = runtimes?.find((r: AcpRuntimeCatalogEntry) => r.id === configOpenId)

  if (isLoadingRuntimes || isLoadingConfig) {
    return (
      <div className="h-full flex items-center justify-center">
        <div className="text-muted-foreground">Loading...</div>
      </div>
    )
  }

  if (!runtimes || !globalConfig) {
    return (
      <div className="h-full flex items-center justify-center">
        <div className="text-muted-foreground">Failed to load agents</div>
      </div>
    )
  }

  return (
    <div className="h-full overflow-y-auto">
      <div className="max-w-3xl mx-auto p-6 space-y-6">
        {/* Header */}
        <div className="flex items-center justify-between">
          <div>
            <h2 className="text-lg font-semibold">My Agents</h2>
            <p className="text-sm text-muted-foreground mt-0.5">
              {installedRuntimes.length} installed, {availableRuntimes.length} available
            </p>
          </div>
          <Button size="sm" onClick={() => setShowCustomForm(true)}>
            <Plus className="h-4 w-4 mr-1" />
            Add Custom
          </Button>
        </div>

        {/* Section 1: Installed Agents */}
        <div className="space-y-2">
          <h3 className="text-sm font-semibold text-muted-foreground">
            Installed Agents
          </h3>
          <div className="space-y-2">
            {installedRuntimes.length === 0 ? (
              <div className="text-sm text-muted-foreground text-center py-6 border border-dashed border-border rounded-lg">
                No agents installed yet. Install one from the list below.
              </div>
            ) : (
              installedRuntimes.map((runtime: AcpRuntimeCatalogEntry) => (
                <div key={runtime.id}>
                  <InstalledAgentRow
                    runtime={runtime}
                    globalConfig={globalConfig}
                    onConfigClick={() =>
                      setConfigOpenId(configOpenId === runtime.id ? null : runtime.id)
                    }
                  />
                  {/* Config Panel inline after the clicked row */}
                  {configOpenId === runtime.id && configOpenRuntime && (
                    <div className="mt-2 border border-border rounded-lg p-4 space-y-4 bg-accent/20">
                      <div className="flex items-center justify-between">
                        <h3 className="text-sm font-semibold">
                          Configuration: {configOpenRuntime.label}
                        </h3>
                        <Button
                          size="sm"
                          variant="ghost"
                          onClick={() => setConfigOpenId(null)}
                        >
                          <XCircle className="h-4 w-4" />
                        </Button>
                      </div>
                      <AgentConfigPanel
                        runtime={configOpenRuntime}
                        globalConfig={globalConfig!}
                        onUpdateConfig={(config) => {
                          setConfigMutation.mutate({
                            ...globalConfig!,
                            ...config,
                          })
                        }}
                        onDelete={
                          configOpenRuntime.source === "custom"
                            ? () => {
                                if (
                                  confirm(
                                    `Delete custom agent "${configOpenRuntime.label}"?`
                                  )
                                ) {
                                  deleteCustomHarnessMutation.mutate(configOpenRuntime.id)
                                }
                              }
                            : undefined
                        }
                      />
                    </div>
                  )}
                </div>
              ))
            )}
          </div>
        </div>

        {/* Section 2: Available Agents */}
        <div className="space-y-2">
          <h3 className="text-sm font-semibold text-muted-foreground">
            Available Agents
          </h3>
          <div className="space-y-2">
            {availableRuntimes.length === 0 ? (
              <div className="text-sm text-muted-foreground text-center py-6 border border-dashed border-border rounded-lg">
                All agents are installed!
              </div>
            ) : (
              availableRuntimes.map((runtime: AcpRuntimeCatalogEntry) => (
                <AvailableAgentRow
                  key={runtime.id}
                  runtime={runtime}
                  onInstall={() => handleInstallRuntime(runtime.id, runtime.label)}
                />
              ))
            )}
          </div>
        </div>

        {/* Custom Harness Form (expanded below when clicked) */}
        {showCustomForm && (
          <div className="border border-border rounded-lg p-4">
            <CustomHarnessForm
              onSave={(harness) => saveCustomHarnessMutation.mutate(harness)}
              onCancel={() => setShowCustomForm(false)}
            />
          </div>
        )}
      </div>
    </div>
  )
}

// Config Panel: shown when "Config" is clicked on an installed agent
function AgentConfigPanel({
  runtime,
  globalConfig,
  onUpdateConfig,
  onDelete,
}: {
  runtime: AcpRuntimeCatalogEntry
  globalConfig: GlobalAgentConfig
  onUpdateConfig: (config: Partial<GlobalAgentConfig>) => void
  onDelete?: () => void
}) {
  const [envVars, setEnvVars] = useState<Record<string, string>>(
    globalConfig.env_vars
  )
  const [provider, setProvider] = useState(globalConfig.provider || "")
  const [model, setModel] = useState(globalConfig.model || "")

  const handleSaveConfig = () => {
    onUpdateConfig({
      env_vars: envVars,
      provider: provider || null,
      model: model || null,
      preferred_runtime: runtime.id,
    })
  }

  const handleAddEnvVar = () => {
    const key = `NEW_VAR_${Date.now()}`
    setEnvVars({ ...envVars, [key]: "" })
  }

  const handleRemoveEnvVar = (key: string) => {
    const newVars = { ...envVars }
    delete newVars[key]
    setEnvVars(newVars)
  }

  const handleEnvVarChange = (key: string, value: string) => {
    setEnvVars({ ...envVars, [key]: value })
  }

  return (
    <div className="space-y-4">
      {/* Status */}
      <div className="space-y-2">
        <Label className="text-xs font-medium">Status</Label>
        <div className="flex items-center gap-2 text-sm">
          <Badge
            variant={
              runtime.availability === "available" ? "default" : "secondary"
            }
          >
            {runtime.availability}
          </Badge>
          {runtime.command && (
            <code className="text-xs bg-muted px-2 py-1 rounded">
              {runtime.command}
            </code>
          )}
        </div>
        {runtime.binary_path && (
          <div>
            <Label className="text-xs text-muted-foreground">Binary Path</Label>
            <code className="text-xs bg-muted px-2 py-1 rounded block mt-1">
              {runtime.binary_path}
            </code>
          </div>
        )}
        {runtime.auth_status === "logged_out" && runtime.login_hint && (
          <div className="p-3 bg-yellow-50 dark:bg-yellow-950/20 border border-yellow-200 dark:border-yellow-900 rounded-md">
            <p className="text-sm text-yellow-800 dark:text-yellow-200">
              {runtime.login_hint}
            </p>
          </div>
        )}
      </div>

      {/* Provider & Model */}
      <div className="space-y-2">
        <Label className="text-xs font-medium">Default Provider & Model</Label>
        <div className="grid grid-cols-2 gap-2">
          <Input
            value={provider}
            onChange={(e) => setProvider(e.target.value)}
            placeholder="Provider (e.g., anthropic)"
            className="text-sm"
          />
          <Input
            value={model}
            onChange={(e) => setModel(e.target.value)}
            placeholder="Model (e.g., claude-3-opus)"
            className="text-sm"
          />
        </div>
      </div>

      {/* Environment Variables */}
      <div className="space-y-2">
        <div className="flex items-center justify-between">
          <Label className="text-xs font-medium">Environment Variables</Label>
          <Button size="sm" variant="outline" onClick={handleAddEnvVar}>
            <Plus className="h-3 w-3 mr-1" />
            Add
          </Button>
        </div>
        <div className="space-y-2">
          {Object.entries(envVars).map(([key, value]) => (
            <div key={key} className="flex gap-2">
              <Input
                value={key}
                onChange={(e) => {
                  const newKey = e.target.value
                  const newVars = { ...envVars }
                  delete newVars[key]
                  newVars[newKey] = value
                  setEnvVars(newVars)
                }}
                placeholder="VAR_NAME"
                className="font-mono text-sm"
              />
              <Input
                value={value}
                onChange={(e) => handleEnvVarChange(key, e.target.value)}
                placeholder="value"
                className="font-mono text-sm flex-1"
              />
              <Button
                size="sm"
                variant="ghost"
                onClick={() => handleRemoveEnvVar(key)}
              >
                <Trash2 className="h-4 w-4" />
              </Button>
            </div>
          ))}
        </div>
      </div>

      {/* Actions */}
      <div className="flex justify-between pt-2 border-t">
        {onDelete && (
          <Button variant="destructive" size="sm" onClick={onDelete}>
            <Trash2 className="h-4 w-4 mr-1" />
            Delete Agent
          </Button>
        )}
        <div className="flex-1" />
        <Button onClick={handleSaveConfig}>Save Configuration</Button>
      </div>
    </div>
  )
}

// Custom Harness Form
function CustomHarnessForm({
  onSave,
  onCancel,
}: {
  onSave: (harness: HarnessDefinition) => void
  onCancel: () => void
}) {
  const [id, setId] = useState("")
  const [label, setLabel] = useState("")
  const [command, setCommand] = useState("")
  const [args, setArgs] = useState("")
  const [installHint, setInstallHint] = useState("")
  const [installUrl, setInstallUrl] = useState("")
  const [envVars, setEnvVars] = useState<Record<string, string>>({})

  const handleSubmit = () => {
    if (!id || !label || !command) {
      toast.error("ID, label, and command are required")
      return
    }

    // Filter out empty keys from env vars
    const filteredEnv = Object.fromEntries(
      Object.entries(envVars).filter(([key]) => key.trim() !== "")
    )

    onSave({
      id,
      label,
      command,
      args: args ? args.split("\n").filter(Boolean) : [],
      env: filteredEnv,
      install_hint: installHint,
      install_instructions_url: installUrl,
    })
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <h3 className="text-sm font-semibold">Add Custom Agent</h3>
          <p className="text-xs text-muted-foreground mt-0.5">
            Define a custom ACP agent harness
          </p>
        </div>
        <Button variant="ghost" size="sm" onClick={onCancel}>
          <XCircle className="h-4 w-4" />
        </Button>
      </div>

      <div className="space-y-3">
        <div>
          <Label className="text-xs">ID *</Label>
          <Input
            value={id}
            onChange={(e) => setId(e.target.value)}
            placeholder="my-custom-agent"
            className="mt-1 font-mono text-sm"
          />
        </div>

        <div>
          <Label className="text-xs">Label *</Label>
          <Input
            value={label}
            onChange={(e) => setLabel(e.target.value)}
            placeholder="My Custom Agent"
            className="mt-1 text-sm"
          />
        </div>

        <div>
          <Label className="text-xs">Command *</Label>
          <Input
            value={command}
            onChange={(e) => setCommand(e.target.value)}
            placeholder="my-agent-acp"
            className="mt-1 font-mono text-sm"
          />
        </div>

        <div>
          <Label className="text-xs">Arguments (one per line)</Label>
          <Textarea
            value={args}
            onChange={(e) => setArgs(e.target.value)}
            placeholder="--flag&#10;--option value"
            className="mt-1 font-mono text-sm"
            rows={3}
          />
        </div>

        <div>
          <Label className="text-xs">Install Hint</Label>
          <Input
            value={installHint}
            onChange={(e) => setInstallHint(e.target.value)}
            placeholder="Install from https://..."
            className="mt-1 text-sm"
          />
        </div>

        <div>
          <Label className="text-xs">Install Instructions URL</Label>
          <Input
            value={installUrl}
            onChange={(e) => setInstallUrl(e.target.value)}
            placeholder="https://..."
            className="mt-1 text-sm"
          />
        </div>

        <div>
          <div className="flex items-center justify-between">
            <Label className="text-xs">Environment Variables</Label>
            <Button
              size="sm"
              variant="outline"
              onClick={() => setEnvVars({ ...envVars, "": "" })}
            >
              <Plus className="h-3 w-3 mr-1" />
              Add
            </Button>
          </div>
          <div className="space-y-2 mt-1">
            {Object.entries(envVars).map(([key, value]) => (
              <div key={key} className="flex gap-2">
                <Input
                  value={key}
                  onChange={(e) => {
                    const newKey = e.target.value
                    const newVars = { ...envVars }
                    delete newVars[key]
                    newVars[newKey] = value
                    setEnvVars(newVars)
                  }}
                  placeholder="VAR_NAME"
                  className="font-mono text-sm"
                />
                <Input
                  value={value}
                  onChange={(e) => {
                    setEnvVars({ ...envVars, [key]: e.target.value })
                  }}
                  placeholder="value"
                  className="font-mono text-sm flex-1"
                />
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() => {
                    const newVars = { ...envVars }
                    delete newVars[key]
                    setEnvVars(newVars)
                  }}
                >
                  <Trash2 className="h-4 w-4" />
                </Button>
              </div>
            ))}
          </div>
        </div>
      </div>

      <div className="flex justify-end gap-2 pt-2 border-t">
        <Button variant="outline" onClick={onCancel}>
          Cancel
        </Button>
        <Button onClick={handleSubmit}>Save Custom Agent</Button>
      </div>
    </div>
  )
}

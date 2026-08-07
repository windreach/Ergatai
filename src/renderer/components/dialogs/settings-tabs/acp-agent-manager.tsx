import { useState } from "react"
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query"
import { trpc } from "../../../lib/trpc"
import { cn } from "../../../lib/utils"
import { Button } from "../../ui/button"
import { Input } from "../../ui/input"
import { Label } from "../../ui/label"
import { Textarea } from "../../ui/textarea"
import { Badge } from "../../ui/badge"
import { ScrollArea } from "../../ui/scroll-area"
import { Separator } from "../../ui/separator"
import {
  CheckCircle2,
  XCircle,
  AlertCircle,
  Plus,
  Trash2,
  Settings,
  ExternalLink,
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

// Left Panel: Agent List
function AgentList({
  runtimes,
  selectedId,
  onSelect,
  onAddCustom,
}: {
  runtimes: AcpRuntimeCatalogEntry[]
  selectedId: string | null
  onSelect: (id: string) => void
  onAddCustom: () => void
}) {
  const builtinRuntimes = runtimes.filter((r) => r.source === "builtin")
  const customRuntimes = runtimes.filter((r) => r.source === "custom")

  return (
    <div className="h-full flex flex-col border-r">
      <div className="p-4 border-b">
        <div className="flex items-center justify-between">
          <h2 className="text-lg font-semibold">ACP Agents</h2>
          <Button size="sm" onClick={onAddCustom}>
            <Plus className="h-4 w-4 mr-1" />
            Custom
          </Button>
        </div>
        <p className="text-sm text-muted-foreground mt-1">
          {runtimes.length} agents detected
        </p>
      </div>

      <ScrollArea className="flex-1">
        <div className="p-2 space-y-4">
          {/* Builtin Agents */}
          {builtinRuntimes.length > 0 && (
            <div>
              <h3 className="text-xs font-semibold text-muted-foreground px-2 py-1">
                Builtin Agents
              </h3>
              <div className="space-y-1">
                {builtinRuntimes.map((runtime) => (
                  <AgentListItem
                    key={runtime.id}
                    runtime={runtime}
                    isSelected={selectedId === runtime.id}
                    onSelect={() => onSelect(runtime.id)}
                  />
                ))}
              </div>
            </div>
          )}

          {/* Custom Agents */}
          {customRuntimes.length > 0 && (
            <div>
              <h3 className="text-xs font-semibold text-muted-foreground px-2 py-1">
                Custom Agents
              </h3>
              <div className="space-y-1">
                {customRuntimes.map((runtime) => (
                  <AgentListItem
                    key={runtime.id}
                    runtime={runtime}
                    isSelected={selectedId === runtime.id}
                    onSelect={() => onSelect(runtime.id)}
                  />
                ))}
              </div>
            </div>
          )}
        </div>
      </ScrollArea>
    </div>
  )
}

function AgentListItem({
  runtime,
  isSelected,
  onSelect,
}: {
  runtime: AcpRuntimeCatalogEntry
  isSelected: boolean
  onSelect: () => void
}) {
  const getStatusIcon = () => {
    if (runtime.availability === "available" && runtime.auth_status !== "logged_out") {
      return <CheckCircle2 className="h-4 w-4 text-green-500" />
    }
    if (runtime.availability === "auth_required" || runtime.auth_status === "logged_out") {
      return <AlertCircle className="h-4 w-4 text-yellow-500" />
    }
    return <XCircle className="h-4 w-4 text-red-500" />
  }

  const getStatusText = () => {
    if (runtime.availability === "not_installed") return "Not Installed"
    if (runtime.auth_status === "logged_out") return "Auth Required"
    if (runtime.availability === "available") return "Ready"
    return "Unknown"
  }

  return (
    <button
      onClick={onSelect}
      className={cn(
        "w-full text-left px-3 py-2 rounded-md transition-colors",
        "hover:bg-accent",
        isSelected && "bg-accent"
      )}
    >
      <div className="flex items-start gap-3">
        {runtime.avatar_url ? (
          <img
            src={runtime.avatar_url}
            alt={runtime.label}
            className="w-8 h-8 rounded-md object-cover"
          />
        ) : (
          <div className="w-8 h-8 rounded-md bg-muted flex items-center justify-center">
            <Settings className="h-4 w-4 text-muted-foreground" />
          </div>
        )}
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <span className="font-medium text-sm">{runtime.label}</span>
            {runtime.source === "custom" && (
              <Badge variant="secondary" className="text-xs">
                Custom
              </Badge>
            )}
          </div>
          <div className="flex items-center gap-1 mt-0.5">
            {getStatusIcon()}
            <span className="text-xs text-muted-foreground">
              {getStatusText()}
            </span>
          </div>
        </div>
      </div>
    </button>
  )
}

// Right Panel: Agent Detail
function AgentDetail({
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
    <div className="h-full overflow-y-auto">
      <div className="max-w-2xl mx-auto p-6 space-y-6">
        {/* Header */}
        <div className="flex items-start gap-4">
          {runtime.avatar_url ? (
            <img
              src={runtime.avatar_url}
              alt={runtime.label}
              className="w-16 h-16 rounded-lg object-cover"
            />
          ) : (
            <div className="w-16 h-16 rounded-lg bg-muted flex items-center justify-center">
              <Settings className="h-8 w-8 text-muted-foreground" />
            </div>
          )}
          <div className="flex-1">
            <h2 className="text-2xl font-bold">{runtime.label}</h2>
            <div className="flex items-center gap-2 mt-2">
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
          </div>
          {onDelete && (
            <Button variant="destructive" size="sm" onClick={onDelete}>
              <Trash2 className="h-4 w-4 mr-1" />
              Delete
            </Button>
          )}
        </div>

        <Separator />

        {/* Status Info */}
        <div className="space-y-3">
          <h3 className="text-sm font-semibold">Status</h3>
          <div className="grid gap-3">
            {runtime.binary_path && (
              <div>
                <Label className="text-xs text-muted-foreground">
                  Binary Path
                </Label>
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
            {runtime.availability === "not_installed" && (
              <div className="p-3 bg-blue-50 dark:bg-blue-950/20 border border-blue-200 dark:border-blue-900 rounded-md">
                <p className="text-sm text-blue-800 dark:text-blue-200 mb-2">
                  {runtime.install_hint}
                </p>
                <a
                  href={runtime.install_instructions_url}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="inline-flex items-center gap-1 text-sm text-blue-600 dark:text-blue-400 hover:underline"
                >
                  Installation Instructions
                  <ExternalLink className="h-3 w-3" />
                </a>
              </div>
            )}
          </div>
        </div>

        <Separator />

        {/* Global Configuration */}
        <div className="space-y-4">
          <h3 className="text-sm font-semibold">Global Configuration</h3>

          <div className="space-y-3">
            <div>
              <Label>Provider</Label>
              <Input
                value={provider}
                onChange={(e) => setProvider(e.target.value)}
                placeholder="e.g., anthropic, openai"
                className="mt-1"
              />
            </div>

            <div>
              <Label>Model</Label>
              <Input
                value={model}
                onChange={(e) => setModel(e.target.value)}
                placeholder="e.g., claude-3-opus, gpt-4"
                className="mt-1"
              />
            </div>
          </div>
        </div>

        <Separator />

        {/* Environment Variables */}
        <div className="space-y-4">
          <div className="flex items-center justify-between">
            <h3 className="text-sm font-semibold">Environment Variables</h3>
            <Button size="sm" variant="outline" onClick={handleAddEnvVar}>
              <Plus className="h-3 w-3 mr-1" />
              Add Variable
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

        <Separator />

        {/* Save Button */}
        <div className="flex justify-end">
          <Button onClick={handleSaveConfig}>Save Configuration</Button>
        </div>
      </div>
    </div>
  )
}

// Main Component: Two-column layout
export function AcpAgentManager() {
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [showCustomForm, setShowCustomForm] = useState(false)
  const queryClient = useQueryClient()

  // Fetch runtimes
  const { data: runtimes, isLoading: isLoadingRuntimes } = useQuery({
    queryKey: ["acp-runtimes"],
    queryFn: () => trpc.agents.listRuntimes.query(),
  })

  // Fetch global config
  const { data: globalConfig, isLoading: isLoadingConfig } = useQuery({
    queryKey: ["global-agent-config"],
    queryFn: () => trpc.agents.getGlobalConfig.query(),
  })

  // Mutations
  const setConfigMutation = useMutation({
    mutationFn: (config: GlobalAgentConfig) =>
      trpc.agents.setGlobalConfig.mutate(config),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["global-agent-config"] })
      toast.success("Configuration saved")
    },
    onError: (error) => {
      toast.error(`Failed to save configuration: ${error.message}`)
    },
  })

  const saveCustomHarnessMutation = useMutation({
    mutationFn: (harness: HarnessDefinition) =>
      trpc.agents.saveCustomHarness.mutate(harness),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["acp-runtimes"] })
      setShowCustomForm(false)
      toast.success("Custom agent saved")
    },
    onError: (error) => {
      toast.error(`Failed to save custom agent: ${error.message}`)
    },
  })

  const deleteCustomHarnessMutation = useMutation({
    mutationFn: (id: string) => trpc.agents.deleteCustomHarness.mutate(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["acp-runtimes"] })
      setSelectedId(null)
      toast.success("Custom agent deleted")
    },
    onError: (error) => {
      toast.error(`Failed to delete custom agent: ${error.message}`)
    },
  })

  const selectedRuntime = runtimes?.find((r) => r.id === selectedId)

  const handleUpdateConfig = (config: Partial<GlobalAgentConfig>) => {
    if (!globalConfig) return
    setConfigMutation.mutate({
      ...globalConfig,
      ...config,
    })
  }

  const handleDeleteCustom = () => {
    if (!selectedRuntime || selectedRuntime.source !== "custom") return
    if (confirm(`Delete custom agent "${selectedRuntime.label}"?`)) {
      deleteCustomHarnessMutation.mutate(selectedRuntime.id)
    }
  }

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
    <div className="h-full flex">
      {/* Left Panel: Agent List */}
      <div className="w-80 flex-shrink-0">
        <AgentList
          runtimes={runtimes}
          selectedId={selectedId}
          onSelect={setSelectedId}
          onAddCustom={() => setShowCustomForm(true)}
        />
      </div>

      {/* Right Panel: Detail or Custom Form */}
      <div className="flex-1">
        {showCustomForm ? (
          <CustomHarnessForm
            onSave={(harness) => saveCustomHarnessMutation.mutate(harness)}
            onCancel={() => setShowCustomForm(false)}
          />
        ) : selectedRuntime ? (
          <AgentDetail
            runtime={selectedRuntime}
            globalConfig={globalConfig}
            onUpdateConfig={handleUpdateConfig}
            onDelete={
              selectedRuntime.source === "custom" ? handleDeleteCustom : undefined
            }
          />
        ) : (
          <div className="h-full flex items-center justify-center text-muted-foreground">
            Select an agent to view details
          </div>
        )}
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

    onSave({
      id,
      label,
      command,
      args: args ? args.split("\n").filter(Boolean) : [],
      env: envVars,
      install_hint: installHint,
      install_instructions_url: installUrl,
    })
  }

  return (
    <div className="h-full overflow-y-auto">
      <div className="max-w-2xl mx-auto p-6 space-y-6">
        <div>
          <h2 className="text-2xl font-bold">Add Custom Agent</h2>
          <p className="text-muted-foreground mt-1">
            Define a custom ACP agent harness
          </p>
        </div>

        <Separator />

        <div className="space-y-4">
          <div>
            <Label>ID *</Label>
            <Input
              value={id}
              onChange={(e) => setId(e.target.value)}
              placeholder="my-custom-agent"
              className="mt-1 font-mono"
            />
            <p className="text-xs text-muted-foreground mt-1">
              Unique identifier (lowercase, hyphens allowed)
            </p>
          </div>

          <div>
            <Label>Label *</Label>
            <Input
              value={label}
              onChange={(e) => setLabel(e.target.value)}
              placeholder="My Custom Agent"
              className="mt-1"
            />
          </div>

          <div>
            <Label>Command *</Label>
            <Input
              value={command}
              onChange={(e) => setCommand(e.target.value)}
              placeholder="my-agent-acp"
              className="mt-1 font-mono"
            />
            <p className="text-xs text-muted-foreground mt-1">
              Command to launch the agent
            </p>
          </div>

          <div>
            <Label>Arguments</Label>
            <Textarea
              value={args}
              onChange={(e) => setArgs(e.target.value)}
              placeholder="--flag&#10;--option value"
              className="mt-1 font-mono text-sm"
              rows={3}
            />
            <p className="text-xs text-muted-foreground mt-1">
              One argument per line
            </p>
          </div>

          <div>
            <Label>Install Hint</Label>
            <Input
              value={installHint}
              onChange={(e) => setInstallHint(e.target.value)}
              placeholder="Install from https://..."
              className="mt-1"
            />
          </div>

          <div>
            <Label>Install Instructions URL</Label>
            <Input
              value={installUrl}
              onChange={(e) => setInstallUrl(e.target.value)}
              placeholder="https://..."
              className="mt-1"
            />
          </div>

          <div>
            <Label>Environment Variables</Label>
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
              <Button
                size="sm"
                variant="outline"
                onClick={() => setEnvVars({ ...envVars, "": "" })}
              >
                <Plus className="h-3 w-3 mr-1" />
                Add Variable
              </Button>
            </div>
          </div>
        </div>

        <Separator />

        <div className="flex justify-end gap-2">
          <Button variant="outline" onClick={onCancel}>
            Cancel
          </Button>
          <Button onClick={handleSubmit}>Save Custom Agent</Button>
        </div>
      </div>
    </div>
  )
}

import { useState, useEffect, type KeyboardEvent } from "react"
import { trpc } from "../../../lib/trpc"
import { toast } from "sonner"
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from "../../ui/dialog"
import { Input } from "../../ui/input"
import { Label } from "../../ui/label"
import { Textarea } from "../../ui/textarea"
import { Button } from "../../ui/button"
import { SearchCombobox } from "../../ui/search-combobox"
import { Popover, PopoverTrigger } from "../../ui/popover"
import { ChevronDown } from "lucide-react"

interface AcpRuntimeCatalogEntry {
  id: string
  label: string
  avatar_url: string
  availability: string
  command: string
  binary_path: string | null
  install_hint: string
  install_instructions_url: string
  has_install_command: boolean
  auth_status: string
  login_hint: string | null
  source: string
}

interface AcpAgentConfigDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  runtime: AcpRuntimeCatalogEntry | null
  onSuccess: () => void
}

export function AcpAgentConfigDialog({
  open,
  onOpenChange,
  runtime,
  onSuccess,
}: AcpAgentConfigDialogProps) {
  // Form state
  const [agentId, setAgentId] = useState("")
  const [command, setCommand] = useState("")
  const [displayName, setDisplayName] = useState("")
  const [baseUrl, setBaseUrl] = useState("")
  const [apiKey, setApiKey] = useState("")
  const [model, setModel] = useState("")
  const [proxy, setProxy] = useState("")
  const [persona, setPersona] = useState("")
  const [avatar, setAvatar] = useState("")
  const [selectedTemplateName, setSelectedTemplateName] = useState("")
  const [templateOpen, setTemplateOpen] = useState(false)

  const utils = trpc.useUtils()

  const { data: runtimes = [] } = trpc.agents.listRuntimes.useQuery()
  const { data: existingConfig } = trpc.agents.getAgentConfig.useQuery(
    { name: runtime?.id ?? "" },
    { enabled: open && runtime !== null },
  )

  const installedRuntimes = runtimes.filter(
    (r: AcpRuntimeCatalogEntry) => r.availability !== "not_installed",
  )

  const saveMutation = trpc.agents.saveAgentConfig.useMutation({
    onSuccess: () => {
      utils.agents.listRuntimes.invalidate()
      utils.agents.getAgentConfig.invalidate()
      toast.success("Agent configuration saved")
      onOpenChange(false)
      onSuccess()
    },
    onError: (error) => {
      toast.error(`Failed to save: ${error.message}`)
    },
  })

  // Initialize form when dialog opens
  useEffect(() => {
    if (open) {
      if (runtime && existingConfig) {
        // Editing existing agent config
        setAgentId((existingConfig as any).name || runtime.id)
        setCommand((existingConfig as any).command || runtime.command)
        setDisplayName((existingConfig as any).displayName || (existingConfig as any).display_name || runtime.label)
        setBaseUrl((existingConfig as any).baseUrl || (existingConfig as any).base_url || "")
        setApiKey((existingConfig as any).apiKey || (existingConfig as any).api_key || "")
        setModel((existingConfig as any).model || "")
        setProxy((existingConfig as any).proxy || "")
        setPersona((existingConfig as any).persona || "")
        setAvatar((existingConfig as any).avatar || "")
        setSelectedTemplateName("")
      } else if (runtime) {
        // New config for installed agent
        setAgentId(runtime.id)
        setCommand(runtime.command)
        setDisplayName(runtime.label)
        setBaseUrl("")
        setApiKey("")
        setModel("")
        setProxy("")
        setPersona("")
        setAvatar("")
        setSelectedTemplateName("")
      } else {
        // Create new custom agent
        setAgentId("")
        setCommand("")
        setDisplayName("")
        setBaseUrl("")
        setApiKey("")
        setModel("")
        setProxy("")
        setPersona("")
        setAvatar("")
        setSelectedTemplateName("")
      }
    }
  }, [open, runtime, existingConfig])

  const handleTemplateSelect = (template: AcpRuntimeCatalogEntry) => {
    setSelectedTemplateName(template.label)
    setAgentId(template.id)
    setCommand(template.command)
    setDisplayName(template.label)
    setTemplateOpen(false)
  }

  const handleSubmit = () => {
    const name = agentId || displayName.toLowerCase().replace(/\s+/g, "-")
    const cmd = command || displayName.toLowerCase().replace(/\s+/g, "-")

    // Preserve existing env map when editing (env has no UI editor yet)
    const existingEnv = existingConfig
      ? ((existingConfig as any).env ?? {})
      : {}

    saveMutation.mutate({
      name,
      command: cmd,
      args: [],
      env: existingEnv,
      display_name: displayName || null,
      model: model || null,
      provider: null,
      agent_type: runtime?.source === "custom" ? "custom" : "builtin",
      base_url: baseUrl || null,
      api_key: apiKey || null,
      proxy: proxy || null,
      persona: persona || null,
      avatar: avatar || null,
    })
  }

  const isLoading = saveMutation.isPending
  const isNewAgent = runtime === null

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-[680px] max-h-[85vh] flex flex-col overflow-hidden">
        <DialogHeader>
          <DialogTitle>{isNewAgent ? "Create New Agent" : `Configure ${runtime?.label}`}</DialogTitle>
          <DialogDescription>
            {isNewAgent
              ? "Set up a new agent configuration."
              : `Edit the configuration for ${runtime?.label}.`}
          </DialogDescription>
        </DialogHeader>

        <div className="flex-1 overflow-y-auto space-y-4 px-1"
          onKeyDown={(e: KeyboardEvent) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault()
              handleSubmit()
            }
          }}
        >
          {/* Template selector for new agents */}
          {isNewAgent && (
            <div className="space-y-1.5">
              <Label>Base Agent</Label>
              <SearchCombobox
                isOpen={templateOpen}
                onOpenChange={setTemplateOpen}
                items={installedRuntimes}
                onSelect={handleTemplateSelect}
                placeholder="Search installed agents..."
                emptyMessage="No installed agents found"
                getItemValue={(item) => item.label}
                renderItem={(item) => (
                  <span className="text-sm">{item.label}</span>
                )}
                trigger={
                  <PopoverTrigger asChild>
                    <Button
                      type="button"
                      variant="outline"
                      className="w-full justify-between font-normal"
                    >
                      {selectedTemplateName || "Select an installed agent..."}
                      <ChevronDown className="h-4 w-4 opacity-50" />
                    </Button>
                  </PopoverTrigger>
                }
              />
              <p className="text-xs text-muted-foreground">
                Pre-fills Agent ID, Command, and Name from the selected agent.
              </p>
            </div>
          )}

          {/* Agent ID & Command (only for new agents) */}
          {isNewAgent && (
            <>
              <div className="grid grid-cols-2 gap-4">
                <div className="space-y-1.5">
                  <Label htmlFor="agent-id">
                    Agent ID <span className="text-destructive">*</span>
                  </Label>
                  <Input
                    id="agent-id"
                    value={agentId}
                    onChange={(e) => setAgentId(e.target.value)}
                    placeholder="my-custom-agent"
                    required
                  />
                  <p className="text-xs text-muted-foreground">Unique identifier</p>
                </div>

                <div className="space-y-1.5">
                  <Label htmlFor="command">
                    Command <span className="text-destructive">*</span>
                  </Label>
                  <Input
                    id="command"
                    value={command}
                    onChange={(e) => setCommand(e.target.value)}
                    placeholder="/usr/local/bin/my-agent"
                    required
                  />
                  <p className="text-xs text-muted-foreground">Executable path</p>
                </div>
              </div>
            </>
          )}

          {/* Display Name */}
          <div className="space-y-1.5">
            <Label htmlFor="display-name">Display Name</Label>
            <Input
              id="display-name"
              value={displayName}
              onChange={(e) => setDisplayName(e.target.value)}
              placeholder="My Custom Agent"
            />
          </div>

          {/* Base URL & API Key */}
          <div className="grid grid-cols-2 gap-4">
            <div className="space-y-1.5">
              <Label htmlFor="base-url">Base URL</Label>
              <Input
                id="base-url"
                value={baseUrl}
                onChange={(e) => setBaseUrl(e.target.value)}
                placeholder="https://api.example.com"
              />
            </div>

            <div className="space-y-1.5">
              <Label htmlFor="api-key">API Key</Label>
              <Input
                id="api-key"
                type="password"
                value={apiKey}
                onChange={(e) => setApiKey(e.target.value)}
                placeholder="sk-..."
              />
            </div>
          </div>

          {/* Model & Proxy */}
          <div className="grid grid-cols-2 gap-4">
            <div className="space-y-1.5">
              <Label htmlFor="model">Model</Label>
              <Input
                id="model"
                value={model}
                onChange={(e) => setModel(e.target.value)}
                placeholder="claude-sonnet-4-20250514"
              />
            </div>

            <div className="space-y-1.5">
              <Label htmlFor="proxy">Proxy</Label>
              <Input
                id="proxy"
                value={proxy}
                onChange={(e) => setProxy(e.target.value)}
                placeholder="http://proxy:8080"
              />
            </div>
          </div>

          {/* Persona */}
          <div className="space-y-1.5">
            <Label htmlFor="persona">Persona</Label>
            <Textarea
              id="persona"
              value={persona}
              onChange={(e) => setPersona(e.target.value)}
              placeholder="You are an expert code reviewer..."
              rows={4}
              className="resize-none"
            />
          </div>

          {/* Avatar */}
          <div className="space-y-1.5">
            <Label htmlFor="avatar">Avatar</Label>
            <Input
              id="avatar"
              value={avatar}
              onChange={(e) => setAvatar(e.target.value)}
              placeholder="Image URL or base64 data URI"
            />
          </div>
        </div>

        <DialogFooter>
          <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button type="button" onClick={handleSubmit} disabled={isLoading}>
            {isLoading ? "Saving..." : "Save Configuration"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

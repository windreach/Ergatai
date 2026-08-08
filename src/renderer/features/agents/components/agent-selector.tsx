/**
 * Agent（ACP Runtime）选择器
 *
 * 替代原 AgentModelSelector。只显示 runtime 列表（不显示 model），
 * model 切换通过 /model 命令完成。
 *
 * 两种使用场景：
 * 1. 聊天输入区：subChatId 传入，显示当前 runtime，点击可切换
 * 2. 新 Chat 表单：defaultRuntimeId 传入，选择后创建 sub-chat
 */

import { useMemo, useState } from "react"
import { useAtomValue } from "jotai"
import { trpc } from "../../../lib/trpc"
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "../../../components/ui/popover"
import {
  Command,
  CommandInput,
  CommandList,
  CommandEmpty,
  CommandGroup,
  CommandItem,
} from "../../../components/ui/command"
import { cn } from "../../../lib/utils"
import { subChatRuntimeIdAtomFamily } from "../atoms/runtime"
import type { AcpRuntime } from "../lib/runtime-types"

export interface AgentSelectorProps {
  /** 当前 sub-chat ID（聊天输入区场景） */
  subChatId?: string
  /** 默认 runtime ID（新 Chat 场景） */
  defaultRuntimeId?: string
  /** 选中 runtime 回调 */
  onRuntimeSelect: (runtimeId: string) => void
  /** 是否允许切换（中途切换 runtime 时需提示） */
  allowSwitch?: boolean
}

/**
 * Normalize NAPI enum output to frontend snake_case format.
 * NAPI string_enum outputs camelCase (e.g., "notInstalled"),
 * but frontend types expect snake_case (e.g., "not_installed").
 */
function normalizeAvailability(status: string): AcpRuntime["availability"] {
  if (status === "available" || status === "Available") return "available"
  if (status === "notInstalled" || status === "NotInstalled") return "not_installed"
  if (status === "authRequired" || status === "AuthRequired") return "auth_required"
  return "not_installed"
}

/** Runtime 状态标记图标 */
function RuntimeStatusIcon({ runtime }: { runtime: AcpRuntime }) {
  const status = normalizeAvailability(runtime.availability as string)
  if (status === "available") {
    return <span className="text-green-500">●</span>
  }
  if (status === "auth_required") {
    return <span className="text-yellow-500">⚠</span>
  }
  return <span className="text-muted-foreground">○</span>
}

/**
 * Agent Selector 组件
 *
 * Popover 内显示可用 runtime 列表，支持搜索。
 * 只展示 availability === "available" 的 runtime（安装且已认证）。
 */
export function AgentSelector({
  subChatId,
  defaultRuntimeId,
  onRuntimeSelect,
  allowSwitch = true,
}: AgentSelectorProps) {
  const { data: runtimes = [] } = trpc.agents.listRuntimes.useQuery()
  // 如果 subChatId 存在，读取该 sub-chat 绑定的 runtime
  const boundRuntimeId = useAtomValue(
    subChatRuntimeIdAtomFamily(subChatId ?? "")
  )
  const [open, setOpen] = useState(false)

  // 当前选中的 runtime：
  // 1. 如果有 subChatId 且已绑定，使用绑定的 runtime
  // 2. 否则使用 defaultRuntimeId（新 Chat 场景）
  // 3. 最后 fallback 到列表第一个
  const currentRuntime = useMemo(() => {
    const targetId = subChatId && boundRuntimeId
      ? boundRuntimeId
      : (defaultRuntimeId ?? runtimes[0]?.id)
    return runtimes.find((r) => r.id === targetId) ?? runtimes[0] ?? null
  }, [subChatId, boundRuntimeId, defaultRuntimeId, runtimes])

  const handleSelect = (runtimeId: string) => {
    if (!allowSwitch && runtimeId !== currentRuntime?.id) {
      // 不允许切换时（中途），调用方应通过 allowSwitch=false 控制
      return
    }
    onRuntimeSelect(runtimeId)
    setOpen(false)
  }

  // 如果没有可用 runtime，显示 fallback
  const displayRuntime = currentRuntime ?? (runtimes[0] as AcpRuntime | undefined)

  if (!displayRuntime) {
    return (
      <button
        className="flex items-center gap-1.5 px-2 py-1 text-sm rounded-md text-muted-foreground/50"
        type="button"
        disabled
      >
        <span className="w-4 h-4 rounded bg-muted flex items-center justify-center text-xs">?</span>
        <span>No agents</span>
      </button>
    )
  }

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <button
          className={cn(
            "flex items-center gap-1.5 px-2 py-1 text-sm rounded-md",
            "hover:bg-accent transition-colors",
            "text-muted-foreground"
          )}
          type="button"
        >
          {displayRuntime.avatar_url ? (
            <img
              src={displayRuntime.avatar_url}
              alt={displayRuntime.label}
              className="w-4 h-4 rounded object-cover"
            />
          ) : (
            <span className="w-4 h-4 rounded bg-muted flex items-center justify-center text-xs">
              {displayRuntime.label[0]}
            </span>
          )}
          <span>{displayRuntime.label}</span>
          <span className="text-muted-foreground/50">▾</span>
        </button>
      </PopoverTrigger>

      <PopoverContent align="start" className="w-64 p-0" sideOffset={8}>
        <Command>
          <CommandInput placeholder="搜索 agent..." />
          <CommandList>
            <CommandGroup>
              {runtimes.length === 0 ? (
                <div className="py-6 text-center text-sm text-muted-foreground">
                  没有找到可用 agent
                </div>
              ) : (
                runtimes.map((runtime) => (
                <CommandItem
                  key={runtime.id}
                  value={runtime.label}
                  onSelect={() => handleSelect(runtime.id)}
                  className={cn(
                    "flex items-center gap-2 px-3 py-2",
                    runtime.id === displayRuntime?.id && "bg-accent"
                  )}
                >
                  {runtime.avatar_url ? (
                    <img
                      src={runtime.avatar_url}
                      alt={runtime.label}
                      className="w-5 h-5 rounded object-cover"
                    />
                  ) : (
                    <div className="w-5 h-5 rounded bg-muted flex items-center justify-center text-xs font-medium">
                      {runtime.label[0]}
                    </div>
                  )}
                  <span className="flex-1 text-sm">{runtime.label}</span>
                  <RuntimeStatusIcon runtime={runtime} />
                  {runtime.id === displayRuntime?.id && (
                    <span className="text-xs text-primary">✓</span>
                  )}
                </CommandItem>
              ))}
            </CommandGroup>
          </CommandList>
        </Command>
      </PopoverContent>
    </Popover>
  )
}

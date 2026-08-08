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
import { availableRuntimesAtom, subChatRuntimeIdAtomFamily } from "../atoms/runtime"
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

/** Runtime 状态标记图标 */
function RuntimeStatusIcon({ runtime }: { runtime: AcpRuntime }) {
  if (runtime.availability === "available") {
    return <span className="text-green-500">●</span>
  }
  if (runtime.availability === "auth_required") {
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
  const runtimes = useAtomValue(availableRuntimesAtom)
  // 如果 subChatId 存在，读取该 sub-chat 绑定的 runtime
  const boundRuntimeId = useAtomValue(
    subChatRuntimeIdAtomFamily(subChatId ?? "")
  )
  const [open, setOpen] = useState(false)

  // 只显示可用的 runtime
  const availableRuntimes = useMemo(
    () => runtimes.filter((r) => r.availability === "available"),
    [runtimes]
  )

  // 当前选中的 runtime：
  // 1. 如果有 subChatId 且已绑定，使用绑定的 runtime
  // 2. 否则使用 defaultRuntimeId（新 Chat 场景）
  // 3. 最后 fallback 到列表第一个
  const currentRuntime = useMemo(() => {
    const targetId = subChatId && boundRuntimeId
      ? boundRuntimeId
      : (defaultRuntimeId ?? availableRuntimes[0]?.id)
    return availableRuntimes.find((r) => r.id === targetId) ?? availableRuntimes[0] ?? null
  }, [subChatId, boundRuntimeId, defaultRuntimeId, availableRuntimes])

  const handleSelect = (runtimeId: string) => {
    if (!allowSwitch && runtimeId !== currentRuntime?.id) {
      // 不允许切换时（中途），调用方应通过 allowSwitch=false 控制
      return
    }
    onRuntimeSelect(runtimeId)
    setOpen(false)
  }

  if (!currentRuntime) {
    return null
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
          {currentRuntime.avatar_url ? (
            <img
              src={currentRuntime.avatar_url}
              alt={currentRuntime.label}
              className="w-4 h-4 rounded object-cover"
            />
          ) : (
            <span className="w-4 h-4 rounded bg-muted flex items-center justify-center text-xs">
              {currentRuntime.label[0]}
            </span>
          )}
          <span>{currentRuntime.label}</span>
          <span className="text-muted-foreground/50"></span>
        </button>
      </PopoverTrigger>

      <PopoverContent align="start" className="w-64 p-0" sideOffset={8}>
        <Command>
          <CommandInput placeholder="搜索 agent..." />
          <CommandList>
            <CommandEmpty>没有找到可用 agent</CommandEmpty>
            <CommandGroup>
              {availableRuntimes.map((runtime) => (
                <CommandItem
                  key={runtime.id}
                  value={runtime.label}
                  onSelect={() => handleSelect(runtime.id)}
                  className={cn(
                    "flex items-center gap-2 px-3 py-2",
                    runtime.id === currentRuntime?.id && "bg-accent"
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
                  {runtime.id === currentRuntime?.id && (
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

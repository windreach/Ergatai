/**
 * ACP Runtime 选择状态 atoms
 *
 * 职责：
 * - availableRuntimesAtom: 可用 runtime catalog（从 tRPC 拉取）
 * - selectedRuntimeIdAtom: 用户上次选的 runtime（持久化到 localStorage）
 * - subChatRuntimeIdAtomFamily: 每个 sub-chat 绑定的 runtime（1:1）
 *
 * 与现有 atoms/index.ts 共存，不迁移旧 atoms（避免大 diff）。
 * 后续 Plan 6（旧代码清理）会逐步引用这里的 atoms 替代旧 atoms。
 */

import { atom } from "jotai"
import { atomWithStorage } from "jotai/utils"
import { atomFamily } from "jotai/utils"

import type { AcpRuntime } from "../lib/runtime-types"
import { normalizeRuntimeId } from "../lib/runtime-types"

/**
 * 可用 ACP runtime 列表
 *
 * 初始值为空数组，由 React Query useQuery(trpc.agents.listRuntimes) 填充。
 * 不用 jotai 直接 fetch，因为需要 React Query 的缓存和 refetch 能力。
 * Atom 只存状态，数据获取由组件层负责。
 */
export const availableRuntimesAtom = atom<AcpRuntime[]>([])

/**
 * 用户上次选择的 runtime ID（持久化）
 *
 * 默认值 "claude" 映射旧 "claude-code" provider。
 * 写入时自动 normalize（旧 provider ID → 新 runtime ID）。
 */
const lastSelectedRuntimeIdAtom = atomWithStorage<string>(
  "ergatai:lastSelectedRuntimeId",
  "claude"
)

/**
 * 带 normalize 的 runtime ID atom（setter 自动转换旧 provider ID）
 */
export const normalizedRuntimeIdAtom = atom(
  (get) => get(lastSelectedRuntimeIdAtom),
  (_get, set, id: string) => set(lastSelectedRuntimeIdAtom, normalizeRuntimeId(id))
)

/**
 * Sub-chat → Runtime ID 映射（atomFamily）
 *
 * 1:1 绑定：每个 sub-chat 固定使用一个 runtime。
 * 中途切换 runtime 需要创建新 sub-chat（不在 atom 层处理，由 UI 层控制）。
 *
 * 用法：
 *   const subChatRuntime = useAtom(subChatRuntimeIdAtomFamily(subChatId))
 */
export const subChatRuntimeIdAtomFamily = atomFamily((subChatId: string) =>
  atom<string | null>(null)
)

/**
 * 获取指定 sub-chat 的 runtime 对象
 *
 * 派生 atom：从 subChatRuntimeIdAtomFamily + availableRuntimesAtom 查找。
 */
export const subChatRuntimeAtomFamily = atomFamily((subChatId: string) =>
  atom((get) => {
    const runtimeId = get(subChatRuntimeIdAtomFamily(subChatId))
    if (!runtimeId) return null
    const runtimes = get(availableRuntimesAtom)
    return runtimes.find((r) => r.id === runtimeId) ?? null
  })
)

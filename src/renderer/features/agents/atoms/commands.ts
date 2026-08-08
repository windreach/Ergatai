import { atom } from "jotai"
import { atomFamily } from "jotai/utils"

export interface AvailableCommand {
  name: string
  description: string
  input?: string
}

/**
 * ACP runtime 推送的可用命令（per sub-chat）
 */
export const availableCommandsAtomFamily = atomFamily((subChatId: string) =>
  atom<AvailableCommand[]>([])
)

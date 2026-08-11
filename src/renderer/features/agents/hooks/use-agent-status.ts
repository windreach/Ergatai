import { trpc } from "@/lib/trpc"
import { atom, useAtom } from "jotai"
import { useEffect } from "react"

/**
 * Agent session status from ACP
 */
export interface AgentSessionInfo {
  sessionId: string
  agentName: string
  cwd: string
  status: string
  createdAt?: string
  updatedAt?: string
}

/**
 * Atom for controlling agent status panel visibility
 */
export const agentStatusPanelOpenAtom = atom<boolean>(false)

/**
 * Hook to poll agent sessions from ACP
 * @param intervalMs Polling interval in milliseconds (default: 2000)
 */
export function useAgentStatus(intervalMs: number = 2000) {
  const { data, isLoading, error, refetch } = trpc.acp.listAgentSessions.useQuery(
    undefined,
    {
      refetchInterval: intervalMs,
      staleTime: 1000,
    }
  )

  return {
    sessions: (data as AgentSessionInfo[]) ?? [],
    isLoading,
    error,
    refetch,
  }
}

/**
 * Hook to control agent status panel
 */
export function useAgentStatusPanel() {
  const [isOpen, setIsOpen] = useAtom(agentStatusPanelOpenAtom)

  return {
    isOpen,
    open: () => setIsOpen(true),
    close: () => setIsOpen(false),
    toggle: () => setIsOpen((prev) => !prev),
  }
}

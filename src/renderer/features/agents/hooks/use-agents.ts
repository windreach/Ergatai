"use client"

import { useCallback, useEffect, useState, useMemo } from "react"
import { trpc } from "../../../lib/trpc"
import type { AgentInfo, AgentStatus } from "../ui/agents-panel"

/**
 * Hook to fetch and manage DAG agent states
 *
 * Polls trpc.dag.getState() periodically and transforms TaskGraph data
 * into AgentInfo[] format for the AgentsPanel component.
 * Also fetches agent status to get session_id mapping for agent switching.
 */
export function useAgents() {
  // Get DAG state from backend
  const { data, isLoading, error, refetch } = trpc.dag.getState.useQuery(
    undefined,
    {
      // Poll every 2 seconds when there are active agents
      refetchInterval: (query) => {
        const hasActiveAgents = query.state?.data?.nodes?.some(
          (n: any) => n.status === "Running" || n.status === "Pending"
        )
        return hasActiveAgents ? 2000 : false // Stop polling if no active agents
      },
      // Don't refetch on window focus (avoid unnecessary requests)
      refetchOnWindowFocus: false,
      // Retry on error
      retry: 3,
      retryDelay: 1000,
    }
  )

  // Get agent status (includes session_id mapping)
  const { data: agentsStatus } = trpc.dag.getAgentsStatus.useQuery(
    undefined,
    {
      refetchInterval: 2000, // Poll every 2 seconds
      refetchOnWindowFocus: false,
    }
  )

  // Create mapping: task_id → session_id
  const taskToSessionMap = useMemo(() => {
    const map = new Map<string, string>()
    if (agentsStatus) {
      for (const agent of agentsStatus) {
        if (agent.session_id) {
          map.set(agent.task_id, agent.session_id)
        }
      }
    }
    return map
  }, [agentsStatus])

  // Transform TaskGraph nodes to AgentInfo[]
  const agents: AgentInfo[] = useMemo(() => {
    if (!data?.nodes) return []

    return data.nodes.map((node: any) => ({
      agentId: node.id,
      name: formatAgentName(node),
      status: mapTaskStatusToAgentStatus(node.status),
      isMain: false, // DAG nodes are sub-agents, not main agent
      lastActiveAt: Date.now(), // Could be improved with actual timestamp
      sessionId: taskToSessionMap.get(node.id), // ACP session ID
    }))
  }, [data, taskToSessionMap])

  return {
    agents,
    isLoading,
    error: error?.message,
    refetch,
    // Convenience: check if any agents are running
    hasActiveAgents: agents.some(
      (a) => a.status === "running" || a.status === "pending"
    ),
    // Count by status
    counts: {
      total: agents.length,
      running: agents.filter((a) => a.status === "running").length,
      completed: agents.filter((a) => a.status === "completed").length,
      failed: agents.filter((a) => a.status === "failed").length,
      pending: agents.filter((a) => a.status === "pending").length,
    },
  }
}

/**
 * Format agent name from TaskNode data
 */
function formatAgentName(node: any): string {
  // Format: "Agent-Name: Task Description"
  const agent = node.agent || "Unknown"
  const task = node.task || node.id

  // Truncate long task descriptions
  const maxLen = 30
  const shortTask = task.length > maxLen ? task.slice(0, maxLen) + "..." : task

  return `${agent}: ${shortTask}`
}

/**
 * Map Rust TaskStatus to frontend AgentStatus
 */
function mapTaskStatusToAgentStatus(status: string): AgentStatus {
  switch (status) {
    case "Pending":
      return "pending"
    case "Running":
      return "running"
    case "Completed":
      return "completed"
    case "Failed":
      return "failed"
    case "Skipped":
      return "completed" // Treat skipped as completed for UI purposes
    default:
      return "pending"
  }
}

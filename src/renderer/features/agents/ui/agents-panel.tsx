"use client"

import { useState, useMemo } from "react"
import { ChevronDown, Bot, Check, Clock, Loader2, X } from "lucide-react"
import { cn } from "../../../lib/utils"

// Agent 状态类型
export type AgentStatus = "running" | "completed" | "failed" | "pending"

export interface AgentInfo {
  agentId: string
  name: string
  status: AgentStatus
  isMain?: boolean
  lastActiveAt?: number
  sessionId?: string // ACP session ID for agent switching
}

// Mock 数据 - 用于演示效果
const MOCK_AGENTS: AgentInfo[] = [
  { agentId: "main", name: "主 Agent", status: "completed", isMain: true },
  { agentId: "agent-a", name: "Agent-A: 分析代码", status: "running" },
  { agentId: "agent-b", name: "Agent-B: 写实现", status: "completed" },
  { agentId: "agent-c", name: "Agent-C: 写测试", status: "pending" },
]

interface AgentsPanelProps {
  /** 当前选中的 Agent ID */
  selectedAgentId?: string
  /** Agent 列表（不传则使用 mock 数据） */
  agents?: AgentInfo[]
  /** 选中 Agent 回调 */
  onSelectAgent?: (agentId: string) => void
  /** 默认展开状态 */
  defaultExpanded?: boolean
}

/**
 * 可折叠的 Agent 面板组件
 *
 * 显示在中栏的 "New Chat" 和 "Chats" 之间
 * 展示当前 DAG 任务中的所有 Agent（主 Agent + 子 Agent）
 */
export function AgentsPanel({
  selectedAgentId = "main",
  agents = MOCK_AGENTS,
  onSelectAgent,
  defaultExpanded = false,
}: AgentsPanelProps) {
  const [isExpanded, setIsExpanded] = useState(defaultExpanded)

  // Agent 数量
  const subAgentCount = agents.filter((a) => !a.isMain).length

  // 状态图标
  function getStatusIcon(status: AgentStatus, isActive: boolean) {
    switch (status) {
      case "running":
        return (
          <Loader2
            className={cn(
              "w-4 h-4 animate-spin",
              isActive ? "text-foreground" : "text-muted-foreground"
            )}
          />
        )
      case "completed":
        return (
          <Check
            className={cn(
              "w-4 h-4",
              isActive ? "text-green-600 dark:text-green-500" : "text-muted-foreground"
            )}
          />
        )
      case "failed":
        return (
          <X className="w-4 h-4 text-red-600 dark:text-red-500" />
        )
      case "pending":
        return (
          <Clock
            className={cn(
              "w-4 h-4",
              isActive ? "text-foreground" : "text-muted-foreground"
            )}
          />
        )
    }
  }

  return (
    <div className="border-b border-border/40">
      {/* 折叠按钮 */}
      <button
        onClick={() => setIsExpanded(!isExpanded)}
        className={cn(
          "flex items-center gap-2 px-3 py-2 w-full hover:bg-muted/50 transition-colors",
          "text-left"
        )}
      >
        <ChevronDown
          className={cn(
            "w-4 h-4 text-muted-foreground transition-transform duration-200",
            !isExpanded && "-rotate-90"
          )}
        />
        <Bot className="w-4 h-4 text-muted-foreground" />
        <span className="text-sm font-medium text-foreground">Agents</span>
        {subAgentCount > 0 && (
          <span className="text-xs text-muted-foreground ml-auto tabular-nums">
            {subAgentCount}
          </span>
        )}
      </button>

      {/* Agent 列表 - 简单条件渲染 */}
      {isExpanded && (
        <div className="pb-2 space-y-0.5">
          {agents.map((agent) => {
            const isActive = agent.agentId === selectedAgentId
            return (
              <AgentRow
                key={agent.agentId}
                agent={agent}
                icon={getStatusIcon(agent.status, isActive)}
                isActive={isActive}
                onClick={() => onSelectAgent?.(agent.agentId)}
              />
            )
          })}
        </div>
      )}
    </div>
  )
}

// 单个 Agent 行
interface AgentRowProps {
  agent: AgentInfo
  icon: React.ReactNode
  isActive: boolean
  onClick: () => void
}

function AgentRow({ agent, icon, isActive, onClick }: AgentRowProps) {
  return (
    <button
      onClick={onClick}
      className={cn(
        "flex items-center gap-2 px-4 py-1.5 w-full text-left transition-colors",
        "hover:bg-muted/50",
        isActive && "bg-muted/70"
      )}
    >
      {/* 缩进 */}
      {agent.isMain && <span className="w-1" />}
      {!agent.isMain && <span className="w-2" />}

      {/* 状态图标 */}
      {icon}

      {/* 名称 */}
      <span
        className={cn(
          "text-sm truncate flex-1",
          isActive ? "text-foreground font-medium" : "text-muted-foreground"
        )}
      >
        {agent.name}
      </span>

      {/* 主 Agent 标记 */}
      {agent.isMain && (
        <span className="text-xs text-muted-foreground/60">当前</span>
      )}
    </button>
  )
}

// 默认导出，方便使用
export default AgentsPanel

"use client"

import React, { useMemo } from "react"
import { X, Plus, Bot, FileText } from "lucide-react"
import { cn } from "../lib/utils"
import { useAgentSubChatStore, type SubChatMeta } from "../features/agents/stores/sub-chat-store"

/**
 * Firefox/Chrome-style tab bar for Agent Sessions (sub-chats).
 *
 * Key design characteristics:
 * - Tab bar has light gray background strip
 * - Active tab: white bg, rounded-top, connects to content below (no gap)
 * - Inactive tabs: gray bg, slightly shorter, rounded-top
 * - Close × visible on hover (inactive) / always (active)
 * - + button on the right for new tab
 */
export function BrowserTabBar() {
  const {
    openSubChatIds,
    activeSubChatId,
    allSubChats,
    setActiveSubChat,
    removeFromOpenSubChats,
  } = useAgentSubChatStore()

  const subChatMap = useMemo(() => {
    const map = new Map<string, SubChatMeta>()
    allSubChats.forEach((sc: SubChatMeta) => map.set(sc.id, sc))
    return map
  }, [allSubChats])

  const handleTabClick = (subChatId: string) => {
    setActiveSubChat(subChatId)
  }

  const handleTabClose = (e: React.MouseEvent, subChatId: string) => {
    e.stopPropagation()
    removeFromOpenSubChats(subChatId)
  }

  const handleNewTab = () => {
    console.log("[BrowserTabBar] New tab clicked")
  }

  const getTabIcon = (mode?: string) => {
    if (mode === "plan") {
      return <FileText className="w-3.5 h-3.5" />
    }
    return <Bot className="w-3.5 h-3.5" />
  }

  return (
    // Tab strip background - light gray bar
    <div className="h-9 flex-shrink-0 flex items-end gap-px px-1 bg-[#dfe1e5] dark:bg-[#1b1b1f]">
      {/* Tabs */}
      {openSubChatIds.map((subChatId: string) => {
        const subChat = subChatMap.get(subChatId)
        if (!subChat) return null

        const isActive = subChatId === activeSubChatId

        return (
          <div
            key={subChatId}
            onClick={() => handleTabClick(subChatId)}
            className={cn(
              "group flex items-center gap-1.5 px-3 cursor-pointer rounded-t-md transition-colors",
              // Active: white bg, taller (h-8), connects to content
              isActive
                ? "h-8 bg-background dark:bg-[#2b2b2b] -mb-px"
                : // Inactive: gray bg, shorter (h-7)
                  "h-7 bg-[#c8ccd1] dark:bg-[#3a3a3e] hover:bg-[#d8dce1] dark:hover:bg-[#45454a]"
            )}
          >
            {/* Icon */}
            <div className="flex-shrink-0 text-muted-foreground flex items-center">
              {getTabIcon(subChat.mode)}
            </div>

            {/* Name */}
            <span
              className={cn(
                "text-xs truncate max-w-[160px]",
                isActive ? "text-foreground font-medium" : "text-muted-foreground"
              )}
            >
              {subChat.name || "New Chat"}
            </span>

            {/* Close button */}
            <button
              onClick={(e) => handleTabClose(e, subChatId)}
              className={cn(
                "flex-shrink-0 w-4 h-4 flex items-center justify-center rounded-sm",
                // Active: always visible; Inactive: hover only
                isActive
                  ? "opacity-60 hover:opacity-100"
                  : "opacity-0 group-hover:opacity-60 hover:opacity-100",
                "hover:bg-black/10 dark:hover:bg-white/10 transition-opacity"
              )}
              aria-label={`Close ${subChat.name || "tab"}`}
            >
              <X className="w-3 h-3" />
            </button>
          </div>
        )
      })}

      {/* New Tab Button */}
      <button
        onClick={handleNewTab}
        className="flex-shrink-0 w-6 h-6 flex items-center justify-center rounded-sm mx-0.5 text-muted-foreground hover:text-foreground hover:bg-black/10 dark:hover:bg-white/10 transition-colors"
        aria-label="New tab"
      >
        <Plus className="w-3.5 h-3.5" />
      </button>

      {/* Spacer */}
      <div className="flex-1" />
    </div>
  )
}

"use client"

import React, { useMemo } from "react"
import { X, Plus, Bot, FileText } from "lucide-react"
import { cn } from "../lib/utils"
import { useAgentSubChatStore, type SubChatMeta } from "../features/agents/stores/sub-chat-store"

/**
 * Chrome-style tab bar for Agent Sessions (sub-chats).
 *
 * Design inspired by Chrome/Edge browser tabs:
 * - Trapezoid-like shape with rounded top corners
 * - Active tab: white background, connected to content area
 * - Inactive tabs: gray background, slightly shorter
 * - Hover: slightly lighter gray
 * - Close button appears on hover
 */
export function BrowserTabBar() {
  const {
    openSubChatIds,
    activeSubChatId,
    allSubChats,
    setActiveSubChat,
    removeFromOpenSubChats,
  } = useAgentSubChatStore()

  // Build a map of sub-chat metadata for quick lookup
  const subChatMap = useMemo(() => {
    const map = new Map<string, SubChatMeta>()
    allSubChats.forEach((sc: SubChatMeta) => map.set(sc.id, sc))
    return map
  }, [allSubChats])

  // Handle tab click - switch active sub-chat
  const handleTabClick = (subChatId: string) => {
    setActiveSubChat(subChatId)
  }

  // Handle tab close - remove from open tabs
  const handleTabClose = (e: React.MouseEvent, subChatId: string) => {
    e.stopPropagation()
    removeFromOpenSubChats(subChatId)
  }

  // Handle new tab - placeholder for now (will be connected to chat creation)
  const handleNewTab = () => {
    // TODO: Connect to "New Chat" creation flow
    console.log("[BrowserTabBar] New tab clicked")
  }

  // Get icon based on sub-chat mode
  const getTabIcon = (mode?: string) => {
    if (mode === "plan") {
      return <FileText className="w-3.5 h-3.5" />
    }
    return <Bot className="w-3.5 h-3.5" />
  }

  return (
    <div className="h-10 flex-shrink-0 flex items-end bg-[#dee1e6] dark:bg-[#202124] px-2 pt-2 gap-0.5 overflow-x-auto scrollbar-hide">
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
              "group relative flex items-center gap-2 px-4 py-2 min-w-[150px] max-w-[240px] cursor-pointer rounded-t-lg transition-all duration-150",
              // Active tab: white background, slightly taller, no bottom border gap
              isActive
                ? "bg-background dark:bg-[#2d2d2d] shadow-sm"
                : // Inactive tabs: gray background, hover effect
                  "bg-[#c8ccd1] dark:bg-[#35363a] hover:bg-[#d8dce1] dark:hover:bg-[#404145]"
            )}
          >
            {/* Icon */}
            <div className="flex-shrink-0 text-muted-foreground">
              {getTabIcon(subChat.mode)}
            </div>

            {/* Name */}
            <span
              className={cn(
                "text-xs truncate flex-1 font-medium",
                isActive ? "text-foreground" : "text-muted-foreground"
              )}
            >
              {subChat.name || "New Chat"}
            </span>

            {/* Close button */}
            <button
              onClick={(e) => handleTabClose(e, subChatId)}
              className={cn(
                "flex-shrink-0 w-4 h-4 flex items-center justify-center rounded",
                "opacity-0 group-hover:opacity-100 transition-opacity",
                "hover:bg-black/10 dark:hover:bg-white/10"
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
        className="flex-shrink-0 w-8 h-8 flex items-center justify-center rounded-lg ml-1 text-muted-foreground hover:text-foreground hover:bg-black/10 dark:hover:bg-white/10 transition-colors"
        aria-label="New tab"
      >
        <Plus className="w-4 h-4" />
      </button>

      {/* Spacer to fill remaining width */}
      <div className="flex-1" />
    </div>
  )
}

"use client"

import React, { useMemo } from "react"
import { X, Plus, Bot, FileText } from "lucide-react"
import { cn } from "../lib/utils"
import { useAgentSubChatStore, type SubChatMeta } from "../features/agents/stores/sub-chat-store"

/**
 * Browser-style tab bar for Agent Sessions (sub-chats).
 *
 * Positioned below CustomTitleBar, above AgentsContent.
 * Each tab represents one Agent Session (sub-chat).
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
    <div className="h-10 flex-shrink-0 flex items-end gap-0.5 px-2 pt-1 bg-muted/50 border-b border-border overflow-x-auto scrollbar-thin">
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
              "group relative flex items-center gap-1.5 px-3 py-1.5 min-w-[140px] max-w-[220px] cursor-pointer rounded-t-md transition-colors",
              isActive
                ? "bg-background border-t border-x border-border -mb-px"
                : "bg-muted/70 hover:bg-muted/90"
            )}
          >
            {/* Icon */}
            <div className="flex-shrink-0 text-muted-foreground">
              {getTabIcon(subChat.mode)}
            </div>

            {/* Name */}
            <span
              className={cn(
                "text-xs truncate flex-1",
                isActive ? "text-foreground font-medium" : "text-muted-foreground"
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
                "hover:bg-muted-foreground/20 hover:text-foreground"
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
        className="flex-shrink-0 w-7 h-7 flex items-center justify-center rounded ml-1 text-muted-foreground hover:text-foreground hover:bg-muted transition-colors"
        aria-label="New tab"
      >
        <Plus className="w-4 h-4" />
      </button>

      {/* Spacer to fill remaining width */}
      <div className="flex-1" />
    </div>
  )
}

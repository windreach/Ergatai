"use client"

import React, { useMemo } from "react"
import { X, Plus, Bot, FileText } from "lucide-react"
import { cn } from "../lib/utils"
import { useAgentSubChatStore, type SubChatMeta } from "../features/agents/stores/sub-chat-store"

/**
 * Firefox-style floating tab bar for Agent Sessions (sub-chats).
 *
 * Key design:
 * - Tab strip: very light gray background
 * - Active tab: white card with rounded corners, shadow (floating effect)
 * - Inactive tabs: transparent, merge with strip background
 * - Close ×: hover to reveal
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
    // Tab strip - very light gray
    <div className="h-10 flex-shrink-0 flex items-end gap-1 px-2 pb-px bg-[#f0f0f0] dark:bg-[#252526] border-b border-border/50">
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
              "group relative flex items-center gap-1.5 px-3 cursor-pointer transition-all duration-150",
              // Active: floating white card with shadow
              isActive
                ? "h-8 bg-background dark:bg-[#2d2d2d] rounded-t-md shadow-sm -mb-px z-10"
                : // Inactive: transparent, blends with strip
                  "h-7 rounded-t-md hover:bg-[#e8e8e8] dark:hover:bg-[#3a3a3e]"
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
                isActive
                  ? "opacity-50 hover:opacity-100"
                  : "opacity-0 group-hover:opacity-50 hover:opacity-100",
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

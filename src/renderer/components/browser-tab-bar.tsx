"use client"

import React, { useMemo } from "react"
import { X, Plus, Bot, FileText } from "lucide-react"
import { cn } from "../lib/utils"
import { useAgentSubChatStore, type SubChatMeta } from "../features/agents/stores/sub-chat-store"

/**
 * Chrome-style tab bar (adamschwartz/chrome-tabs technique).
 *
 * Uses pseudo-elements with radial-gradient at bottom corners to create
 * the characteristic "round-out" curved transition between tab and strip.
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

  const handleTabClick = (subChatId: string) => setActiveSubChat(subChatId)

  const handleTabClose = (e: React.MouseEvent, subChatId: string) => {
    e.stopPropagation()
    removeFromOpenSubChats(subChatId)
  }

  const handleNewTab = () => console.log("[BrowserTabBar] New tab clicked")

  const getTabIcon = (mode?: string) => {
    if (mode === "plan") return <FileText className="w-3.5 h-3.5" />
    return <Bot className="w-3.5 h-3.5" />
  }

  return (
    <div className="h-10 flex-shrink-0 flex items-end gap-0.5 px-2 overflow-x-auto scrollbar-hide bg-[#dee1e6]">
      {openSubChatIds.map((subChatId: string) => {
        const subChat = subChatMap.get(subChatId)
        if (!subChat) return null
        const isActive = subChatId === activeSubChatId

        const tabColor = isActive ? "#ffffff" : "#c8ccd1"

        return (
          <div
            key={subChatId}
            onClick={() => handleTabClick(subChatId)}
            className="group relative cursor-pointer flex-shrink-0"
            style={{ height: "36px", minWidth: "100px", maxWidth: "240px" }}
          >
            {/* Tab body */}
            <div
              className={cn(
                "relative z-10 flex items-center gap-1.5 px-3 h-full rounded-t-[8px] transition-colors",
                "group-hover:opacity-90"
              )}
              style={{ backgroundColor: tabColor }}
            >
              {/* Icon */}
              <div className="flex-shrink-0 text-muted-foreground flex items-center">
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
                  "flex-shrink-0 w-4 h-4 flex items-center justify-center rounded-sm",
                  isActive
                    ? "opacity-50 hover:opacity-100"
                    : "opacity-0 group-hover:opacity-50 hover:opacity-100",
                  "hover:bg-black/10 transition-opacity"
                )}
                aria-label={`Close ${subChat.name || "tab"}`}
              >
                <X className="w-3 h-3" />
              </button>
            </div>

            {/* Left round-out curve (::before) - creates the outward curve at bottom-left */}
            <div
              className="absolute z-[5] pointer-events-none"
              style={{
                bottom: "-4px",
                left: "-8px",
                width: "16px",
                height: "16px",
                background: `radial-gradient(circle at 0% 0%, transparent 8px, ${tabColor} 8px)`,
              }}
            />

            {/* Right round-out curve (::after) - creates the outward curve at bottom-right */}
            <div
              className="absolute z-[5] pointer-events-none"
              style={{
                bottom: "-4px",
                right: "-8px",
                width: "16px",
                height: "16px",
                background: `radial-gradient(circle at 100% 0%, transparent 8px, ${tabColor} 8px)`,
              }}
            />
          </div>
        )
      })}

      {/* New Tab Button */}
      <button
        onClick={handleNewTab}
        className="flex-shrink-0 w-7 h-7 flex items-center justify-center rounded-sm ml-1 mb-1 text-muted-foreground hover:text-foreground hover:bg-black/10 transition-colors z-10"
        aria-label="New tab"
      >
        <Plus className="w-4 h-4" />
      </button>

      <div className="flex-1" />
    </div>
  )
}

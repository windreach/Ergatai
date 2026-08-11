"use client"

import React, { useMemo } from "react"
import { Plus, Bot, FileText } from "lucide-react"
import "./chrome-tabs.css"
import { useAgentSubChatStore, type SubChatMeta } from "../features/agents/stores/sub-chat-store"

/**
 * SVG tab shape — the chrome-tab-geometry path.
 * This is the same SVG used by adamschwartz/chrome-tabs.
 * The path: M17 0h197v36H0v-2c4.5 0 9-3.5 9-8V8c0-4.5 3.5-8 8-8z
 */
const ChromeTabSVG = () => (
  <svg version="1.1" xmlns="http://www.w3.org/2000/svg">
    <defs>
      <symbol id="chrome-tab-geometry-left" viewBox="0 0 214 36">
        <path d="M17 0h197v36H0v-2c4.5 0 9-3.5 9-8V8c0-4.5 3.5-8 8-8z" />
      </symbol>
      <symbol id="chrome-tab-geometry-right" viewBox="0 0 214 36">
        <use xlinkHref="#chrome-tab-geometry-left" />
      </symbol>
    </defs>
    <svg width="52%" height="100%">
      <use
        xlinkHref="#chrome-tab-geometry-left"
        width="214"
        height="36"
        className="chrome-tab-geometry"
      />
    </svg>
    <g transform="scale(-1, 1)">
      <svg width="52%" height="100%" x="-100%" y="0">
        <use
          xlinkHref="#chrome-tab-geometry-right"
          width="214"
          height="36"
          className="chrome-tab-geometry"
        />
      </svg>
    </g>
  </svg>
)

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
    if (mode === "plan") return <FileText />
    return <Bot />
  }

  return (
    <>
      <div className="chrome-tabs">
        <div className="chrome-tabs-content">
          {openSubChatIds.map((subChatId: string) => {
            const subChat = subChatMap.get(subChatId)
            if (!subChat) return null
            const isActive = subChatId === activeSubChatId

            return (
              <div
                key={subChatId}
                className="chrome-tab"
                active={isActive ? "" : undefined}
                onClick={() => handleTabClick(subChatId)}
              >
                <div className="chrome-tab-dividers" />
                <div className="chrome-tab-background">
                  <ChromeTabSVG />
                </div>
                <div className="chrome-tab-content">
                  <div className="chrome-tab-favicon">
                    {getTabIcon(subChat.mode)}
                  </div>
                  <div className="chrome-tab-title">
                    {subChat.name || "New Chat"}
                  </div>
                  <button
                    className="chrome-tab-close"
                    onClick={(e) => handleTabClose(e, subChatId)}
                    aria-label={`Close ${subChat.name || "tab"}`}
                  />
                </div>
              </div>
            )
          })}

          {/* New Tab Button */}
          <button
            className="chrome-tab-new"
            onClick={handleNewTab}
            aria-label="New tab"
          >
            <Plus size={16} />
          </button>
        </div>
        <div className="chrome-tabs-bottom-bar" />
      </div>
      <div className="chrome-tabs-optional-shadow-below-bottom-bar" />
    </>
  )
}

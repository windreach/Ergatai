// src/renderer/components/custom-title-bar.tsx
import { useAtomValue } from 'jotai'
import { cn } from '@/lib/utils'
import { isFocusedAtom, isFullscreenAtom } from '@/lib/atoms/window'
import { WindowControls } from './window-controls'

export function CustomTitleBar() {
  const isFocused = useAtomValue(isFocusedAtom)
  const isFullscreen = useAtomValue(isFullscreenAtom)
  const platform = window.desktopApi?.platform

  // Don't render on macOS (uses native titlebar)
  if (platform === 'darwin') {
    return null
  }

  // Hide in fullscreen mode
  if (isFullscreen) {
    return null
  }

  const handleDoubleClick = async () => {
    // Double-click titlebar to maximize/restore
    const isMaximized = await window.desktopApi?.windowIsMaximized()
    if (isMaximized) {
      await window.desktopApi?.windowRestore()
    } else {
      await window.desktopApi?.windowMaximize()
    }
  }

  return (
    <div className="h-8 flex-shrink-0 flex items-center justify-between bg-background border-b">
      {/* Left side - App title (draggable) */}
      <div
        className="flex items-center gap-2 px-3 h-full flex-1"
        // @ts-expect-error - WebKit-specific property for Electron window dragging
        style={{ WebkitAppRegion: 'drag' }}
        onDoubleClick={handleDoubleClick}
      >
        <span className={cn('text-xs font-medium', !isFocused && 'opacity-60')}>
          Ergatai
        </span>
      </div>

      {/* Right side - Window controls (non-draggable) */}
      <div
        className="flex items-center h-full"
        // @ts-expect-error - WebKit-specific property
        style={{ WebkitAppRegion: 'no-drag' }}
      >
        <WindowControls />
      </div>
    </div>
  )
}

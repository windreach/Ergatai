// src/renderer/components/window-controls.tsx
import { useAtomValue } from 'jotai'
import { Minus, Square, Copy, X } from 'lucide-react'
import { cn } from '@/lib/utils'
import { isMaximizedAtom } from '@/lib/atoms/window'

export function WindowControls() {
  const isMaximized = useAtomValue(isMaximizedAtom)
  const platform = window.desktopApi?.platform

  const handleMinimize = () => {
    window.desktopApi?.windowMinimize()
  }

  const handleMaximize = () => {
    if (isMaximized) {
      window.desktopApi?.windowRestore()
    } else {
      window.desktopApi?.windowMaximize()
    }
  }

  const handleClose = () => {
    window.desktopApi?.windowClose()
  }

  const buttonWidth = platform === 'linux' ? 'w-10' : 'w-[46px]'

  return (
    <>
      {/* Minimize */}
      <button
        onClick={handleMinimize}
        aria-label="Minimize"
        className={cn(
          buttonWidth,
          'h-full flex items-center justify-center transition-colors',
          'hover:bg-accent active:bg-accent/80',
          'focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent'
        )}
      >
        <Minus className="h-4 w-4" />
      </button>

      {/* Maximize/Restore */}
      <button
        onClick={handleMaximize}
        aria-label={isMaximized ? 'Restore' : 'Maximize'}
        className={cn(
          buttonWidth,
          'h-full flex items-center justify-center transition-colors',
          'hover:bg-accent active:bg-accent/80',
          'focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent'
        )}
      >
        {isMaximized ? (
          <Copy className="h-3 w-3" />
        ) : (
          <Square className="h-3 w-3" />
        )}
      </button>

      {/* Close */}
      <button
        onClick={handleClose}
        aria-label="Close"
        className={cn(
          buttonWidth,
          'h-full flex items-center justify-center transition-colors',
          'hover:bg-red-500/20 hover:text-red-500 active:bg-red-500/30',
          'focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-red-500'
        )}
      >
        <X className="h-4 w-4" />
      </button>
    </>
  )
}

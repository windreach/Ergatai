// src/renderer/lib/atoms/window.ts
import { atom } from 'jotai'

/**
 * Window state atoms for tracking fullscreen, focus, and maximized state.
 * Updated via IPC listeners from main process.
 */

export const isFullscreenAtom = atom<boolean>(false)
export const isFocusedAtom = atom<boolean>(true)
export const isMaximizedAtom = atom<boolean>(false)

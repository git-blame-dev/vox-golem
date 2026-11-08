import type { UiTheme } from '../types/chat'

export const UI_THEMES: readonly { value: UiTheme; label: string }[] = [
  { value: 'dark', label: 'Dark' },
  { value: 'light', label: 'Light' },
]

/** Legacy snapshots may contain slate; all new publications use dark/light. */
export function readUiTheme(value: unknown): UiTheme | null {
  if (value === 'slate') return 'dark'
  return value === 'dark' || value === 'light' ? value : null
}

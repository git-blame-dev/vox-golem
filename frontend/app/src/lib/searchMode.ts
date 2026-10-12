import type { AssistantSettings } from './assistantSettings'
import { getTauriInternals, isAndroidRuntime, isBrowserPreviewRuntime, normalizeTauriError } from './tauri'

export type SearchMode = 'quick' | 'verify' | 'research'
export const SEARCH_MODES: readonly SearchMode[] = ['quick', 'verify', 'research']
export const SEARCH_MODE_LABELS: Record<SearchMode, string> = { quick: 'Quick', verify: 'Verify', research: 'Research' }

export function searchModeAvailability(settings: AssistantSettings, modelAvailable: boolean): { available: boolean; reason?: string } {
  if (!settings.webEnabled) return { available: false, reason: 'Web is off. Normal chat remains available; Verify and Research require web access.' }
  if (settings.deepEnabled && settings.deep.startsWith('opencode-')) {
    return { available: false, reason: 'OpenCode Deep owns research for this turn.' }
  }
  if (!['custom-sol-high', 'custom-sol-none', 'custom-luna-none', 'custom-luna-low'].includes(settings.instant)) {
    return { available: false, reason: 'Native search requires a supported Custom model.' }
  }
  if (!modelAvailable) return { available: false, reason: 'The selected Custom model is unavailable.' }
  return { available: true }
}

export function admittedSearchMode(mode: SearchMode, settings: AssistantSettings, modelAvailable: boolean): SearchMode {
  return searchModeAvailability(settings, modelAvailable).available ? mode : 'quick'
}

export async function selectNativeSearchMode(mode: SearchMode): Promise<void> {
  if (isBrowserPreviewRuntime()) return
  const runtime = getTauriInternals()
  if (runtime === null) throw new Error('Search mode selection requires a runtime adapter')
  try { await runtime.invoke('set_search_mode', { mode }) }
  catch (error) { throw isAndroidRuntime ? normalizeTauriError(error) : error }
}

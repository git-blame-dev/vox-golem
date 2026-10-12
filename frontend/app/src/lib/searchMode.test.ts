import { afterEach, describe, expect, it } from 'vitest'
import { DEFAULT_ASSISTANT_SETTINGS } from './assistantSettings'
import { admittedSearchMode, searchModeAvailability, selectNativeSearchMode } from './searchMode'

afterEach(() => { delete window.__TAURI_INTERNALS__ })

describe('native search mode routing', () => {
  const settings = { ...DEFAULT_ASSISTANT_SETTINGS, instant: 'custom-sol-high' as const }

  it.each(['custom-sol-high', 'custom-sol-none', 'custom-luna-none', 'custom-luna-low'] as const)('allows exactly the supported Custom alias %s', (instant) => {
    expect(searchModeAvailability({ ...settings, instant }, true).available).toBe(true)
  })

  it('keeps a selected but disabled OpenCode Deep model from blocking native search', () => {
    expect(searchModeAvailability(settings, true).available).toBe(true)
    const delegated = { ...settings, deepEnabled: true }
    expect(searchModeAvailability(delegated, true)).toEqual({ available: false, reason: 'OpenCode Deep owns research for this turn.' })
    expect(admittedSearchMode('research', delegated, true)).toBe('quick')
    expect(searchModeAvailability({ ...delegated, deep: 'custom-sol-high' }, true).available).toBe(true)
  })

  it('gates unavailable providers and unsupported routes without sending a non-Quick policy', () => {
    expect(searchModeAvailability(settings, false).available).toBe(false)
    for (const instant of ['local-fast', 'local-quality', 'opencode-sol-high', 'opencode-luna-low'] as const) {
      const unsupported = { ...settings, instant }
      expect(searchModeAvailability(unsupported, true).available).toBe(false)
      expect(admittedSearchMode('verify', unsupported, true)).toBe('quick')
    }
    expect(admittedSearchMode('verify', settings, true)).toBe('verify')
  })

  it('updates the native transient voice mode without persisting preferences', async () => {
    const calls: unknown[] = []
    window.__TAURI_INTERNALS__ = { invoke: async (command, args) => { calls.push([command, args]); return null } }
    await selectNativeSearchMode('research')
    expect(calls).toEqual([['set_search_mode', { mode: 'research' }]])
  })

  it('is safe and local in browser preview', async () => {
    await expect(selectNativeSearchMode('verify')).resolves.toBeUndefined()
  })
})

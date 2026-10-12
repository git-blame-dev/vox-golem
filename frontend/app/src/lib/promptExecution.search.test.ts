import { afterEach, describe, expect, it, vi } from 'vitest'
import type { PromptExecutionEvent } from '../types/chat'

afterEach(() => { delete window.__TAURI_INTERNALS__; vi.unstubAllEnvs(); vi.resetModules() })

describe('Android search prompt contract', () => {
  it('sets the transient Android voice mode through the native command without a preference write', async () => {
    vi.stubEnv('VITE_VOXGOLEM_RUNTIME', 'android')
    vi.resetModules()
    const { selectNativeSearchMode } = await import('./searchMode')
    const calls: unknown[] = []
    window.__TAURI_INTERNALS__ = { invoke: async (command, args) => { calls.push([command, args]); return null } }
    await selectNativeSearchMode('verify')
    expect(calls).toEqual([['set_search_mode', { mode: 'verify' }]])
    window.__TAURI_INTERNALS__ = { invoke: async () => { throw { code: 'busy', message: 'Synthetic request already admitted' } } }
    await expect(selectNativeSearchMode('research')).rejects.toMatchObject({ code: 'busy', message: 'Synthetic request already admitted' })
  })

  it.each(['quick', 'verify', 'research'] as const)('passes %s and keeps bounded search activity correlated to its own Channel', async (searchMode) => {
    vi.stubEnv('VITE_VOXGOLEM_RUNTIME', 'android')
    vi.resetModules()
    const { executePrompt } = await import('./promptExecution')
    const events: PromptExecutionEvent[] = []
    let update: ((message: unknown) => void) | undefined
    window.__TAURI_INTERNALS__ = {
      createChannel: <T,>(handler: (message: T) => void) => { update = handler as (message: unknown) => void; return 'synthetic-channel' },
      invoke: async (command, args) => {
        expect(command).toBe('submit_prompt')
        expect(args).toEqual({ requestId: 'synthetic-request', prompt: 'Synthetic question', searchMode, updates: 'synthetic-channel' })
        for (const message of ['Searching', 'Reading', 'Fallback', 'Partial']) update?.({ kind: 'status', message })
        update?.({ kind: 'status', message: '界'.repeat(171) })
        update?.({ kind: 'status', message: 123 })
        return { kind: 'cancelled' }
      },
    }
    await executePrompt('synthetic-request', 'Synthetic question', (event) => events.push(event), 'typed', searchMode)
    expect(events).toEqual(['Searching', 'Reading', 'Fallback', 'Partial'].map((message) => ({ requestId: 'synthetic-request', kind: 'status', message })))
  })

  it('defaults omitted Android searchMode to Quick', async () => {
    vi.stubEnv('VITE_VOXGOLEM_RUNTIME', 'android')
    vi.resetModules()
    const { executePrompt } = await import('./promptExecution')
    window.__TAURI_INTERNALS__ = {
      createChannel: () => 'synthetic-channel',
      invoke: async (_command, args) => { expect(args).toMatchObject({ searchMode: 'quick' }); return { kind: 'cancelled' } },
    }
    await executePrompt('synthetic-request', 'Synthetic question', () => undefined)
  })
})

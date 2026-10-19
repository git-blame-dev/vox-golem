import { afterEach, describe, expect, it, vi } from 'vitest'
import type { PromptExecutionEvent } from '../types/chat'

afterEach(() => { delete window.__TAURI_INTERNALS__; vi.unstubAllEnvs(); vi.resetModules() })

describe('Android search prompt contract', () => {
  it('submits the verbatim prompt without mode and keeps bounded search activity correlated to its own Channel', async () => {
    vi.stubEnv('VITE_VOXGOLEM_RUNTIME', 'android')
    vi.resetModules()
    const { executePrompt } = await import('./promptExecution')
    const events: PromptExecutionEvent[] = []
    let update: ((message: unknown) => void) | undefined
    window.__TAURI_INTERNALS__ = {
      createChannel: <T,>(handler: (message: T) => void) => { update = handler as (message: unknown) => void; return 'synthetic-channel' },
      invoke: async (command, args) => {
        expect(command).toBe('submit_prompt')
        expect(args).toEqual({ requestId: 'synthetic-request', prompt: 'Synthetic question', updates: 'synthetic-channel' })
        for (const message of ['Searching', 'Reading', 'Fallback', 'Partial']) update?.({ kind: 'status', message })
        update?.({ kind: 'status', message: '界'.repeat(171) })
        update?.({ kind: 'status', message: 123 })
        return { kind: 'cancelled' }
      },
    }
    await executePrompt('synthetic-request', 'Synthetic question', (event) => events.push(event), 'typed')
    expect(events).toEqual(['Searching', 'Reading', 'Fallback', 'Partial'].map((message) => ({ requestId: 'synthetic-request', kind: 'status', message })))
  })

  it('submits Android voice without mode or a preference write', async () => {
    vi.stubEnv('VITE_VOXGOLEM_RUNTIME', 'android')
    vi.resetModules()
    const { executePrompt } = await import('./promptExecution')
    window.__TAURI_INTERNALS__ = {
      createChannel: () => 'synthetic-channel',
      invoke: async (command, args) => {
        expect(command).toBe('submit_prompt')
        expect(args).toEqual({ requestId: 'synthetic-request', prompt: 'Synthetic question', updates: 'synthetic-channel' })
        return { kind: 'cancelled' }
      },
    }
    await executePrompt('synthetic-request', 'Synthetic question', () => undefined, 'voice')
  })
})

import { StrictMode, act } from 'react'
import { createRoot } from 'react-dom/client'
import type { Root } from 'react-dom/client'
import { afterEach, describe, expect, it, vi } from 'vitest'

let root: Root | null = null
let container: HTMLElement | null = null

afterEach(() => {
  act(() => root?.unmount())
  root = null
  container?.remove()
  container = null
  delete window.__TAURI_INTERNALS__
  vi.useRealTimers()
  vi.unstubAllEnvs()
  vi.resetModules()
})

describe('production Android App runtime', () => {
  it('recovers hydration through the real cache-only adapter under StrictMode', async () => {
    vi.stubEnv('VITE_VOXGOLEM_RUNTIME', 'android')
    vi.resetModules()
    let snapshotCalls = 0
    let startupCalls = 0
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          if (snapshotCalls === 1) throw new Error('transient initial snapshot failure')
          return wireSnapshot()
        }
        if (command === 'get_startup_state') {
          startupCalls += 1
          throw new Error('Android startup must use the accepted cache')
        }
        return null
      },
    }
    const { default: App } = await import('./App')

    container = document.createElement('div')
    document.body.append(container)
    root = createRoot(container)
    await act(async () => {
      root?.render(<StrictMode><App /></StrictMode>)
      await Promise.resolve()
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(snapshotCalls).toBeGreaterThanOrEqual(2)
    expect(startupCalls).toBe(0)
    const composer = container.querySelector<HTMLTextAreaElement>('#promptComposer')
    if (composer === null) throw new Error('Missing composer')
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set
      setter?.call(composer, 'Ready after recovery')
      composer.dispatchEvent(new Event('input', { bubbles: true }))
    })
    expect(container.querySelector<HTMLButtonElement>('button[type="submit"]')?.disabled).toBe(false)
  })

  it('adopts a current snapshot after reset exhausts while initial hydration is stale', async () => {
    vi.useFakeTimers()
    vi.stubEnv('VITE_VOXGOLEM_RUNTIME', 'android')
    vi.resetModules()
    let snapshotCalls = 0
    let startupCalls = 0
    const initialResolvers: Array<(value: unknown) => void> = []
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          if (snapshotCalls <= 2) {
            return new Promise((resolve) => { initialResolvers.push(resolve) })
          }
          if (snapshotCalls <= 5) throw new Error('reset snapshot unavailable')
          return wireSnapshot()
        }
        if (command === 'reset_session') return wireSnapshot()
        if (command === 'get_startup_state') {
          startupCalls += 1
          throw new Error('Android startup must use the accepted cache')
        }
        return null
      },
    }
    const { default: App } = await import('./App')

    container = document.createElement('div')
    document.body.append(container)
    root = createRoot(container)
    await act(async () => {
      root?.render(<StrictMode><App /></StrictMode>)
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(snapshotCalls).toBe(2)
    await act(async () => {
      container?.querySelector<HTMLButtonElement>('[aria-label="New Chat"]')?.click()
      await Promise.resolve()
      initialResolvers[1]?.(wireSnapshot())
      await Promise.resolve()
      await vi.advanceTimersByTimeAsync(2_000)
    })

    expect(snapshotCalls).toBeGreaterThanOrEqual(6)
    expect(startupCalls).toBe(0)
    const composer = container.querySelector<HTMLTextAreaElement>('#promptComposer')
    if (composer === null) throw new Error('Missing composer')
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set
      setter?.call(composer, 'Ready after reset recovery')
      composer.dispatchEvent(new Event('input', { bubbles: true }))
    })
    expect(container.querySelector<HTMLButtonElement>('button[type="submit"]')?.disabled).toBe(false)
  })
})

function wireSnapshot(): Record<string, unknown> {
  const capabilityIds = ['custom_provider', 'opencode', 'local_fast', 'local_quality', 'qwen_prediction', 'wake_word', 'vad', 'parakeet', 'tts', 'deep', 'review']
  return {
    session_id: '1',
    runtime_phase: 'sleeping',
    transcript: [],
    active_request: null,
    capabilities: capabilityIds.map((id) => ({
      id,
      state: id === 'custom_provider' ? 'available' : 'not_configured',
      reason: id === 'custom_provider' ? 'Ready' : 'Unavailable on Android',
      actual_provider: null,
    })),
    model_options: [
      { id: 'custom-sol-high', label: 'Sol High', available: true, unavailable_reason: null },
      { id: 'custom-luna-low', label: 'Luna Low', available: true, unavailable_reason: null },
    ],
    preferences: {
      instant: 'custom-sol-high', deep: 'custom-sol-high', review: 'custom-sol-high',
      deep_enabled: false, review_enabled: false, prefetch: false, completion: false,
      response_profile: 'fast', tts_enabled: false, ui_theme: 'dark', ui_text_size: 'medium',
    },
  }
}

import { act } from 'react'
import { createRoot } from 'react-dom/client'
import type { Root } from 'react-dom/client'
import { afterEach, describe, expect, it, vi } from 'vitest'

vi.mock('./lib/tauri', async (importOriginal) => {
  const actual = await importOriginal<typeof import('./lib/tauri')>()
  return { ...actual, isAndroidRuntime: true }
})

import App from './App'

let root: Root | null = null
let container: HTMLElement | null = null

afterEach(() => {
  vi.useRealTimers()
  act(() => root?.unmount())
  container?.remove()
  root = null
  container = null
  delete window.__TAURI_INTERNALS__
})

describe('Android App lifecycle', () => {
  it('recovers initial hydration before loading Android startup state', async () => {
    vi.useFakeTimers()
    let snapshotCalls = 0
    let startupCalls = 0
    let resolveSnapshot: (value: unknown) => void = () => undefined
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          if (snapshotCalls === 1) throw new Error('transient initial snapshot failure')
          return new Promise((resolve) => { resolveSnapshot = resolve })
        }
        if (command === 'get_startup_state') {
          startupCalls += 1
          return readyStartupState()
        }
        return null
      },
    }

    container = document.createElement('div')
    document.body.append(container)
    root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    for (let index = 0; index < 5; index += 1) {
      await act(async () => { await Promise.resolve() })
    }
    expect(snapshotCalls).toBe(1)
    expect(startupCalls).toBe(0)

    await act(async () => { await vi.advanceTimersByTimeAsync(250) })
    expect(snapshotCalls).toBe(2)
    expect(startupCalls).toBe(0)

    await act(async () => {
      resolveSnapshot(appSnapshot())
      await Promise.resolve()
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(snapshotCalls).toBe(2)
    expect(startupCalls).toBe(1)
  })

  it('restarts hydration after stale initial success and failed reset recovery', async () => {
    vi.useFakeTimers()
    let snapshotCalls = 0
    let resolveInitial: (value: unknown) => void = () => undefined
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          if (snapshotCalls === 1) return new Promise((resolve) => { resolveInitial = resolve })
          if (snapshotCalls <= 4) throw new Error('reset snapshot unavailable')
          return appSnapshot()
        }
        if (command === 'reset_session') return appSnapshot()
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'get_assistant_settings') return {
          instant: 'custom-sol-high', deep: 'custom-sol-high', review: 'custom-sol-high',
          deep_enabled: false, review_enabled: false, prefetch: false, completion: false,
        }
        if (command === 'get_ui_theme') return 'dark'
        if (command === 'get_ui_text_size') return 'medium'
        return null
      },
    }

    container = document.createElement('div')
    document.body.append(container)
    root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    await act(async () => {
      container?.querySelector<HTMLButtonElement>('[aria-label="New Chat"]')?.click()
      await Promise.resolve()
      resolveInitial(appSnapshot())
      await Promise.resolve()
      await vi.advanceTimersByTimeAsync(2_000)
    })
    expect(snapshotCalls).toBeGreaterThanOrEqual(5)
    expect(container.textContent).not.toContain('Session recovery required')
    const composer = container.querySelector<HTMLTextAreaElement>('#promptComposer')
    if (composer === null) throw new Error('Missing composer')
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set
      setter?.call(composer, 'Ready after reset recovery')
      composer.dispatchEvent(new Event('input', { bubbles: true }))
    })
    expect(container.querySelector<HTMLButtonElement>('button[type="submit"]')?.disabled).toBe(false)
  })

  it('stops obsolete hydration retries after reset recovery succeeds', async () => {
    vi.useFakeTimers()
    let snapshotCalls = 0
    let rejectInitial: (reason?: unknown) => void = () => undefined
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          if (snapshotCalls === 1) {
            return new Promise((_resolve, reject) => { rejectInitial = reject })
          }
          if (snapshotCalls === 2) return appSnapshot()
          throw new Error('obsolete initial hydration must not retry')
        }
        if (command === 'reset_session') return appSnapshot()
        if (command === 'get_startup_state') return readyStartupState()
        return null
      },
    }

    container = document.createElement('div')
    document.body.append(container)
    root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    await act(async () => {
      container?.querySelector<HTMLButtonElement>('[aria-label="New Chat"]')?.click()
      await Promise.resolve()
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(snapshotCalls).toBe(2)
    await act(async () => {
      rejectInitial(new Error('stale initial failure'))
      await Promise.resolve()
      await vi.advanceTimersByTimeAsync(1_000)
    })
    expect(snapshotCalls).toBe(2)
  })

  it('coalesces overlapping foreground control refreshes', async () => {
    let snapshotCalls = 0
    let resolveRefresh: (value: unknown) => void = () => undefined
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          if (snapshotCalls === 1) return appSnapshot()
          if (snapshotCalls === 2) {
            return new Promise((resolve) => { resolveRefresh = resolve })
          }
          throw new Error('foreground events must share one refresh')
        }
        if (command === 'get_startup_state') return readyStartupState()
        return null
      },
    }

    container = document.createElement('div')
    document.body.append(container)
    root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    for (let index = 0; index < 5; index += 1) {
      await act(async () => { await Promise.resolve() })
    }

    await act(async () => {
      window.dispatchEvent(new Event('focus'))
      window.dispatchEvent(new PageTransitionEvent('pageshow'))
      await Promise.resolve()
    })
    expect(snapshotCalls).toBe(2)

    await act(async () => {
      resolveRefresh(appSnapshot())
      await Promise.resolve()
      await Promise.resolve()
    })
    const composer = container.querySelector<HTMLTextAreaElement>('#promptComposer')
    if (composer === null) throw new Error('Missing composer')
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set
      setter?.call(composer, 'Ready after foreground refresh')
      composer.dispatchEvent(new Event('input', { bubbles: true }))
    })
    expect(container.querySelector<HTMLButtonElement>('button[type="submit"]')?.disabled).toBe(false)
  })

  it('retries one foreground refresh after failure', async () => {
    let snapshotCalls = 0
    let rejectRefresh: (reason?: unknown) => void = () => undefined
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          if (snapshotCalls === 1 || snapshotCalls === 3) return appSnapshot()
          if (snapshotCalls === 2) {
            return new Promise((_resolve, reject) => { rejectRefresh = reject })
          }
          throw new Error('foreground retry must remain bounded')
        }
        if (command === 'get_startup_state') return readyStartupState()
        return null
      },
    }

    container = document.createElement('div')
    document.body.append(container)
    root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    for (let index = 0; index < 5; index += 1) {
      await act(async () => { await Promise.resolve() })
    }

    await act(async () => {
      window.dispatchEvent(new Event('focus'))
      rejectRefresh(new Error('transient snapshot failure'))
      await Promise.resolve()
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(snapshotCalls).toBe(3)

    const composer = container.querySelector<HTMLTextAreaElement>('#promptComposer')
    if (composer === null) throw new Error('Missing composer')
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set
      setter?.call(composer, 'Ready after queued retry')
      composer.dispatchEvent(new Event('input', { bubbles: true }))
    })
    expect(container.querySelector<HTMLButtonElement>('button[type="submit"]')?.disabled).toBe(false)
  })

  it('does not retry a foreground snapshot superseded by terminal reconciliation', async () => {
    let snapshotCalls = 0
    let requestId = ''
    let finishPrompt: (value: unknown) => void = () => undefined
    let resolveForeground: (value: unknown) => void = () => undefined
    window.__TAURI_INTERNALS__ = {
      createChannel: () => ({}),
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          if (snapshotCalls === 1) return appSnapshot()
          if (snapshotCalls === 2) {
            return new Promise((resolve) => { resolveForeground = resolve })
          }
          if (snapshotCalls === 3) {
            return appSnapshot([canonicalTurn('2', '3', 'First prompt', 'First answer', requestId)])
          }
          throw new Error('a superseded foreground snapshot must not retry')
        }
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          return new Promise((resolve) => { finishPrompt = resolve })
        }
        return null
      },
    }

    container = document.createElement('div')
    document.body.append(container)
    root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    for (let index = 0; index < 5; index += 1) {
      await act(async () => { await Promise.resolve() })
    }

    const composer = container.querySelector<HTMLTextAreaElement>('#promptComposer')
    if (composer === null) throw new Error('Missing composer')
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set
      setter?.call(composer, 'First prompt')
      composer.dispatchEvent(new Event('input', { bubbles: true }))
    })
    await act(async () => {
      container?.querySelector<HTMLButtonElement>('button[type="submit"]')?.click()
      await Promise.resolve()
      window.dispatchEvent(new Event('focus'))
      await Promise.resolve()
    })
    expect(snapshotCalls).toBe(2)

    const turn = canonicalTurn('2', '3', 'First prompt', 'First answer', requestId)
    await act(async () => {
      finishPrompt({ kind: 'completed', turn })
      await Promise.resolve()
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(snapshotCalls).toBe(3)
    await act(async () => {
      resolveForeground(appSnapshot())
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(snapshotCalls).toBe(3)

    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set
      setter?.call(composer, 'Second prompt')
      composer.dispatchEvent(new Event('input', { bubbles: true }))
    })
    expect(container.querySelector<HTMLButtonElement>('button[type="submit"]')?.disabled).toBe(false)
  })

  it('recovers stale controls when terminal snapshot reconciliation is exhausted', async () => {
    vi.useFakeTimers()
    let snapshotCalls = 0
    let requestId = ''
    let finishPrompt: (value: unknown) => void = () => undefined
    let resolveForeground: (value: unknown) => void = () => undefined
    window.__TAURI_INTERNALS__ = {
      createChannel: () => ({}),
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          if (snapshotCalls === 1) return appSnapshot()
          if (snapshotCalls === 2) {
            return new Promise((resolve) => { resolveForeground = resolve })
          }
          if (snapshotCalls >= 3 && snapshotCalls <= 5) {
            throw new Error('terminal snapshot unavailable')
          }
          if (snapshotCalls === 6) return appSnapshot()
          throw new Error('control recovery must remain bounded')
        }
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          return new Promise((resolve) => { finishPrompt = resolve })
        }
        return null
      },
    }

    container = document.createElement('div')
    document.body.append(container)
    root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    for (let index = 0; index < 5; index += 1) {
      await act(async () => { await Promise.resolve() })
    }

    const composer = container.querySelector<HTMLTextAreaElement>('#promptComposer')
    if (composer === null) throw new Error('Missing composer')
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set
      setter?.call(composer, 'Fallback prompt')
      composer.dispatchEvent(new Event('input', { bubbles: true }))
    })
    await act(async () => {
      container?.querySelector<HTMLButtonElement>('button[type="submit"]')?.click()
      await Promise.resolve()
      window.dispatchEvent(new Event('focus'))
      await Promise.resolve()
    })
    expect(snapshotCalls).toBe(2)

    const turn = canonicalTurn('4', '5', 'Fallback prompt', 'Fallback answer', requestId)
    await act(async () => {
      finishPrompt({ kind: 'completed', turn })
      await Promise.resolve()
      await vi.advanceTimersByTimeAsync(750)
    })
    expect(snapshotCalls).toBe(5)

    await act(async () => {
      resolveForeground(appSnapshot())
      await Promise.resolve()
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(snapshotCalls).toBe(6)
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set
      setter?.call(composer, 'Ready after fallback recovery')
      composer.dispatchEvent(new Event('input', { bubbles: true }))
    })
    expect(container.querySelector<HTMLButtonElement>('button[type="submit"]')?.disabled).toBe(false)
    vi.useRealTimers()
  })
})

function capabilities(): Array<Record<string, unknown>> {
  return ['custom_provider', 'opencode', 'local_fast', 'local_quality', 'qwen_prediction', 'wake_word', 'vad', 'parakeet', 'tts', 'deep', 'review']
    .map((id) => ({
      id,
      state: id === 'custom_provider' ? 'available' : 'not_configured',
      reason: id === 'custom_provider' ? 'Ready' : 'Unavailable on Android',
      actual_provider: null,
    }))
}

function appSnapshot(transcript: readonly Record<string, unknown>[] = []): Record<string, unknown> {
  return {
    session_id: '1',
    runtime_phase: 'sleeping',
    transcript,
    active_request: null,
    capabilities: capabilities(),
    model_options: [
      { id: 'custom-sol-high', label: 'Sol High', available: true, unavailable_reason: null },
      { id: 'custom-luna-low', label: 'Luna Low', available: true, unavailable_reason: null },
    ],
    preferences: {
      instant: 'custom-sol-high',
      deep: 'custom-sol-high',
      review: 'custom-sol-high',
      deep_enabled: false,
      review_enabled: false,
      prefetch: false,
      completion: false,
      response_profile: 'fast',
      tts_enabled: false,
      ui_theme: 'dark',
      ui_text_size: 'medium',
    },
  }
}

function canonicalTurn(
  userId: string,
  answerId: string,
  prompt: string,
  answer: string,
  requestId: string,
): Record<string, unknown> {
  return {
    request_id: requestId,
    execution_id: { session_id: '1', generation: answerId },
    user: { id: userId, role: 'user', content: { kind: 'text', text: prompt } },
    answer: { id: answerId, role: 'assistant', content: { kind: 'text', text: answer } },
    final_stage: 'instant',
  }
}

function readyStartupState(): Record<string, unknown> {
  return {
    kind: 'ready',
    cue_asset_paths: { start_listening: 'start.wav', stop_listening: 'stop.wav' },
    runtime_phase: 'sleeping',
    voice_input_available: false,
    voice_input_error: 'Voice input is unavailable on Android',
    silence_timeout_ms: 1500,
    selected_response_profile: 'fast',
    supported_response_profiles: [],
    prompt_cancellation_available: true,
    tts_enabled: false,
    tts_output_gain_db: 3,
    capabilities: capabilities(),
  }
}

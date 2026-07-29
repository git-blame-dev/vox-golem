import { StrictMode, act } from 'react'
import { createRoot } from 'react-dom/client'
import type { Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import * as liveAudioSourceModule from './lib/liveAudioSource'
import type { StartLiveAudioSourceOptions } from './lib/liveAudioSource'
import App from './App'

const startLiveAudioSourceMock = vi.spyOn(liveAudioSourceModule, 'startLiveAudioSource')
const listAudioInputDevicesMock = vi.spyOn(liveAudioSourceModule, 'listAudioInputDevices')
  .mockResolvedValue([])

const mountedContainers: HTMLElement[] = []
const mountedRoots: Root[] = []
const originalAudio = globalThis.Audio
const originalAudioContext = globalThis.AudioContext
const originalDateNow = Date.now
const originalMathRandom = Math.random
const originalRequestAnimationFrame = window.requestAnimationFrame
const originalCancelAnimationFrame = window.cancelAnimationFrame
const originalHistoryPushState = window.history.pushState
const originalHistoryReplaceState = window.history.replaceState
const originalHistoryBack = window.history.back
const originalHistoryForward = window.history.forward
const originalBodyOverflow = document.body.style.getPropertyValue('overflow')
const originalBodyOverflowPriority = document.body.style.getPropertyPriority('overflow')
const originalBodyOverflowX = document.body.style.getPropertyValue('overflow-x')
const originalBodyOverflowXPriority = document.body.style.getPropertyPriority('overflow-x')
const originalBodyOverflowY = document.body.style.getPropertyValue('overflow-y')
const originalBodyOverflowYPriority = document.body.style.getPropertyPriority('overflow-y')

interface TestHistoryHarness {
  entries: unknown[]
  index: number
  backCalls: number
  forwardCalls: number
  deferBack: boolean
  settleBack: () => void
  pushState: typeof window.history.pushState
  replaceState: typeof window.history.replaceState
  back: typeof window.history.back
  forward: typeof window.history.forward
  setCurrentState: (state: unknown) => void
}

let historyHarness: TestHistoryHarness | null = null

function installHistoryHarness(): void {
  const initialState = window.history.state
  const entries: unknown[] = [initialState]
  let index = 0
  let pendingBack = false
  const expose = (url = window.location.href): void => originalHistoryReplaceState.call(
    window.history,
    entries[index] ?? null,
    '',
    url,
  )
  const traverse = (nextIndex: number): void => {
    if (nextIndex < 0 || nextIndex >= entries.length) return
    index = nextIndex
    harness.index = index
    expose()
    const traversedState = entries[index]
    queueMicrotask(() => window.dispatchEvent(new PopStateEvent('popstate', { state: traversedState })))
  }
  const harness: TestHistoryHarness = {
    entries,
    index,
    backCalls: 0,
    forwardCalls: 0,
    deferBack: false,
    settleBack: () => {
      if (!pendingBack) return
      pendingBack = false
      traverse(index - 1)
    },
    pushState: ((state: unknown) => {
      entries.splice(index + 1)
      entries.push(state)
      index += 1
      harness.index = index
      expose()
    }) as typeof window.history.pushState,
    replaceState: ((state: unknown, _unused: string, url?: string | URL | null) => {
      entries[index] = state
      expose(url === undefined || url === null ? window.location.href : String(url))
    }) as typeof window.history.replaceState,
    back: (() => {
      harness.backCalls += 1
      if (harness.deferBack) {
        pendingBack = true
      } else {
        traverse(index - 1)
      }
    }) as typeof window.history.back,
    forward: (() => {
      harness.forwardCalls += 1
      traverse(index + 1)
    }) as typeof window.history.forward,
    setCurrentState: (state: unknown) => {
      entries[index] = state
      expose()
    },
  }
  historyHarness = harness
  Object.defineProperty(window.history, 'pushState', { configurable: true, value: harness.pushState })
  Object.defineProperty(window.history, 'replaceState', { configurable: true, value: harness.replaceState })
  Object.defineProperty(window.history, 'back', { configurable: true, value: harness.back })
  Object.defineProperty(window.history, 'forward', { configurable: true, value: harness.forward })
  expose()
}

beforeEach(() => {
  installHistoryHarness()
})

afterEach(() => {
  for (const root of mountedRoots) {
    act(() => {
      root.unmount()
    })
  }

  for (const container of mountedContainers) {
    container.remove()
  }

  mountedRoots.length = 0
  mountedContainers.length = 0

  if (originalAudio === undefined) {
    Reflect.deleteProperty(globalThis, 'Audio')
  } else {
    Object.defineProperty(globalThis, 'Audio', {
      configurable: true,
      value: originalAudio,
    })
  }

  if (originalAudioContext === undefined) {
    Reflect.deleteProperty(globalThis, 'AudioContext')
  } else {
    Object.defineProperty(globalThis, 'AudioContext', {
      configurable: true,
      value: originalAudioContext,
    })
  }

  Reflect.deleteProperty(window, '__TAURI_INTERNALS__')
  delete document.documentElement.dataset['uiTheme']
  Date.now = originalDateNow
  Math.random = originalMathRandom
  window.requestAnimationFrame = originalRequestAnimationFrame
  window.cancelAnimationFrame = originalCancelAnimationFrame
  Object.defineProperty(window.history, 'pushState', { configurable: true, value: originalHistoryPushState })
  Object.defineProperty(window.history, 'replaceState', { configurable: true, value: originalHistoryReplaceState })
  Object.defineProperty(window.history, 'back', { configurable: true, value: originalHistoryBack })
  Object.defineProperty(window.history, 'forward', { configurable: true, value: originalHistoryForward })
  document.body.style.setProperty('overflow', originalBodyOverflow, originalBodyOverflowPriority)
  document.body.style.setProperty('overflow-x', originalBodyOverflowX, originalBodyOverflowXPriority)
  document.body.style.setProperty('overflow-y', originalBodyOverflowY, originalBodyOverflowYPriority)
  vi.useRealTimers()
  startLiveAudioSourceMock.mockReset()
  listAudioInputDevicesMock.mockReset()
  listAudioInputDevicesMock.mockResolvedValue([])
  window.localStorage.removeItem('voxgolem.audioInputDeviceId')
  originalHistoryReplaceState.call(window.history, null, '', window.location.href)
  historyHarness = null
})

describe('App', () => {
  it('auto-follows the latest message when conversation grows', async () => {
    const { container } = await renderApp()
    const composer = getComposer(container)
    const sendButton = getSendButton(container)
    const conversation = getConversation(container)
    expect(container.querySelector('.shell__header')).not.toBeNull()
    expect(conversation).toBeTruthy()
    const scrollToSpy = vi.fn()

    Object.defineProperty(conversation, 'scrollTo', {
      configurable: true,
      value: scrollToSpy,
    })

    const baselineCalls = scrollToSpy.mock.calls.length

    await act(async () => {
      setTextAreaValue(composer, 'Scroll check prompt')
    })

    await act(async () => {
      sendButton.click()
      await Promise.resolve()
    })

    expect(scrollToSpy.mock.calls.length).toBeGreaterThan(baselineCalls)
  })

  it('renders one response-first semantic shell with a single reset affordance', async () => {
    const { container } = await renderApp()
    const header = container.querySelector('header.shell__header')
    const shell = getShell(container)

    expect(header).not.toBeNull()
    expect(header?.querySelector('[data-shell-identity]')?.textContent).toContain('VoxGolem')
    expect(header?.querySelector('[data-runtime-phase]')?.textContent).toContain(getShell(container).dataset['runtimeStatus'])
    expect(header?.querySelector('[aria-label="Switch to light mode"]')).not.toBeNull()
    expect(header?.querySelector('[aria-label="Settings"]')).not.toBeNull()
    expect(header?.querySelector('button[aria-label="New Chat"]')?.textContent).toContain('New Chat')
    expect(container.querySelectorAll('button[aria-label="New Chat"]')).toHaveLength(1)
    expect(getConversation(container).hasAttribute('aria-live')).toBe(false)
    expect(shell.querySelector('main.conversation')).not.toBeNull()
    expect(container.querySelector('form.composer .prompt-composer')).not.toBeNull()
  })

  it('preserves an older reading position until Jump to latest is requested', async () => {
    const { container } = await renderApp()
    const conversation = getConversation(container)
    const scrollToSpy = vi.fn()
    Object.defineProperties(conversation, {
      clientHeight: { configurable: true, value: 400 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, value: 100, writable: true },
      scrollTo: { configurable: true, value: scrollToSpy },
    })
    await act(async () => {
      conversation.dispatchEvent(new Event('scroll', { bubbles: true }))
    })
    expect(getButtonByLabel(container, 'Jump to latest')).not.toBeNull()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Keep my reading position')
      getSendButton(container).click()
      await Promise.resolve()
    })

    expect(scrollToSpy).not.toHaveBeenCalled()
    expect(getButtonByLabel(container, 'Jump to latest')).not.toBeNull()
    await act(async () => { getButtonByLabel(container, 'Jump to latest').click() })
    expect(scrollToSpy).toHaveBeenCalledWith({ top: 1_000, behavior: 'smooth' })
    expect(container.querySelector('button[aria-label="Jump to latest"]')).toBeNull()

    const callsAfterJump = scrollToSpy.mock.calls.length
    conversation.scrollTop = 250
    await act(async () => {
      conversation.dispatchEvent(new Event('scroll', { bubbles: true }))
      setTextAreaValue(getComposer(container), 'Follow during smooth scroll')
      getSendButton(container).click()
      await Promise.resolve()
    })
    expect(scrollToSpy.mock.calls.length).toBeGreaterThan(callsAfterJump)
    expect(container.querySelector('button[aria-label="Jump to latest"]')).toBeNull()

    conversation.scrollTop = 100
    await act(async () => {
      conversation.dispatchEvent(new WheelEvent('wheel', { bubbles: true }))
      conversation.dispatchEvent(new Event('scroll', { bubbles: true }))
    })
    expect(getButtonByLabel(container, 'Jump to latest')).not.toBeNull()
    conversation.scrollTop = 600
    await act(async () => { conversation.dispatchEvent(new Event('scroll', { bubbles: true })) })
    expect(container.querySelector('button[aria-label="Jump to latest"]')).toBeNull()
  })

  it('submits from send button and renders prompt/response history', async () => {
    const { container } = await renderApp()
    const composer = getComposer(container)
    const sendButton = getSendButton(container)

    await act(async () => {
      setTextAreaValue(composer, 'Draft release notes')
    })

    await act(async () => {
      sendButton.click()
    })

    expect(container.textContent).toContain('Draft release notes')
    expect(container.textContent).toContain(
      'Browser preview only — no backend response was generated. Prompt: Draft release notes',
    )
  })

  it('keeps browser projections distinct when generated request IDs repeat', async () => {
    Date.now = () => 1_000
    Math.random = () => 0
    const { container } = await renderApp()
    for (const prompt of ['First repeated request', 'Second repeated request']) {
      await act(async () => {
        setTextAreaValue(getComposer(container), prompt)
        getSendButton(container).click()
        await Promise.resolve()
      })
    }
    expect(container.querySelectorAll('.message--user')).toHaveLength(2)
    expect(container.querySelectorAll('.message--assistant')).toHaveLength(2)
    expect(getConversation(container).textContent).toContain('First repeated request')
    expect(getConversation(container).textContent).toContain('Second repeated request')
  })

  it('starts with an empty chat transcript', async () => {
    const { container } = await renderApp()
    const conversation = getConversation(container)

    expect(conversation.textContent).toBe('')
  })

  it('hydrates the canonical transcript and preferences from one native snapshot', async () => {
    const invokedCommands: string[] = []
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        invokedCommands.push(command)
        if (command === 'get_app_snapshot') {
          return appSnapshot([canonicalTurn('7', '8', 'Hydrated question', 'Hydrated answer')], {
            instant: 'custom-sol-high',
            tts_enabled: true,
            ui_theme: 'light',
            ui_text_size: 'large',
          })
        }
        if (command === 'get_startup_state') return readyStartupState({ tts_enabled: false })
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })

    expect(getConversation(container).textContent).toContain('Hydrated question')
    expect(getConversation(container).textContent).toContain('Hydrated answer')
    expect(getShell(container).dataset['uiTheme']).toBe('light')
    expect(getShell(container).dataset['uiTextSize']).toBe('large')
    expect(container.querySelector<HTMLSelectElement>('#assistantInstantSelect')?.value).toBe('custom-sol-high')
    expect(getTtsToggle(container).checked).toBe(true)
    expect(getConversation(container).hasAttribute('aria-live')).toBe(false)
    expect(getResponseAnnouncer(container).textContent).toBe('')
    expect(invokedCommands).not.toContain('get_ui_text_size')
    expect(invokedCommands).not.toContain('get_ui_theme')
    expect(invokedCommands).not.toContain('get_assistant_settings')
  })

  it('uses snapshot model labels and capability availability instead of stale startup controls', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') return appSnapshot([], { instant: 'custom-sol-high' }, {
          capabilities: completeCapabilities({ custom_provider: 'unavailable' }).map((capability) =>
            capability['id'] === 'custom_provider' ? { ...capability, reason: 'Snapshot provider unavailable' } : capability),
          model_options: [{ id: 'custom-sol-high', label: 'Snapshot Custom Sol', available: false, unavailable_reason: 'Snapshot provider unavailable' }],
        })
        if (command === 'get_startup_state') return readyStartupState()
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    await act(async () => setTextAreaValue(getComposer(container), 'Must remain blocked'))
    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })
    const selected = container.querySelector<HTMLOptionElement>('#assistantInstantSelect option[value="custom-sol-high"]')
    expect(selected?.textContent).toContain('Snapshot Custom Sol')
    expect(selected?.disabled).toBe(true)
    expect(selected?.title).toBe('Snapshot provider unavailable')
    await closeSettings(container)
    expect(getSendButton(container).disabled).toBe(true)
  })

  it('does not announce unavailable capabilities from a stale startup poll', async () => {
    let resolveStartup: (value: unknown) => void = () => undefined
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') return appSnapshot()
        if (command === 'get_startup_state') return new Promise((resolve) => { resolveStartup = resolve })
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      resolveStartup(readyStartupState({
        capabilities: completeCapabilities({ local_fast: 'unavailable' }).map((capability) =>
          capability['id'] === 'local_fast' ? { ...capability, reason: 'stale local failure' } : capability),
      }))
      await Promise.resolve()
      await Promise.resolve()
      setTextAreaValue(getComposer(container), 'Snapshot remains available')
    })
    expect(getSendButton(container).disabled).toBe(false)
    expect(container.textContent).not.toContain('stale local failure')
  })

  it('retains detailed snapshot model reasons through later startup polls', async () => {
    vi.useFakeTimers()
    const unavailableCapabilities = completeCapabilities({ custom_provider: 'unavailable' }).map((capability) =>
      capability['id'] === 'custom_provider' ? { ...capability, reason: 'Detailed provider authentication failure' } : capability)
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') return appSnapshot([], { instant: 'custom-sol-high' }, {
          capabilities: unavailableCapabilities,
          model_options: [{ id: 'custom-sol-high', label: 'Snapshot Custom Sol', available: false, unavailable_reason: 'Detailed provider authentication failure' }],
        })
        if (command === 'get_startup_state') return readyStartupState({ capabilities: unavailableCapabilities })
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1_000)
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })
    const selected = container.querySelector<HTMLOptionElement>('#assistantInstantSelect option[value="custom-sol-high"]')
    expect(selected?.title).toBe('Detailed provider authentication failure')
  })

  it('disables a model omitted from the authoritative snapshot', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') return appSnapshot([], { instant: 'local-fast' }, {
          model_options: [{ id: 'custom-sol-high', label: 'Custom only', available: true, unavailable_reason: null }],
        })
        if (command === 'get_startup_state') return readyStartupState()
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => setTextAreaValue(getComposer(container), 'Unavailable omitted model'))
    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })
    const localFast = container.querySelector<HTMLOptionElement>('#assistantInstantSelect option[value="local-fast"]')
    expect(localFast?.disabled).toBe(true)
    expect(localFast?.title).toBe('Unavailable in the current app snapshot')
    await closeSettings(container)
    expect(getSendButton(container).disabled).toBe(true)
  })

  it('does not promote an unavailable snapshot model during later startup polls', async () => {
    vi.useFakeTimers()
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') return appSnapshot([], { instant: 'custom-sol-high' }, {
          model_options: [
            { id: 'custom-sol-high', label: 'Custom Sol', available: true, unavailable_reason: null },
            { id: 'custom-luna-low', label: 'Custom Luna', available: false, unavailable_reason: 'Luna is not supported' },
          ],
        })
        if (command === 'get_startup_state') return readyStartupState()
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1_000)
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })
    const sol = container.querySelector<HTMLOptionElement>('#assistantInstantSelect option[value="custom-sol-high"]')
    const luna = container.querySelector<HTMLOptionElement>('#assistantInstantSelect option[value="custom-luna-low"]')
    expect(sol?.disabled).toBe(false)
    expect(luna?.disabled).toBe(true)
    expect(luna?.title).toBe('Luna is not supported')
  })

  it('refreshes authoritative model options after a successful local retry', async () => {
    let snapshotCalls = 0
    let startupCalls = 0
    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          return snapshotCalls === 1
            ? appSnapshot([], { instant: 'local-fast' }, {
                capabilities: completeCapabilities({ local_fast: 'failed' }),
                model_options: [{ id: 'local-fast', label: 'Local: Fast', available: false, unavailable_reason: 'startup failed' }],
              })
            : appSnapshot()
        }
        if (command === 'get_startup_state') {
          startupCalls += 1
          return readyStartupState({
            capabilities: completeCapabilities(startupCalls === 1 ? { local_fast: 'failed' } : {}),
          })
        }
        if (command === 'switch_response_profile') return { selected_response_profile: 'fast' }
        if (command === 'set_assistant_settings') return (args as { settings: unknown }).settings
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      getButtonByLabel(container, 'Retry local profile').click()
      for (let index = 0; index < 8; index += 1) await Promise.resolve()
    })
    expect(snapshotCalls).toBe(2)
    expect(container.querySelector('[aria-label="Retry local profile"]')).toBeNull()
    await closeSettings(container)
    await act(async () => setTextAreaValue(getComposer(container), 'Available after retry'))
    expect(getSendButton(container).disabled).toBe(false)
  })

  it('stops active microphone capture when a delayed snapshot removes voice capability', async () => {
    let resolveSnapshot: (value: unknown) => void = () => undefined
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    let ingestCalls = 0
    const stop = vi.fn()
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') return new Promise((resolve) => { resolveSnapshot = resolve })
        if (command === 'get_startup_state') return readyStartupState({ voice_input_available: true })
        if (command === 'ingest_audio_frame') {
          ingestCalls += 1
          return {
            runtime_phase: 'sleeping', transcription_ready_samples: null, transcript_text: null,
            last_activity_ms: null, capturing_utterance: false, preroll_samples: 0, utterance_samples: 0,
          }
        }
        return null
      },
    }

    const { container } = await renderApp()
    expect(getControlButton(container, 'Stop mic').disabled).toBe(false)
    const frame = onFrame as unknown as (samples: readonly number[]) => Promise<void> | void
    await act(async () => {
      resolveSnapshot(appSnapshot([], {}, {
        capabilities: completeCapabilities({ wake_word: 'unavailable' }),
      }))
      for (let index = 0; index < 5; index += 1) await Promise.resolve()
      await frame([0.2, -0.2])
    })

    expect(stop).toHaveBeenCalledTimes(1)
    expect(Array.from(container.querySelectorAll<HTMLButtonElement>('button')).filter((button) => button.textContent?.trim() === 'Start mic')).toHaveLength(0)
    await act(async () => { await frame([0.2, -0.2]); await Promise.resolve() })
    expect(ingestCalls).toBe(0)
  })

  it('rejects pending microphone capture when a delayed snapshot removes voice capability', async () => {
    let resolveSnapshot: (value: unknown) => void = () => undefined
    let resolveSource: (source: { stop: () => void }) => void = () => undefined
    const stop = vi.fn()
    startLiveAudioSourceMock.mockImplementation(() => new Promise((resolve) => { resolveSource = resolve }))
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') return new Promise((resolve) => { resolveSnapshot = resolve })
        if (command === 'get_startup_state') return readyStartupState({ voice_input_available: true })
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      resolveSnapshot(appSnapshot([], {}, {
        capabilities: completeCapabilities({ parakeet: 'unavailable' }),
      }))
      await Promise.resolve()
      await Promise.resolve()
    })
    await act(async () => {
      resolveSource({ stop })
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(stop).toHaveBeenCalledTimes(1)
    expect(Array.from(container.querySelectorAll<HTMLButtonElement>('button')).filter((button) => button.textContent?.trim() === 'Start mic')).toHaveLength(0)
  })

  it.each(['resolve', 'reject'] as const)('ignores an in-flight microphone result after capability loss: %s', async (settlement) => {
    let resolveSnapshot: (value: unknown) => void = () => undefined
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    let resolveIngest: (value: unknown) => void = () => undefined
    let rejectIngest: (error: Error) => void = () => undefined
    let submitCalls = 0
    const stop = vi.fn()
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') return new Promise((resolve) => { resolveSnapshot = resolve })
        if (command === 'get_startup_state') return readyStartupState({ voice_input_available: true })
        if (command === 'ingest_audio_frame') {
          return new Promise((resolve, reject) => {
            resolveIngest = resolve
            rejectIngest = reject
          })
        }
        if (command === 'submit_prompt') { submitCalls += 1; return null }
        return null
      },
    }

    const { container } = await renderApp()
    const frame = onFrame as unknown as (samples: readonly number[]) => Promise<void>
    await act(async () => {
      const frameResult = frame([0.2, -0.2])
      await Promise.resolve()
      resolveSnapshot(appSnapshot([], {}, {
        capabilities: completeCapabilities({ vad: 'unavailable' }),
      }))
      for (let index = 0; index < 5; index += 1) await Promise.resolve()
      if (settlement === 'resolve') {
        resolveIngest({
          runtime_phase: 'listening', transcription_ready_samples: null, transcript_text: null,
          last_activity_ms: null, capturing_utterance: true, preroll_samples: 4, utterance_samples: 4,
        })
      } else {
        rejectIngest(new Error('stale ingest failed'))
      }
      await frameResult
    })

    expect(stop).toHaveBeenCalledTimes(1)
    expect(getShell(container).dataset['runtimeStatus']).toBe('sleeping')
    expect(container.textContent).not.toContain('Runtime control failed')
    expect(container.textContent).not.toContain('stale ingest failed')
    expect(submitCalls).toBe(0)
  })

  it('applies a delayed snapshot runtime over initial startup state', async () => {
    let resolveSnapshot: (value: unknown) => void = () => undefined
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') return new Promise((resolve) => { resolveSnapshot = resolve })
        if (command === 'get_startup_state') return readyStartupState({ runtime_phase: 'sleeping' })
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      resolveSnapshot(appSnapshot([], {}, { runtime_phase: 'listening' }))
      await Promise.resolve()
      await Promise.resolve()
      setTextAreaValue(getComposer(container), 'Blocked by snapshot runtime')
    })
    expect(getSendButton(container).disabled).toBe(true)
  })

  it('accepts partial transcription after hydrating an active listening session', async () => {
    let resolveSnapshot: (value: unknown) => void = () => undefined
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    let partialEventHandler: ((event: { payload: unknown }) => void) | undefined
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop: vi.fn() }
    })
    window.__TAURI_INTERNALS__ = {
      listen: async (event, handler) => {
        if (event === 'partial-transcription-event') partialEventHandler = handler
        return () => undefined
      },
      invoke: async (command) => {
        if (command === 'get_app_snapshot') return new Promise((resolve) => { resolveSnapshot = resolve })
        if (command === 'get_startup_state') return readyStartupState({ voice_input_available: true, runtime_phase: 'sleeping' })
        if (command === 'ingest_audio_frame') return {
          runtime_phase: 'listening', transcription_ready_samples: null, transcript_text: null,
          last_activity_ms: null, capturing_utterance: true, preroll_samples: 4, utterance_samples: 4,
        }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      resolveSnapshot(appSnapshot([], {}, { runtime_phase: 'listening' }))
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(getShell(container).dataset['runtimeStatus']).toBe('listening')
    expect(partialEventHandler).toBeDefined()
    await act(async () => {
      partialEventHandler?.({ payload: { session_id: 7, revision: 1, text: 'hydrated partial' } })
      await Promise.resolve()
    })
    expect(container.textContent).toContain('hydrated partial')

    const frame = onFrame as unknown as (samples: readonly number[]) => Promise<void> | void
    await act(async () => {
      await frame([0.2, -0.2])
      partialEventHandler?.({ payload: { session_id: 7, revision: 2, text: 'continued partial' } })
      await Promise.resolve()
    })
    expect(container.textContent).toContain('continued partial')
  })

  it('accepts partial transcription from a fallback startup listening session', async () => {
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    let partialEventHandler: ((event: { payload: unknown }) => void) | undefined
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop: vi.fn() }
    })
    window.__TAURI_INTERNALS__ = {
      listen: async (event, handler) => {
        if (event === 'partial-transcription-event') partialEventHandler = handler
        return () => undefined
      },
      invoke: async (command) => {
        if (command === 'get_app_snapshot') throw new Error('snapshot unavailable')
        if (command === 'get_startup_state') return readyStartupState({ voice_input_available: true, runtime_phase: 'listening' })
        if (command === 'get_ui_text_size') return 'medium'
        if (command === 'get_ui_theme') return 'dark'
        if (command === 'get_assistant_settings') return defaultAssistantSettings()
        if (command === 'ingest_audio_frame') return {
          runtime_phase: 'listening', transcription_ready_samples: null, transcript_text: null,
          last_activity_ms: null, capturing_utterance: true, preroll_samples: 4, utterance_samples: 4,
        }
        return null
      },
    }

    const { container } = await renderApp()
    expect(getShell(container).dataset['runtimeStatus']).toBe('listening')
    await act(async () => {
      partialEventHandler?.({ payload: { session_id: 9, revision: 1, text: 'fallback partial' } })
      await Promise.resolve()
    })
    expect(container.textContent).toContain('fallback partial')

    const frame = onFrame as unknown as (samples: readonly number[]) => Promise<void> | void
    await act(async () => {
      await frame([0.2, -0.2])
      partialEventHandler?.({ payload: { session_id: 9, revision: 2, text: 'fallback continued' } })
      await Promise.resolve()
    })
    expect(container.textContent).toContain('fallback continued')
  })

  it('does not let delayed snapshot runtime overwrite newer live activity', async () => {
    let resolveSnapshot: (value: unknown) => void = () => undefined
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    Object.defineProperty(globalThis, 'Audio', {
      configurable: true,
      value: class FakeAudio { play(): Promise<void> { return Promise.resolve() } },
    })
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop: vi.fn() }
    })
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') return new Promise((resolve) => { resolveSnapshot = resolve })
        if (command === 'get_startup_state') return readyStartupState({ voice_input_available: true, runtime_phase: 'sleeping' })
        if (command === 'ingest_audio_frame') return {
          runtime_phase: 'listening', transcription_ready_samples: null, transcript_text: null,
          last_activity_ms: 1_000, capturing_utterance: true, preroll_samples: 4, utterance_samples: 4,
        }
        return null
      },
    }

    const { container } = await renderApp()
    await openSettings(container); await act(async () => { getAutoStopToggle(container).click() })
    await closeSettings(container)
    const frame = onFrame as unknown as (samples: readonly number[]) => Promise<void> | void
    await act(async () => { await frame([0.04, -0.04]); await Promise.resolve() })
    expect(getShell(container).dataset['runtimeStatus']).toBe('listening')
    await act(async () => {
      resolveSnapshot(appSnapshot([], {}, { runtime_phase: 'sleeping' }))
      await Promise.resolve()
      await Promise.resolve()
      setTextAreaValue(getComposer(container), 'Still blocked by live runtime')
    })
    expect(getShell(container).dataset['runtimeStatus']).toBe('listening')
    expect(getSendButton(container).disabled).toBe(true)
  })

  it('reconciles a completed transcript without overwriting newer live runtime', async () => {
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    let finishPrompt: ((value: unknown) => void) | undefined
    let resolveReconciliation: (value: unknown) => void = () => undefined
    let snapshotCalls = 0
    let requestId = ''
    Object.defineProperty(globalThis, 'Audio', {
      configurable: true,
      value: class FakeAudio { play(): Promise<void> { return Promise.resolve() } },
    })
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop: vi.fn() }
    })
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          if (snapshotCalls === 1) return appSnapshot()
          return new Promise((resolve) => { resolveReconciliation = resolve })
        }
        if (command === 'get_startup_state') return readyStartupState({ voice_input_available: true })
        if (command === 'ingest_audio_frame') return {
          runtime_phase: 'listening', transcription_ready_samples: null, transcript_text: null,
          last_activity_ms: 1_000, capturing_utterance: true, preroll_samples: 4, utterance_samples: 4,
        }
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          return new Promise((resolve) => { finishPrompt = resolve })
        }
        return null
      },
    }

    const { container } = await renderApp()
    await openSettings(container); await act(async () => { getAutoStopToggle(container).click() })
    await closeSettings(container)
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Reconcile runtime prompt')
      getSendButton(container).click()
      await Promise.resolve()
      finishPrompt?.({ request_id: requestId, runtime_phase: 'sleeping', outcome: 'completed' })
      await Promise.resolve()
      await Promise.resolve()
    })
    const frame = onFrame as unknown as (samples: readonly number[]) => Promise<void> | void
    await act(async () => { await frame([0.04, -0.04]); await Promise.resolve() })
    expect(getShell(container).dataset['runtimeStatus']).toBe('listening')
    await act(async () => {
      resolveReconciliation(appSnapshot(
        [canonicalTurn('71', '72', 'Reconcile runtime prompt', 'Reconciled answer', requestId)],
        {},
        { runtime_phase: 'sleeping' },
      ))
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(getConversation(container).textContent).toContain('Reconciled answer')
    expect(getShell(container).dataset['runtimeStatus']).toBe('listening')
    await act(async () => setTextAreaValue(getComposer(container), 'Blocked by live frame'))
    expect(getSendButton(container).disabled).toBe(true)
  })

  it('uses the newest canonical turn when request IDs are reused', async () => {
    Date.now = () => 1_234
    Math.random = () => 0
    const requestId = 'request-1234-'
    let committed = false
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command) => {
        if (command === 'get_app_snapshot') {
          const oldTurn = canonicalTurn('83', '84', 'Old reused prompt', 'Old reused answer', requestId, 'text')
          return committed
            ? appSnapshot([oldTurn, canonicalTurn('85', '86', 'New reused prompt', 'New reused refusal', requestId, 'refusal')])
            : appSnapshot([oldTurn])
        }
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'submit_prompt') {
          committed = true
          return { request_id: requestId, runtime_phase: 'sleeping', outcome: 'completed' }
        }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'New reused prompt')
      getSendButton(container).click()
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(getConversation(container).textContent).toContain('New reused refusal')
    expect(getResponseAnnouncer(container).textContent).toBe('Assistant refusal: New reused refusal')
  })

  it('does not let a fallback terminal phase overwrite newer live runtime', async () => {
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    let finishPrompt: ((value: unknown) => void) | undefined
    let requestId = ''
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop: vi.fn() }
    })
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') throw new Error('snapshot unavailable')
        if (command === 'get_startup_state') return readyStartupState({ voice_input_available: true })
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        if (command === 'ingest_audio_frame') return {
          runtime_phase: 'listening', transcription_ready_samples: null, transcript_text: null,
          last_activity_ms: 1_000, capturing_utterance: true, preroll_samples: 4, utterance_samples: 4,
        }
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          return new Promise((resolve) => { finishPrompt = resolve })
        }
        return null
      },
    }

    const { container } = await renderApp()
    await openSettings(container); await act(async () => { getAutoStopToggle(container).click() })
    await closeSettings(container)
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Fallback runtime prompt')
      getSendButton(container).click()
      await Promise.resolve()
    })
    const frame = onFrame as unknown as (samples: readonly number[]) => Promise<void> | void
    await act(async () => { await frame([0.04, -0.04]); await Promise.resolve() })
    expect(getShell(container).dataset['runtimeStatus']).toBe('listening')
    await act(async () => {
      finishPrompt?.({ request_id: requestId, runtime_phase: 'sleeping', outcome: 'completed' })
      await Promise.resolve()
    })
    expect(getShell(container).dataset['runtimeStatus']).toBe('listening')
  })

  it('reconciles cancellation without overwriting newer live runtime or canonical history', async () => {
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    let finishPrompt: ((value: unknown) => void) | undefined
    let resolveReconciliation: (value: unknown) => void = () => undefined
    let snapshotCalls = 0
    let requestId = ''
    Object.defineProperty(globalThis, 'Audio', {
      configurable: true,
      value: class FakeAudio { play(): Promise<void> { return Promise.resolve() } },
    })
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop: vi.fn() }
    })
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          if (snapshotCalls === 1) {
            return appSnapshot([canonicalTurn('73', '74', 'Earlier question', 'Earlier answer')])
          }
          return new Promise((resolve) => { resolveReconciliation = resolve })
        }
        if (command === 'get_startup_state') return readyStartupState({ voice_input_available: true })
        if (command === 'ingest_audio_frame') return {
          runtime_phase: 'listening', transcription_ready_samples: null, transcript_text: null,
          last_activity_ms: 1_000, capturing_utterance: true, preroll_samples: 4, utterance_samples: 4,
        }
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          return new Promise((resolve) => { finishPrompt = resolve })
        }
        return null
      },
    }

    const { container } = await renderApp()
    await openSettings(container); await act(async () => { getAutoStopToggle(container).click() })
    await closeSettings(container)
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Cancel this projection')
      getSendButton(container).click()
      await Promise.resolve()
      finishPrompt?.({ request_id: requestId, runtime_phase: 'sleeping', outcome: 'cancelled' })
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(snapshotCalls).toBe(2)

    const frame = onFrame as unknown as (samples: readonly number[]) => Promise<void> | void
    await act(async () => { await frame([0.04, -0.04]); await Promise.resolve() })
    expect(getShell(container).dataset['runtimeStatus']).toBe('listening')
    await act(async () => {
      resolveReconciliation(appSnapshot(
        [canonicalTurn('73', '74', 'Earlier question', 'Earlier answer')],
        {},
        { runtime_phase: 'sleeping' },
      ))
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(getConversation(container).textContent).toContain('Earlier question')
    expect(getConversation(container).textContent).toContain('Earlier answer')
    expect(getConversation(container).textContent).not.toContain('Cancel this projection')
    expect(getShell(container).dataset['runtimeStatus']).toBe('listening')
    await act(async () => setTextAreaValue(getComposer(container), 'Still blocked by live runtime'))
    expect(getSendButton(container).disabled).toBe(true)
  })

  it('bounds cancellation reconciliation retries and removes only the pending projection', async () => {
    vi.useFakeTimers()
    let finishPrompt: ((value: unknown) => void) | undefined
    let snapshotCalls = 0
    let requestId = ''
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          if (snapshotCalls === 1) {
            return appSnapshot([canonicalTurn('75', '76', 'Retained question', 'Retained answer')])
          }
          throw new Error('snapshot unavailable')
        }
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          return new Promise((resolve) => { finishPrompt = resolve })
        }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Cancelled pending question')
      getSendButton(container).click()
      await Promise.resolve()
      finishPrompt?.({ request_id: requestId, runtime_phase: 'sleeping', outcome: 'cancelled' })
      await Promise.resolve()
    })
    await act(async () => {
      await vi.advanceTimersByTimeAsync(3_000)
      await Promise.resolve()
    })

    expect(snapshotCalls).toBe(4)
    expect(getConversation(container).textContent).toContain('Retained question')
    expect(getConversation(container).textContent).toContain('Retained answer')
    expect(getConversation(container).textContent).not.toContain('Cancelled pending question')
    expect(container.textContent).toContain('Session recovery required')
    await act(async () => setTextAreaValue(getComposer(container), 'Blocked until recovery'))
    expect(getSendButton(container).disabled).toBe(true)
  })

  it('does not let reconciliation overwrite a newer live-audio runtime error', async () => {
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    let rejectIngest: (error: Error) => void = () => undefined
    let finishPrompt: ((value: unknown) => void) | undefined
    let resolveReconciliation: (value: unknown) => void = () => undefined
    let requestId = ''
    let snapshotCalls = 0
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop: vi.fn() }
    })
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          if (snapshotCalls === 1) return appSnapshot()
          return new Promise((resolve) => { resolveReconciliation = resolve })
        }
        if (command === 'get_startup_state') return readyStartupState({ voice_input_available: true })
        if (command === 'ingest_audio_frame') {
          return new Promise((_, reject) => { rejectIngest = reject })
        }
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          return new Promise((resolve) => { finishPrompt = resolve })
        }
        return null
      },
    }

    const { container } = await renderApp()
    const frame = onFrame as unknown as (samples: readonly number[]) => Promise<void> | void
    let framePromise: Promise<void> = Promise.resolve()
    await act(async () => {
      framePromise = Promise.resolve(frame([0.04, -0.04])).catch(() => undefined)
      await Promise.resolve()
    })
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Reconcile after audio failure')
      getSendButton(container).click()
      await Promise.resolve()
      finishPrompt?.({ request_id: requestId, runtime_phase: 'sleeping', outcome: 'completed' })
      await Promise.resolve()
      await Promise.resolve()
    })
    await act(async () => {
      rejectIngest(new Error('audio ingest failed'))
      await framePromise
    })
    expect(getShell(container).dataset['runtimeStatus']).toBe('error')

    await act(async () => {
      resolveReconciliation(appSnapshot(
        [canonicalTurn('77', '78', 'Reconcile after audio failure', 'Canonical answer', requestId)],
        {},
        { runtime_phase: 'sleeping' },
      ))
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(getShell(container).dataset['runtimeStatus']).toBe('error')
  })

  it('does not let reconciliation overwrite a newer profile transition', async () => {
    let finishPrompt: ((value: unknown) => void) | undefined
    let resolveReconciliation: (value: unknown) => void = () => undefined
    let requestId = ''
    let snapshotCalls = 0
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          if (snapshotCalls === 1) return appSnapshot()
          return new Promise((resolve) => { resolveReconciliation = resolve })
        }
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          return new Promise((resolve) => { finishPrompt = resolve })
        }
        if (command === 'switch_response_profile') return new Promise(() => undefined)
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Reconcile during profile switch')
      getSendButton(container).click()
      await Promise.resolve()
      finishPrompt?.({ request_id: requestId, runtime_phase: 'sleeping', outcome: 'completed' })
      await Promise.resolve()
      await Promise.resolve()
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
      setSelectValue(await getResponseProfileSelect(container), 'quality')
      await Promise.resolve()
    })
    expect(getShell(container).dataset['runtimeStatus']).toBe('initializing')

    await act(async () => {
      resolveReconciliation(appSnapshot(
        [canonicalTurn('79', '80', 'Reconcile during profile switch', 'Canonical answer', requestId)],
        {},
        { runtime_phase: 'sleeping' },
      ))
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(getShell(container).dataset['runtimeStatus']).toBe('initializing')
  })

  it('reconciles a rejected prompt without overwriting newer live runtime or canonical history', async () => {
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    let rejectPrompt: (error: Error) => void = () => undefined
    let resolveReconciliation: (value: unknown) => void = () => undefined
    let snapshotCalls = 0
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop: vi.fn() }
    })
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          if (snapshotCalls === 1) {
            return appSnapshot([canonicalTurn('81', '82', 'Earlier rejection question', 'Earlier rejection answer')])
          }
          return new Promise((resolve) => { resolveReconciliation = resolve })
        }
        if (command === 'get_startup_state') return readyStartupState({ voice_input_available: true })
        if (command === 'ingest_audio_frame') return {
          runtime_phase: 'listening', transcription_ready_samples: null, transcript_text: null,
          last_activity_ms: 1_000, capturing_utterance: true, preroll_samples: 4, utterance_samples: 4,
        }
        if (command === 'submit_prompt') {
          return new Promise((_, reject) => { rejectPrompt = reject })
        }
        return null
      },
    }

    const { container } = await renderApp()
    await openSettings(container); await act(async () => { getAutoStopToggle(container).click() })
    await closeSettings(container)
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Rejected pending question')
      getSendButton(container).click()
      await Promise.resolve()
      rejectPrompt(new Error('provider disconnected'))
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(snapshotCalls).toBe(2)

    const frame = onFrame as unknown as (samples: readonly number[]) => Promise<void> | void
    await act(async () => { await frame([0.04, -0.04]); await Promise.resolve() })
    expect(getShell(container).dataset['runtimeStatus']).toBe('listening')
    await act(async () => {
      resolveReconciliation(appSnapshot(
        [canonicalTurn('81', '82', 'Earlier rejection question', 'Earlier rejection answer')],
        {},
        { runtime_phase: 'sleeping' },
      ))
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(getConversation(container).textContent).toContain('Earlier rejection answer')
    expect(getConversation(container).textContent).not.toContain('Rejected pending question')
    expect(getShell(container).dataset['runtimeStatus']).toBe('listening')
    expect(container.textContent).toContain('provider disconnected')
  })

  it.each(['text', 'refusal'] as const)('accepts a newly committed canonical %s turn after transport rejection', async (kind) => {
    let snapshotCalls = 0
    let requestId = ''
    let synthesizeCalls = 0
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          return snapshotCalls === 1
            ? appSnapshot([], { tts_enabled: true })
            : appSnapshot([
                canonicalTurn('191', '192', 'Committed after rejection', 'Canonical accepted answer', requestId, kind),
              ], { tts_enabled: true })
        }
        if (command === 'get_startup_state') return readyStartupState({ tts_enabled: true })
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          throw new Error('transport disconnected after commit')
        }
        if (command === 'synthesize_local_tts') {
          synthesizeCalls += 1
          return { pcm_f32: [], sample_rate_hz: 24_000 }
        }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Committed after rejection')
      getSendButton(container).click()
      for (let index = 0; index < 8; index += 1) await Promise.resolve()
    })

    expect(getConversation(container).textContent).toContain('Canonical accepted answer')
    expect(container.textContent).not.toContain('Response failed')
    expect(getResponseAnnouncer(container).textContent).toBe(
      `${kind === 'refusal' ? 'Assistant refusal' : 'Assistant response'}: Canonical accepted answer`,
    )
    expect(synthesizeCalls).toBe(1)
  })

  it('does not accept an old canonical turn with a reused request ID', async () => {
    Date.now = () => 2_000
    Math.random = () => 0
    const requestId = 'request-2000-'
    const oldTurn = canonicalTurn('201', '202', 'Old reused question', 'Old reused answer', requestId)
    let synthesizeCalls = 0
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command) => {
        if (command === 'get_app_snapshot') return appSnapshot([oldTurn], { tts_enabled: true })
        if (command === 'get_startup_state') return readyStartupState({ tts_enabled: true })
        if (command === 'submit_prompt') throw new Error('transport failed without commit')
        if (command === 'synthesize_local_tts') { synthesizeCalls += 1; return null }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'New reused question')
      getSendButton(container).click()
      for (let index = 0; index < 8; index += 1) await Promise.resolve()
    })

    expect(getConversation(container).textContent).toContain('Old reused answer')
    expect(container.textContent).toContain('Response failed')
    expect(getResponseAnnouncer(container).textContent).toBe('')
    expect(synthesizeCalls).toBe(0)
  })

  it.each(['cancelled', 'error', 'completed'] as const)('does not speak an uncommitted correction after %s settlement', async (outcome) => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let finishPrompt: (value: unknown) => void = () => undefined
    let requestId = ''
    let snapshotCalls = 0
    let synthesizeCalls = 0
    window.__TAURI_INTERNALS__ = {
      listen: async (_event, handler) => { promptEventHandler = handler; return () => undefined },
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          return appSnapshot([], { tts_enabled: true })
        }
        if (command === 'get_startup_state') return readyStartupState({ tts_enabled: true })
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          return new Promise((resolve) => { finishPrompt = resolve })
        }
        if (command === 'synthesize_local_tts') { synthesizeCalls += 1; return null }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Correction without commit')
      getSendButton(container).click()
      await Promise.resolve()
      promptEventHandler?.({
        payload: {
          request_id: requestId,
          kind: 'correction',
          stage: 'deep',
          text: 'Provisional corrected answer',
          correction: 'Correction: provisional',
        },
      })
      finishPrompt({
        request_id: requestId,
        runtime_phase: 'sleeping',
        outcome,
        ...(outcome === 'error' ? { error_message: 'explicit failure' } : {}),
      })
      for (let index = 0; index < 8; index += 1) await Promise.resolve()
    })

    expect(snapshotCalls).toBe(2)
    expect(synthesizeCalls).toBe(0)
    expect(getResponseAnnouncer(container).textContent).toBe('')
  })

  it('speaks the canonical answer once after a provisional correction', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let requestId = ''
    let snapshotCalls = 0
    let synthesizeCalls = 0
    let synthesizedText = ''
    window.__TAURI_INTERNALS__ = {
      listen: async (_event, handler) => { promptEventHandler = handler; return () => undefined },
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          return snapshotCalls === 1
            ? appSnapshot([], { tts_enabled: true })
            : appSnapshot([
                canonicalTurn('231', '232', 'Corrected canonical prompt', 'Canonical corrected answer', requestId),
              ], { tts_enabled: true })
        }
        if (command === 'get_startup_state') return readyStartupState({ tts_enabled: true })
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          promptEventHandler?.({
            payload: {
              request_id: requestId,
              kind: 'correction',
              stage: 'deep',
              text: 'Provisional corrected answer',
              correction: 'Correction: provisional',
            },
          })
          return { request_id: requestId, runtime_phase: 'sleeping', outcome: 'completed' }
        }
        if (command === 'synthesize_local_tts') {
          synthesizeCalls += 1
          synthesizedText = (args as { text: string }).text
          return { pcm_f32: [], sample_rate_hz: 24_000 }
        }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Corrected canonical prompt')
      getSendButton(container).click()
      for (let index = 0; index < 8; index += 1) await Promise.resolve()
    })

    expect(synthesizeCalls).toBe(1)
    expect(synthesizedText).toBe('Canonical corrected answer')
    expect(getResponseAnnouncer(container).textContent).toBe('Assistant response: Canonical corrected answer')
  })

  it.each(['cancelled', 'error'] as const)('preserves explicit %s despite a new matching canonical turn', async (outcome) => {
    let requestId = ''
    let snapshotCalls = 0
    let synthesizeCalls = 0
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          return snapshotCalls === 1
            ? appSnapshot([], { tts_enabled: true })
            : appSnapshot([
                canonicalTurn('241', '242', 'Explicit terminal prompt', 'Unrelated matching turn', requestId),
              ], { tts_enabled: true })
        }
        if (command === 'get_startup_state') return readyStartupState({ tts_enabled: true })
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          return {
            request_id: requestId,
            runtime_phase: 'sleeping',
            outcome,
            ...(outcome === 'error' ? { error_message: 'authoritative failure' } : {}),
          }
        }
        if (command === 'synthesize_local_tts') { synthesizeCalls += 1; return null }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Explicit terminal prompt')
      getSendButton(container).click()
      for (let index = 0; index < 8; index += 1) await Promise.resolve()
    })

    expect(container.textContent).toContain(outcome === 'error' ? 'Response failed' : 'Response cancelled')
    expect(getResponseAnnouncer(container).textContent).toBe('')
    expect(synthesizeCalls).toBe(0)
  })

  it('reconciles an orphan transcript without overwriting newer live runtime', async () => {
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    let resolveRecovery: (value: unknown) => void = () => undefined
    let snapshotCalls = 0
    Object.defineProperty(globalThis, 'Audio', {
      configurable: true,
      value: class FakeAudio { play(): Promise<void> { return Promise.resolve() } },
    })
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop: vi.fn() }
    })
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          if (snapshotCalls === 1) return appSnapshot([], {}, {
            runtime_phase: 'executing',
            active_request: { execution_id: { session_id: '1', generation: '5' }, request_id: 'orphan-runtime', prompt: 'Orphan runtime prompt' },
          })
          return new Promise((resolve) => { resolveRecovery = resolve })
        }
        if (command === 'get_startup_state') return readyStartupState({ voice_input_available: true })
        if (command === 'cancel_prompt') return true
        if (command === 'ingest_audio_frame') return {
          runtime_phase: 'listening', transcription_ready_samples: null, transcript_text: null,
          last_activity_ms: 1_000, capturing_utterance: true, preroll_samples: 4, utterance_samples: 4,
        }
        return null
      },
    }

    const { container } = await renderApp()
    await openSettings(container); await act(async () => { getAutoStopToggle(container).click() })
    const frame = onFrame as unknown as (samples: readonly number[]) => Promise<void> | void
    await act(async () => { await frame([0.04, -0.04]); await Promise.resolve() })
    expect(getShell(container).dataset['runtimeStatus']).toBe('listening')
    await act(async () => {
      resolveRecovery(appSnapshot([], {}, { runtime_phase: 'sleeping' }))
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(getConversation(container).textContent).not.toContain('Orphan runtime prompt')
    expect(getShell(container).dataset['runtimeStatus']).toBe('listening')
  })

  it('replaces one optimistic projection with the committed snapshot turn', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let finishPrompt: ((value: unknown) => void) | undefined
    let activeRequestId = ''
    let committed = false
    window.__TAURI_INTERNALS__ = {
      listen: async (_event, handler) => {
        promptEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') {
          return committed
            ? appSnapshot([canonicalTurn('11', '12', 'Canonical question', 'Canonical answer', activeRequestId)], {}, { runtime_phase: 'listening' })
            : appSnapshot()
        }
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'get_assistant_settings') return defaultAssistantSettings()
        if (command === 'submit_prompt') {
          activeRequestId = (args as { requestId: string }).requestId
          return new Promise((resolve) => { finishPrompt = resolve })
        }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Canonical question')
      getSendButton(container).click()
      await Promise.resolve()
      promptEventHandler?.({ payload: { request_id: activeRequestId, kind: 'text', text: 'Streaming answer' } })
    })
    expect(container.querySelectorAll('.message--user')).toHaveLength(1)
    expect(container.querySelectorAll('.message--assistant')).toHaveLength(1)

    await act(async () => {
      committed = true
      finishPrompt?.({ request_id: activeRequestId, runtime_phase: 'sleeping', outcome: 'completed' })
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(container.querySelectorAll('.message--user')).toHaveLength(1)
    expect(container.querySelectorAll('.message--assistant')).toHaveLength(1)
    expect(getConversation(container).textContent?.match(/Canonical question/g)).toHaveLength(1)
    expect(getConversation(container).textContent?.match(/Canonical answer/g)).toHaveLength(1)
    expect(getConversation(container).textContent).not.toContain('Streaming answer')
    await act(async () => setTextAreaValue(getComposer(container), 'Blocked while listening'))
    expect(getSendButton(container).disabled).toBe(true)
  })

  it('does not let delayed startup hydration resurrect a reset transcript', async () => {
    let resolveInitialSnapshot: (value: unknown) => void = () => undefined
    let snapshotCalls = 0
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          if (snapshotCalls === 1) {
            return new Promise((resolve) => { resolveInitialSnapshot = resolve })
          }
          return appSnapshot()
        }
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'reset_session') return null
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })
    await closeSettings(container)
    await act(async () => {
      getNewChatButton(container).click()
      await Promise.resolve()
      await Promise.resolve()
    })
    await act(async () => {
      resolveInitialSnapshot(appSnapshot([canonicalTurn('21', '22', 'Old question', 'Old answer')]))
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(snapshotCalls).toBe(2)
    expect(getConversation(container).textContent).not.toContain('Old question')
    expect(getConversation(container).textContent).not.toContain('Old answer')
  })

  it('keeps the reset session identity when delayed initial hydration resolves later', async () => {
    let resolveInitialSnapshot: (value: unknown) => void = () => undefined
    let snapshotCalls = 0
    let requestId = ''
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          if (snapshotCalls === 1) return new Promise((resolve) => { resolveInitialSnapshot = resolve })
          if (snapshotCalls === 2) return appSnapshot([], {}, { session_id: '2' })
          return appSnapshot([
            canonicalTurn('211', '212', 'Session two prompt', 'Session two answer', requestId, 'text', 'instant', '2'),
          ], {}, { session_id: '2' })
        }
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'reset_session') return null
        if (command === 'set_tts_enabled') return { enabled: true }
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          return { request_id: requestId, runtime_phase: 'sleeping', outcome: 'completed' }
        }
        if (command === 'synthesize_local_tts') return { pcm_f32: [], sample_rate_hz: 24_000 }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      for (let index = 0; index < 6; index += 1) await Promise.resolve()
    })
    await closeSettings(container)
    await act(async () => {
      getNewChatButton(container).click()
      for (let index = 0; index < 6; index += 1) await Promise.resolve()
    })
    await act(async () => {
      resolveInitialSnapshot(appSnapshot([
        canonicalTurn('221', '222', 'Stale session one', 'Stale session answer'),
      ]))
      for (let index = 0; index < 6; index += 1) await Promise.resolve()
    })
    await closeSettings(container)
    await openSettings(container)
    await act(async () => {
      getTtsToggle(container).click()
      await Promise.resolve()
    })
    await closeSettings(container)
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Session two prompt')
      getSendButton(container).click()
      for (let index = 0; index < 8; index += 1) await Promise.resolve()
    })

    expect(getConversation(container).textContent).toContain('Session two answer')
    expect(getConversation(container).textContent).not.toContain('Stale session answer')
    expect(container.textContent).not.toContain('could not be synchronized')
    expect(getResponseAnnouncer(container).textContent).toBe('Assistant response: Session two answer')
  })

  it('waits for a new-session snapshot after a successful reset', async () => {
    vi.useFakeTimers()
    let snapshotCalls = 0
    let requestId = ''
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          if (snapshotCalls === 1) return appSnapshot()
          if (snapshotCalls === 2) throw new Error('new session snapshot not ready')
          if (snapshotCalls === 3) return appSnapshot([], {}, { session_id: '2' })
          return appSnapshot([
            canonicalTurn('251', '252', 'Prompt after reset retry', 'Answer after reset retry', requestId, 'text', 'instant', '2'),
          ], {}, { session_id: '2' })
        }
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'reset_session') return null
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          return { request_id: requestId, runtime_phase: 'sleeping', outcome: 'completed' }
        }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })
    await closeSettings(container)
    await act(async () => {
      getNewChatButton(container).click()
      await Promise.resolve()
    })
    await closeSettings(container)
    expect(getSendButton(container).disabled).toBe(true)
    await act(async () => {
      await vi.advanceTimersByTimeAsync(250)
      for (let index = 0; index < 6; index += 1) await Promise.resolve()
    })
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Prompt after reset retry')
      getSendButton(container).click()
      for (let index = 0; index < 8; index += 1) await Promise.resolve()
    })

    expect(snapshotCalls).toBe(4)
    expect(getConversation(container).textContent).toContain('Answer after reset retry')
    expect(container.textContent).not.toContain('could not be synchronized')
  })

  it('clears a legacy settings error after reset snapshot recovery', async () => {
    let snapshotCalls = 0
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          if (snapshotCalls === 1) throw new Error('initial snapshot failed')
          return appSnapshot()
        }
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'get_ui_text_size') return 'medium'
        if (command === 'get_ui_theme') return 'dark'
        if (command === 'get_assistant_settings') throw new Error('legacy settings failed')
        if (command === 'reset_session') return null
        return null
      },
    }

    const { container } = await renderApp()
    expect(container.textContent).toContain('Assistant settings unavailable: legacy settings failed')
    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      for (let index = 0; index < 8; index += 1) await Promise.resolve()
    })
    await closeSettings(container)
    await act(async () => {
      getNewChatButton(container).click()
      for (let index = 0; index < 8; index += 1) await Promise.resolve()
    })

    expect(container.textContent).not.toContain('Assistant settings unavailable: legacy settings failed')
    await openSettings(container)
    expect(container.querySelector<HTMLSelectElement>('#assistantInstantSelect')?.disabled).toBe(false)
  })

  it('blocks another prompt while retrying failed canonical reconciliation', async () => {
    vi.useFakeTimers()
    let snapshotCalls = 0
    let submitCalls = 0
    let requestId = ''
    let finishPrompt: ((value: unknown) => void) | undefined
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          if (snapshotCalls === 2) throw new Error('snapshot temporarily unavailable')
          return snapshotCalls >= 3
            ? appSnapshot([canonicalTurn('31', '32', 'First prompt', 'Committed answer', requestId)])
            : appSnapshot()
        }
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'submit_prompt') {
          submitCalls += 1
          requestId = (args as { requestId: string }).requestId
          return new Promise((resolve) => { finishPrompt = resolve })
        }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'First prompt')
      getSendButton(container).click()
      await Promise.resolve()
      finishPrompt?.({ request_id: requestId, runtime_phase: 'sleeping', outcome: 'completed' })
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(getSendButton(container).disabled).toBe(true)
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Second prompt')
      getSendButton(container).click()
      await Promise.resolve()
    })
    expect(submitCalls).toBe(1)

    await act(async () => {
      await vi.advanceTimersByTimeAsync(250)
      await Promise.resolve()
    })
    expect(snapshotCalls).toBe(3)
    expect(getConversation(container).textContent).toContain('Committed answer')
    expect(getConversation(container).textContent).not.toContain('Second prompt')
  })

  it('recovers authoritative state when reset rejects during an active prompt', async () => {
    let snapshotCalls = 0
    let requestId = ''
    startLiveAudioSourceMock.mockResolvedValue({ stop: vi.fn() })
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          return appSnapshot()
        }
        if (command === 'get_startup_state') return readyStartupState({ prompt_cancellation_available: true })
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          return new Promise(() => undefined)
        }
        if (command === 'cancel_prompt') return true
        if (command === 'reset_session') throw new Error('reset rejected')
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Reset failure prompt')
      getSendButton(container).click()
      await Promise.resolve()
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })
    await closeSettings(container)
    await act(async () => {
      getNewChatButton(container).click()
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(requestId).not.toBe('')
    expect(snapshotCalls).toBe(2)
    expect(container.textContent).toContain('New chat failed')
    expect(getConversation(container).textContent).not.toContain('Reset failure prompt')
    await closeSettings(container)
    await act(async () => setTextAreaValue(getComposer(container), 'Recovered prompt'))
    expect(getSendButton(container).disabled).toBe(false)
  })

  it('cancels and reconciles an active request found during hydration', async () => {
    let snapshotCalls = 0
    const cancelledRequests: string[] = []
    const activeRequest = {
      execution_id: { session_id: '1', generation: '9' },
      request_id: 'orphan-request',
      prompt: 'Interrupted prompt',
    }
    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          return snapshotCalls === 1
            ? appSnapshot([], {}, { runtime_phase: 'executing', active_request: activeRequest })
            : appSnapshot()
        }
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'cancel_prompt') {
          cancelledRequests.push((args as { requestId: string }).requestId)
          return true
        }
        return null
      },
    }

    const { container } = await renderApp()
    expect(cancelledRequests).toEqual(['orphan-request'])
    expect(snapshotCalls).toBe(2)
    expect(getConversation(container).textContent).not.toContain('Interrupted prompt')
    await act(async () => setTextAreaValue(getComposer(container), 'After recovery'))
    expect(getSendButton(container).disabled).toBe(false)
  })

  it('reconciles a voice prompt from a microphone callback installed before hydration', async () => {
    let resolveInitialSnapshot: (value: unknown) => void = () => undefined
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    let snapshotCalls = 0
    let requestId = ''
    let nowMs = 1_000
    Date.now = () => nowMs
    Object.defineProperty(globalThis, 'Audio', {
      configurable: true,
      value: class FakeAudio { play(): Promise<void> { return Promise.resolve() } },
    })
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop: vi.fn() }
    })
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          if (snapshotCalls === 1) return new Promise((resolve) => { resolveInitialSnapshot = resolve })
          return appSnapshot(
            [canonicalTurn('61', '62', 'Delayed voice prompt', 'Canonical voice answer', requestId)],
            { instant: 'local-quality', response_profile: 'quality' },
          )
        }
        if (command === 'get_startup_state') return readyStartupState({
          voice_input_available: true,
          selected_response_profile: 'quality',
        })
        if (command === 'ingest_audio_frame') return {
          runtime_phase: 'listening', transcription_ready_samples: null, transcript_text: null,
          last_activity_ms: 1_000, capturing_utterance: true, preroll_samples: 4, utterance_samples: 4,
        }
        if (command === 'mark_silence') return {
          runtime_phase: 'processing', transcription_ready_samples: 3200, transcript_text: 'Delayed voice prompt',
          last_activity_ms: null, capturing_utterance: false, preroll_samples: 4, utterance_samples: 0,
        }
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          return { request_id: requestId, outcome: 'completed', error_message: null, runtime_phase: 'sleeping' }
        }
        return null
      },
    }

    const { container } = await renderApp()
    expect(onFrame).not.toBeNull()
    await act(async () => {
      resolveInitialSnapshot(appSnapshot([], { instant: 'local-quality', response_profile: 'quality' }))
      await Promise.resolve()
      await Promise.resolve()
    })
    const frame = onFrame as unknown as (samples: readonly number[]) => Promise<void> | void
    await act(async () => { await frame([0.04, -0.04]); await Promise.resolve() })
    nowMs = 3_600
    await act(async () => { await frame([0.001, -0.001]); await Promise.resolve(); await Promise.resolve() })

    expect(snapshotCalls).toBe(2)
    expect(getConversation(container).textContent).toContain('Canonical voice answer')
    await act(async () => setTextAreaValue(getComposer(container), 'Next prompt'))
    expect(getSendButton(container).disabled).toBe(false)
  })

  it('does not submit voice input after delayed snapshot disables the selected model', async () => {
    let resolveSnapshot: (value: unknown) => void = () => undefined
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    let submitCalls = 0
    let nowMs = 1_000
    Date.now = () => nowMs
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop: vi.fn() }
    })
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') return new Promise((resolve) => { resolveSnapshot = resolve })
        if (command === 'get_startup_state') return readyStartupState({ voice_input_available: true })
        if (command === 'ingest_audio_frame') return {
          runtime_phase: 'listening', transcription_ready_samples: null, transcript_text: null,
          last_activity_ms: 1_000, capturing_utterance: true, preroll_samples: 4, utterance_samples: 4,
        }
        if (command === 'mark_silence') return {
          runtime_phase: 'processing', transcription_ready_samples: 3_200, transcript_text: 'Unavailable voice prompt',
          last_activity_ms: null, capturing_utterance: false, preroll_samples: 4, utterance_samples: 0,
        }
        if (command === 'submit_prompt') { submitCalls += 1; return null }
        return null
      },
    }

    await renderApp()
    await act(async () => {
      resolveSnapshot(appSnapshot([], {}, {
        capabilities: completeCapabilities({ local_fast: 'unavailable' }),
        model_options: [{ id: 'local-fast', label: 'Local: Fast', available: false, unavailable_reason: 'model unavailable' }],
      }))
      await Promise.resolve()
      await Promise.resolve()
    })
    const frame = onFrame as unknown as (samples: readonly number[]) => Promise<void> | void
    await act(async () => { await frame([0.04, -0.04]); await Promise.resolve() })
    nowMs = 3_000
    await act(async () => { await frame([0.001, -0.001]); await Promise.resolve(); await Promise.resolve() })
    expect(submitCalls).toBe(0)
  })

  it('applies delayed snapshot preferences after optimistic writes reject', async () => {
    let resolveSnapshot: (value: unknown) => void = () => undefined
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') return new Promise((resolve) => { resolveSnapshot = resolve })
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'set_ui_theme') throw new Error('theme write failed')
        if (command === 'set_ui_text_size') throw new Error('text write failed')
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      getButtonByLabel(container, 'Switch to light mode').click()
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
      getButtonByLabel(container, 'Increase text size').click()
      await Promise.resolve()
      await Promise.resolve()
    })
    await act(async () => {
      resolveSnapshot(appSnapshot([], { ui_theme: 'light', ui_text_size: 'large' }))
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(getShell(container).dataset['uiTheme']).toBe('light')
    expect(getShell(container).dataset['uiTextSize']).toBe('large')
  })

  it('applies delayed snapshot TTS after an earlier toggle rejects', async () => {
    let resolveSnapshot: (value: unknown) => void = () => undefined
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') return new Promise((resolve) => { resolveSnapshot = resolve })
        if (command === 'get_startup_state') return readyStartupState({ tts_enabled: false })
        if (command === 'set_tts_enabled') throw new Error('TTS write failed')
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      await openSettings(container)
      getTtsToggle(container).click()
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(getTtsToggle(container).checked).toBe(false)

    await act(async () => {
      resolveSnapshot(appSnapshot([], { tts_enabled: true }))
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(getTtsToggle(container).checked).toBe(true)
  })

  it('keeps a successful TTS write over delayed snapshot hydration', async () => {
    let resolveSnapshot: (value: unknown) => void = () => undefined
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') return new Promise((resolve) => { resolveSnapshot = resolve })
        if (command === 'get_startup_state') return readyStartupState({ tts_enabled: false })
        if (command === 'set_tts_enabled') return { enabled: true }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      await openSettings(container)
      getTtsToggle(container).click()
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(getTtsToggle(container).checked).toBe(true)

    await act(async () => {
      resolveSnapshot(appSnapshot([], { tts_enabled: false }))
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(getTtsToggle(container).checked).toBe(true)
  })

  it('keeps the snapshot TTS rollback baseline over an older startup response', async () => {
    let resolveStartup: (value: unknown) => void = () => undefined
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') return appSnapshot([], { tts_enabled: true })
        if (command === 'get_startup_state') return new Promise((resolve) => { resolveStartup = resolve })
        if (command === 'set_tts_enabled') throw new Error('TTS write failed')
        return null
      },
    }

    const { container } = await renderApp()
    await openSettings(container)
    expect(getTtsToggle(container).checked).toBe(true)
    await act(async () => {
      resolveStartup(readyStartupState({ tts_enabled: false }))
      await Promise.resolve()
      await Promise.resolve()
    })
    await closeSettings(container)
    await openSettings(container)
    expect(getTtsToggle(container).checked).toBe(true)
    await act(async () => {
      getTtsToggle(container).click()
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(getTtsToggle(container).checked).toBe(true)
  })

  it('accepts a post-snapshot TTS startup transition', async () => {
    vi.useFakeTimers()
    let startupCalls = 0
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') return appSnapshot([], { tts_enabled: false })
        if (command === 'get_startup_state') {
          startupCalls += 1
          return startupCalls === 1
            ? { ...readyStartupState({ tts_enabled: false }), kind: 'warming_model', message: 'Starting TTS' }
            : readyStartupState({ tts_enabled: true })
        }
        return null
      },
    }

    const { container } = await renderApp()
    await openSettings(container)
    expect(getTtsToggle(container).checked).toBe(false)
    await act(async () => {
      await vi.advanceTimersByTimeAsync(500)
      await Promise.resolve()
    })
    expect(startupCalls).toBeGreaterThanOrEqual(2)
    expect(getTtsToggle(container).checked).toBe(true)
  })

  it('reverses a pending TTS toggle and rolls back to the earlier successful write', async () => {
    let resolveFirstWrite: (value: unknown) => void = () => undefined
    let rejectSecondWrite: (error: Error) => void = () => undefined
    const requestedValues: boolean[] = []
    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') return appSnapshot([], { tts_enabled: false })
        if (command === 'get_startup_state') return readyStartupState({ tts_enabled: false })
        if (command === 'set_tts_enabled') {
          requestedValues.push((args as { enabled: boolean }).enabled)
          return requestedValues.length === 1
            ? new Promise((resolve) => { resolveFirstWrite = resolve })
            : new Promise((_, reject) => { rejectSecondWrite = reject })
        }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      await openSettings(container)
      getTtsToggle(container).click()
      await Promise.resolve()
    })
    expect(getTtsToggle(container).checked).toBe(true)
    await closeSettings(container)
    await act(async () => {
      await openSettings(container)
      getTtsToggle(container).click()
      await Promise.resolve()
    })
    expect(getTtsToggle(container).checked).toBe(false)
    await act(async () => {
      resolveFirstWrite({ enabled: true })
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(requestedValues).toEqual([true, false])
    await act(async () => {
      rejectSecondWrite(new Error('second TTS write failed'))
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(getTtsToggle(container).checked).toBe(true)
  })

  it('resets browser preview history without invoking a native command', async () => {
    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Browser reset prompt')
      getSendButton(container).click()
      await Promise.resolve()
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })
    await closeSettings(container)
    await act(async () => {
      getNewChatButton(container).click()
      await Promise.resolve()
    })

    expect(getConversation(container).textContent).toBe('')
    expect(container.textContent).not.toContain('New chat failed')
  })

  it('does not reconcile a prompt result after unmount', async () => {
    let finishPrompt: ((value: unknown) => void) | undefined
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let requestId = ''
    let snapshotCalls = 0
    let scheduledFrame: FrameRequestCallback | undefined
    const requestFrame = vi.fn((callback: FrameRequestCallback) => {
      scheduledFrame = callback
      return 73
    })
    const cancelFrame = vi.fn()
    window.requestAnimationFrame = requestFrame
    window.cancelAnimationFrame = cancelFrame
    window.__TAURI_INTERNALS__ = {
      listen: async (_event, handler) => {
        promptEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          return appSnapshot()
        }
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          return new Promise((resolve) => { finishPrompt = resolve })
        }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Unmount prompt')
      getSendButton(container).click()
      await Promise.resolve()
      promptEventHandler?.({ payload: { request_id: requestId, kind: 'text', text: 'Queued output' } })
    })
    expect(requestFrame).toHaveBeenCalledTimes(1)
    const root = mountedRoots.pop()
    expect(root).toBeDefined()
    act(() => root?.unmount())
    expect(cancelFrame).toHaveBeenCalledWith(73)
    await act(async () => {
      scheduledFrame?.(16)
      finishPrompt?.({ request_id: requestId, runtime_phase: 'sleeping', outcome: 'completed' })
      await Promise.resolve()
    })

    expect(snapshotCalls).toBe(1)
  })

  it('settles a native prompt after StrictMode effect replay', async () => {
    let requestId = ''
    let finishPrompt: ((value: unknown) => void) | undefined
    let committed = false
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') {
          return committed
            ? appSnapshot([canonicalTurn('41', '42', 'Strict prompt', 'Strict answer', requestId)])
            : appSnapshot()
        }
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          return new Promise((resolve) => { finishPrompt = resolve })
        }
        return null
      },
    }

    const { container } = await renderApp(true)
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Strict prompt')
      getSendButton(container).click()
      await Promise.resolve()
      committed = true
      finishPrompt?.({ request_id: requestId, runtime_phase: 'sleeping', outcome: 'completed' })
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(getConversation(container).textContent).toContain('Strict answer')
    expect(container.textContent).not.toContain('Synchronizing response')
  })

  it('rolls back failed second preference writes to the first persisted values', async () => {
    let themeWrites = 0
    let textSizeWrites = 0
    let resolveFirstTheme: (value: unknown) => void = () => undefined
    let resolveFirstTextSize: (value: unknown) => void = () => undefined
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') return appSnapshot()
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'set_ui_theme') {
          themeWrites += 1
          if (themeWrites === 1) return new Promise((resolve) => { resolveFirstTheme = resolve })
          throw new Error('second theme write failed')
        }
        if (command === 'set_ui_text_size') {
          textSizeWrites += 1
          if (textSizeWrites === 1) return new Promise((resolve) => { resolveFirstTextSize = resolve })
          throw new Error('second text-size write failed')
        }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => { getButtonByLabel(container, 'Switch to light mode').click() })
    await act(async () => { getButtonByLabel(container, 'Switch to dark mode').click() })
    await act(async () => {
      resolveFirstTheme('light')
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(getShell(container).dataset['uiTheme']).toBe('light')

    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
      getButtonByLabel(container, 'Increase text size').click()
    })
    await act(async () => { getButtonByLabel(container, 'Increase text size').click() })
    await act(async () => {
      resolveFirstTextSize('large')
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(getShell(container).dataset['uiTextSize']).toBe('large')
  })

  it('rolls back rejected queued fallback preference writes to loaded baselines', async () => {
    let themeWrites = 0
    let textSizeWrites = 0
    let ttsWrites = 0
    let rejectFirstTheme: (error: Error) => void = () => undefined
    let rejectFirstTextSize: (error: Error) => void = () => undefined
    let rejectFirstTts: (error: Error) => void = () => undefined
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') throw new Error('snapshot unavailable')
        if (command === 'get_startup_state') return readyStartupState({ tts_enabled: false })
        if (command === 'get_ui_theme') return 'dark'
        if (command === 'get_ui_text_size') return 'medium'
        if (command === 'get_assistant_settings') return defaultAssistantSettings()
        if (command === 'set_ui_theme') {
          themeWrites += 1
          if (themeWrites === 1) return new Promise((_, reject) => { rejectFirstTheme = reject })
          throw new Error('second theme write failed')
        }
        if (command === 'set_ui_text_size') {
          textSizeWrites += 1
          if (textSizeWrites === 1) return new Promise((_, reject) => { rejectFirstTextSize = reject })
          throw new Error('second text-size write failed')
        }
        if (command === 'set_tts_enabled') {
          ttsWrites += 1
          if (ttsWrites === 1) return new Promise((_, reject) => { rejectFirstTts = reject })
          throw new Error('second TTS write failed')
        }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => { getButtonByLabel(container, 'Switch to light mode').click() })
    await act(async () => { getButtonByLabel(container, 'Switch to dark mode').click() })
    await act(async () => {
      rejectFirstTheme(new Error('first theme write failed'))
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(themeWrites).toBe(2)
    expect(getShell(container).dataset['uiTheme']).toBe('dark')

    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
      getButtonByLabel(container, 'Increase text size').click()
    })
    await act(async () => { getButtonByLabel(container, 'Increase text size').click() })
    await act(async () => {
      rejectFirstTextSize(new Error('first text-size write failed'))
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(textSizeWrites).toBe(2)
    expect(getShell(container).dataset['uiTextSize']).toBe('medium')

    await closeSettings(container)
    await openSettings(container); await act(async () => { getTtsToggle(container).click() })
    await closeSettings(container)
    await openSettings(container)
    await act(async () => { getTtsToggle(container).click() })
    await act(async () => {
      rejectFirstTts(new Error('first TTS write failed'))
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(ttsWrites).toBe(2)
    expect(getTtsToggle(container).checked).toBe(false)
  })

  it('bounds reset recovery polling and leaves Reset available for retry', async () => {
    vi.useFakeTimers()
    let snapshotCalls = 0
    let ingestCalls = 0
    let markSilenceCalls = 0
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const stop = vi.fn()
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          if (snapshotCalls === 1) return appSnapshot()
          throw new Error('snapshot unavailable')
        }
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'reset_session') throw new Error('reset rejected')
        if (command === 'ingest_audio_frame') ingestCalls += 1
        if (command === 'mark_silence') markSilenceCalls += 1
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })
    await closeSettings(container)
    await act(async () => {
      getNewChatButton(container).click()
      await Promise.resolve()
    })
    await act(async () => {
      await vi.advanceTimersByTimeAsync(750)
      await Promise.resolve()
    })

    expect(snapshotCalls).toBe(4)
    expect(container.textContent).toContain('Session recovery required')
    expect(getNewChatButton(container).disabled).toBe(false)
    await closeSettings(container)
    await act(async () => setTextAreaValue(getComposer(container), 'Blocked until retry'))
    expect(getSendButton(container).disabled).toBe(true)
    const frame = onFrame as unknown as (samples: readonly number[]) => Promise<void> | void
    await act(async () => { await frame([0.2, -0.2]); await Promise.resolve() })
    expect(stop).toHaveBeenCalled()
    expect(ingestCalls).toBe(0)
    expect(markSilenceCalls).toBe(0)
    expect(startLiveAudioSourceMock).toHaveBeenCalledTimes(1)
  })

  it.each([
    { kind: 'text' as const, finalStage: 'deep' as const, answer: 'Canonical deep answer' },
    { kind: 'refusal' as const, finalStage: 'review' as const, answer: 'Canonical refusal' },
  ])('projects canonical $kind semantics and $finalStage completion without streamed text', async ({ kind, finalStage, answer }) => {
    let requestId = ''
    let committed = false
    let synthesizeCalls = 0
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') {
          return committed
            ? appSnapshot([canonicalTurn('51', '52', 'Canonical terminal prompt', answer, requestId, kind, finalStage)], { tts_enabled: true })
            : appSnapshot([], { tts_enabled: true })
        }
        if (command === 'get_startup_state') return readyStartupState({ tts_enabled: true })
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          committed = true
          return { request_id: requestId, runtime_phase: 'sleeping', outcome: 'completed' }
        }
        if (command === 'synthesize_local_tts') {
          synthesizeCalls += 1
          return { pcm_f32: [], sample_rate_hz: 24_000 }
        }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Canonical terminal prompt')
      getSendButton(container).click()
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(getConversation(container).textContent).toContain(answer)
    expect(container.querySelector(`[data-content-kind="${kind}"]`)).not.toBeNull()
    expect(container.querySelector(`[aria-label="${finalStage[0]?.toUpperCase()}${finalStage.slice(1)} status: completed"]`)).not.toBeNull()
    expect(container.textContent).not.toContain('No response')
    expect(synthesizeCalls).toBe(1)
    expect(getResponseAnnouncer(container).textContent).toBe(
      `${kind === 'refusal' ? 'Assistant refusal' : 'Assistant response'}: ${answer}`,
    )
  })

  it('ignores late prompt events after unmount', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let requestId = ''
    let synthesizeCalls = 0
    window.__TAURI_INTERNALS__ = {
      listen: async (_event, handler) => {
        promptEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') return appSnapshot([], { tts_enabled: true })
        if (command === 'get_startup_state') return readyStartupState({ tts_enabled: true })
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          return new Promise(() => undefined)
        }
        if (command === 'synthesize_local_tts') synthesizeCalls += 1
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Late event prompt')
      getSendButton(container).click()
      await Promise.resolve()
    })
    const root = mountedRoots.pop()
    act(() => root?.unmount())
    await act(async () => {
      promptEventHandler?.({ payload: { request_id: requestId, kind: 'text', text: 'Late line\n' } })
      await Promise.resolve()
    })
    expect(synthesizeCalls).toBe(0)
  })

  it('does not continue orphan recovery after unmount', async () => {
    let resolveCancel: () => void = () => undefined
    let snapshotCalls = 0
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          return appSnapshot([], {}, {
            runtime_phase: 'executing',
            active_request: { execution_id: { session_id: '1', generation: '4' }, request_id: 'orphan', prompt: 'Orphan' },
          })
        }
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'cancel_prompt') return new Promise<void>((resolve) => { resolveCancel = resolve })
        return null
      },
    }

    await renderApp()
    const root = mountedRoots.pop()
    act(() => root?.unmount())
    await act(async () => {
      resolveCancel()
      await Promise.resolve()
    })
    expect(snapshotCalls).toBe(1)
  })

  it('does not load a reset snapshot after unmount', async () => {
    let resolveReset: () => void = () => undefined
    let snapshotCalls = 0
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          return appSnapshot()
        }
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'reset_session') return new Promise<void>((resolve) => { resolveReset = resolve })
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })
    await closeSettings(container)
    await act(async () => {
      getNewChatButton(container).click()
      await Promise.resolve()
    })
    const root = mountedRoots.pop()
    act(() => root?.unmount())
    await act(async () => {
      resolveReset()
      await Promise.resolve()
    })
    expect(snapshotCalls).toBe(1)
  })

  it('keeps the typed shell nonfatal and lists every zero-asset capability in one red notice', async () => {
    const capabilityIds = [
      'custom_provider', 'opencode', 'local_fast', 'local_quality', 'qwen_prediction',
      'wake_word', 'vad', 'parakeet', 'tts', 'deep', 'review',
    ]
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_ui_text_size') return 'medium'
        if (command === 'get_ui_theme') return 'dark'
        if (command === 'get_app_snapshot') return appSnapshot([], {}, {
          capabilities: capabilityIds.map((id) => ({
            id,
            state: id === 'review' ? 'failed' : 'not_configured',
            reason: `${id} unavailable`,
            actual_provider: null,
          })),
          model_options: [{ id: 'local-fast', label: 'Local: Fast', available: false, unavailable_reason: 'local_fast unavailable' }],
        })
        if (command === 'get_startup_state') return {
          kind: 'ready',
          cue_asset_paths: { start_listening: 'start.wav', stop_listening: 'stop.wav' },
          runtime_phase: 'sleeping',
          voice_input_available: false,
          voice_input_error: 'voice assets are not configured',
          silence_timeout_ms: 1500,
          selected_response_profile: 'fast',
          supported_response_profiles: [],
          capabilities: capabilityIds.map((id) => ({
            id,
            state: id === 'review' ? 'failed' : 'not_configured',
            reason: `${id} unavailable`,
            actual_provider: null,
          })),
        }
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const composer = getComposer(container)
    await act(async () => setTextAreaValue(composer, 'typed input remains editable'))

    expect(composer.disabled).toBe(false)
    expect(getSendButton(container).disabled).toBe(true)
    expect(container.textContent).toContain('Sending disabled:')
    const notice = container.querySelector('.notice-toast--error')
    expect(notice).not.toBeNull()
    expect(notice?.parentElement?.classList.contains('conversation-notice')).toBe(true)
    expect(notice?.parentElement?.parentElement?.classList.contains('conversation-region')).toBe(true)
    expect(container.querySelector('[aria-label="Response announcements"]')?.closest('.conversation-region')).toBeNull()
    for (const id of capabilityIds) {
      expect(container.textContent).toContain(`${id} unavailable`)
    }
    const settingsButton = getButtonByLabel(container, 'Settings')
    await act(async () => { settingsButton.click(); await Promise.resolve() })
    const instantSelect = container.querySelector<HTMLSelectElement>('#assistantInstantSelect')
    expect(instantSelect).not.toBeNull()
    expect(Array.from(instantSelect?.options ?? []).every((option) => option.disabled || option.value === 'local-fast')).toBe(true)
  })

  it('opens settings and live-updates persisted app text size', async () => {
    const invoked: Array<{ command: string; args: unknown }> = []
    let textSize = 'medium'

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        invoked.push({ command, args })

        if (command === 'get_ui_text_size') {
          return textSize
        }

        if (command === 'set_ui_text_size') {
          if (!isRecord(args) || typeof args['textSize'] !== 'string') {
            throw new Error('textSize argument is required')
          }

          textSize = args['textSize']
          return textSize
        }

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
            tts_enabled: false,
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const shell = getShell(container)

    expect(shell.dataset['uiTextSize']).toBe('medium')

    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })

    expect(container.textContent).toContain('Text size')
    expect(container.textContent).toContain('Medium (100%)')

    await act(async () => {
      getButtonByLabel(container, 'Increase text size').click()
      await Promise.resolve()
    })

    expect(shell.dataset['uiTextSize']).toBe('large')
    expect(container.textContent).toContain('Large (112.5%)')

    await act(async () => {
      getButtonByLabel(container, 'Decrease text size').click()
      await Promise.resolve()
    })

    expect(shell.dataset['uiTextSize']).toBe('medium')
    expect(container.textContent).toContain('Medium (100%)')
    expect(invoked).toContainEqual({ command: 'set_ui_text_size', args: { textSize: 'large' } })
    expect(invoked).toContainEqual({ command: 'set_ui_text_size', args: { textSize: 'medium' } })
  })

  it('keeps an immediate text-size change when delayed persisted settings load later', async () => {
    let resolvePersistedTextSize: (value: string) => void = () => undefined
    const persistedTextSize = new Promise<string>((resolve) => {
      resolvePersistedTextSize = resolve
    })
    let textSize = 'medium'

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        if (command === 'get_ui_text_size') {
          return persistedTextSize
        }

        if (command === 'set_ui_text_size') {
          if (!isRecord(args) || typeof args['textSize'] !== 'string') {
            throw new Error('textSize argument is required')
          }

          textSize = args['textSize']
          return textSize
        }

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
            tts_enabled: false,
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const shell = getShell(container)

    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })

    await act(async () => {
      getButtonByLabel(container, 'Increase text size').click()
      await Promise.resolve()
    })

    expect(shell.dataset['uiTextSize']).toBe('large')

    await act(async () => {
      resolvePersistedTextSize('small')
      await persistedTextSize
      await Promise.resolve()
    })

    expect(shell.dataset['uiTextSize']).toBe('large')
  })

  it('starts in persisted dark mode and toggles to light mode', async () => {
    const invoked: Array<{ command: string; args: unknown }> = []
    let theme = 'dark'

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        invoked.push({ command, args })

        if (command === 'get_ui_theme') {
          return theme
        }

        if (command === 'set_ui_theme') {
          if (!isRecord(args) || typeof args['theme'] !== 'string') {
            throw new Error('theme argument is required')
          }

          theme = args['theme']
          return theme
        }

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
            tts_enabled: false,
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const shell = getShell(container)

    expect(shell.dataset['uiTheme']).toBe('dark')
    expect(getButtonByLabel(container, 'Switch to light mode').textContent).toBe('☀')

    await act(async () => {
      getButtonByLabel(container, 'Switch to light mode').click()
      await Promise.resolve()
    })

    expect(shell.dataset['uiTheme']).toBe('light')
    expect(getButtonByLabel(container, 'Switch to dark mode').textContent).toBe('☾')
    expect(invoked).toContainEqual({ command: 'set_ui_theme', args: { theme: 'light' } })
  })

  it('keeps an immediate theme toggle when delayed persisted theme loads later', async () => {
    let resolvePersistedTheme: (value: string) => void = () => undefined
    const persistedTheme = new Promise<string>((resolve) => {
      resolvePersistedTheme = resolve
    })
    let theme = 'dark'

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        if (command === 'get_ui_theme') {
          return persistedTheme
        }

        if (command === 'set_ui_theme') {
          if (!isRecord(args) || typeof args['theme'] !== 'string') {
            throw new Error('theme argument is required')
          }

          theme = args['theme']
          return theme
        }

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
            tts_enabled: false,
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const shell = getShell(container)

    await act(async () => {
      getButtonByLabel(container, 'Switch to light mode').click()
      await Promise.resolve()
    })

    expect(shell.dataset['uiTheme']).toBe('light')

    await act(async () => {
      resolvePersistedTheme('dark')
      await persistedTheme
      await Promise.resolve()
    })

    expect(shell.dataset['uiTheme']).toBe('light')
  })

  it('defaults to dark mode when persisted theme is unsupported', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_ui_theme') {
          return 'solarized'
        }

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
            tts_enabled: false,
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()

    expect(getShell(container).dataset['uiTheme']).toBe('dark')
    expect(getButtonByLabel(container, 'Switch to light mode').textContent).toBe('☀')
  })

  it('reverts theme and shows a notice when theme persistence fails', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_ui_theme') {
          return 'dark'
        }

        if (command === 'set_ui_theme') {
          throw new Error('theme write failed')
        }

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
            tts_enabled: false,
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()

    await act(async () => {
      getButtonByLabel(container, 'Switch to light mode').click()
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(getShell(container).dataset['uiTheme']).toBe('dark')
    expect(container.textContent).toContain('Theme not saved')
    expect(container.textContent).toContain('theme write failed')
  })

  it('reverts text size and shows a notice when settings persistence fails', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_ui_text_size') {
          return 'medium'
        }

        if (command === 'set_ui_text_size') {
          throw new Error('state write failed')
        }

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
            tts_enabled: false,
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const shell = getShell(container)

    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })

    await act(async () => {
      getButtonByLabel(container, 'Increase text size').click()
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(shell.dataset['uiTextSize']).toBe('medium')
    expect(container.textContent).toContain('Setting not saved')
    expect(container.textContent).toContain('state write failed')
  })

  it('supports keyboard focus and Escape close for the settings overlay', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_ui_text_size') {
          return 'medium'
        }

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
            tts_enabled: false,
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const settingsButton = getButtonByLabel(container, 'Settings')

    await act(async () => {
      settingsButton.click()
      await Promise.resolve()
    })

    const closeButton = getButtonByLabel(container, 'Close settings')
    expect(document.activeElement).toBe(closeButton)

    await act(async () => {
      closeButton.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
      await new Promise((resolve) => window.setTimeout(resolve, 0))
      await new Promise((resolve) => window.setTimeout(resolve, 0))
    })

    expect(container.textContent).not.toContain('Adjust the app text size live.')
    expect(document.activeElement).toBe(settingsButton)
  })

  it('marks settings history once, exposes dialog ownership, and restores body scrolling', async () => {
    const originalOverflow = document.body.style.overflow
    document.body.style.setProperty('overflow', 'scroll', 'important')
    document.body.style.setProperty('overflow-x', 'clip', 'important')
    document.body.style.setProperty('overflow-y', 'auto', 'important')
    const { container } = await renderApp()
    const opener = getButtonByLabel(container, 'Settings')

    await act(async () => { opener.click(); await Promise.resolve() })
    expect(opener.getAttribute('aria-controls')).toBe('settings-dialog')
    expect(container.querySelector('[role="dialog"]')?.id).toBe('settings-dialog')
    expect(document.body.style.overflowX).toBe('hidden')
    expect(document.body.style.overflowY).toBe('hidden')
    expect(historyHarness?.entries).toHaveLength(2)
    expect(historyHarness?.entries[1]).toMatchObject({ __voxgolemSettingsOpen: expect.any(String) })

    await act(async () => { getButtonByLabel(container, 'Close settings').click() })
    expect(historyHarness?.backCalls).toBe(1)
    expect(document.body.style.overflow).toBe('scroll')
    expect(document.body.style.overflowX).toBe('clip')
    expect(document.body.style.overflowY).toBe('auto')
    expect(document.body.style.getPropertyPriority('overflow')).toBe('important')
    document.body.style.overflow = originalOverflow
  })

  it('closes settings on its marker popstate without changing root navigation', async () => {
    const { container } = await renderApp()
    await act(async () => { getButtonByLabel(container, 'Settings').click() })
    expect(historyHarness?.entries).toHaveLength(2)

    await act(async () => { historyHarness?.back() })
    expect(container.querySelector('[role="dialog"]')).toBeNull()
    expect(historyHarness?.backCalls).toBe(1)
    expect(historyHarness?.entries[1]).toMatchObject({ __voxgolemSettingsOpen: expect.any(String) })
    await act(async () => { await Promise.resolve() })
    await act(async () => {
      historyHarness?.forward()
      await new Promise((resolve) => window.setTimeout(resolve, 0))
    })
    expect(container.querySelector('[role="dialog"]')).not.toBeNull()
  })

  it('opens from an authoritative marked entry without pushing another marker', async () => {
    historyHarness?.setCurrentState({ existing: 'field', __voxgolemSettingsOpen: true })
    const { container } = await renderApp(true)

    expect(container.querySelector('[role="dialog"]')).not.toBeNull()
    expect(historyHarness?.entries).toHaveLength(1)
    expect(window.history.state).toEqual({ existing: 'field', __voxgolemSettingsOpen: true })
  })

  it('closes an inherited marker without traversing and can reopen with one owned marker', async () => {
    historyHarness?.setCurrentState({ root: 'state', __voxgolemSettingsOpen: 'other-mount' })
    const { container } = await renderApp()
    const opener = getButtonByLabel(container, 'Settings')

    await act(async () => { getButtonByLabel(container, 'Close settings').click() })
    expect(historyHarness?.backCalls).toBe(0)
    expect(window.history.state).toEqual({ root: 'state' })
    expect(container.querySelector('[role="dialog"]')).toBeNull()

    await act(async () => { opener.click() })
    expect(historyHarness?.entries).toHaveLength(2)
    expect(window.history.state?.['__voxgolemSettingsOpen']).toEqual(expect.any(String))
    expect(window.history.state?.['__voxgolemSettingsOpen']).not.toBe('other-mount')

    await act(async () => { getButtonByLabel(container, 'Close settings').click() })
    expect(container.querySelector('[role="dialog"]')).toBeNull()
    expect(window.history.state).toEqual({ root: 'state' })
    await act(async () => { historyHarness?.forward() })
    expect(container.querySelector('[role="dialog"]')).not.toBeNull()
  })

  it('ignores Back at a closed root entry', async () => {
    const { container } = await renderApp()
    await act(async () => { historyHarness?.back() })
    expect(historyHarness?.index).toBe(0)
    expect(container.querySelector('[role="dialog"]')).toBeNull()
  })

  it('does not overwrite a competing body overflow mutation', async () => {
    document.body.style.setProperty('overflow', 'scroll', 'important')
    const { container } = await renderApp()

    await act(async () => { getButtonByLabel(container, 'Settings').click() })
    document.body.style.setProperty('overflow', 'auto')
    await act(async () => { getButtonByLabel(container, 'Close settings').click() })

    expect(document.body.style.overflow).toBe('auto')
    expect(document.body.style.getPropertyPriority('overflow')).toBe('')
  })

  it('restores only the unmodified overflow axis', async () => {
    document.body.style.setProperty('overflow-x', 'scroll', 'important')
    document.body.style.setProperty('overflow-y', 'auto', 'important')
    const { container } = await renderApp()

    await act(async () => { getButtonByLabel(container, 'Settings').click() })
    document.body.style.setProperty('overflow-x', 'visible')
    await act(async () => { getButtonByLabel(container, 'Close settings').click() })

    expect(document.body.style.overflowX).toBe('visible')
    expect(document.body.style.overflowY).toBe('auto')
    expect(document.body.style.getPropertyPriority('overflow-y')).toBe('important')
  })

  it('restores y when only x is owned and preserves absent declarations', async () => {
    document.body.style.setProperty('overflow-y', 'scroll')
    const { container } = await renderApp()

    await act(async () => { getButtonByLabel(container, 'Settings').click() })
    document.body.style.setProperty('overflow-y', 'clip')
    await act(async () => { getButtonByLabel(container, 'Close settings').click() })

    expect(document.body.style.overflowY).toBe('clip')
    expect(document.body.style.getPropertyValue('overflow-x')).toBe('')
  })

  it('queues a rapid reopen until the pending Back traversal settles', async () => {
    if (historyHarness !== null) historyHarness.deferBack = true
    const { container } = await renderApp()
    const opener = getButtonByLabel(container, 'Settings')

    await act(async () => { opener.click() })
    await act(async () => { getButtonByLabel(container, 'Close settings').click() })
    await act(async () => { opener.click() })
    expect(historyHarness?.entries).toHaveLength(2)
    expect(historyHarness?.backCalls).toBe(1)
    expect(container.querySelector('[role="dialog"]')).toBeNull()

    await act(async () => { historyHarness?.settleBack() })
    expect(historyHarness?.entries).toHaveLength(2)
    expect(container.querySelector('[role="dialog"]')).not.toBeNull()
    expect(window.history.state?.['__voxgolemSettingsOpen']).toEqual(expect.any(String))
  })

  it('restores its body scroll lock when unmounted while settings is open', async () => {
    document.body.style.setProperty('overflow', 'scroll', 'important')
    document.body.style.setProperty('overflow-x', 'clip', 'important')
    document.body.style.setProperty('overflow-y', 'auto', 'important')
    const { container } = await renderApp()
    await act(async () => { getButtonByLabel(container, 'Settings').click() })
    expect(document.body.style.overflowX).toBe('hidden')
    expect(document.body.style.overflowY).toBe('hidden')

    const root = mountedRoots.pop()
    act(() => root?.unmount())
    expect(document.body.style.overflow).toBe('scroll')
    expect(document.body.style.overflowX).toBe('clip')
    expect(document.body.style.overflowY).toBe('auto')
    expect(document.body.style.getPropertyPriority('overflow')).toBe('important')
  })

  it('falls back to medium when persisted text size is unsupported', async () => {
    let textSize = 'giant'

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        if (command === 'get_ui_text_size') {
          return textSize
        }

        if (command === 'set_ui_text_size') {
          if (!isRecord(args) || typeof args['textSize'] !== 'string') {
            throw new Error('textSize argument is required')
          }

          textSize = args['textSize']
          return textSize
        }

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
            tts_enabled: false,
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const shell = getShell(container)

    expect(shell.dataset['uiTextSize']).toBe('medium')

    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })

    expect(container.textContent).toContain('Medium (100%)')

    await act(async () => {
      getButtonByLabel(container, 'Increase text size').click()
      await Promise.resolve()
    })

    expect(shell.dataset['uiTextSize']).toBe('large')
    expect(container.textContent).toContain('Large (112.5%)')
  })

  it('submits from Enter and ignores Shift+Enter', async () => {
    const { container } = await renderApp()
    const composer = getComposer(container)
    expect(container.querySelector('.prompt-composer')).not.toBeNull()
    expect(container.querySelector('.prompt-composer__ghost')).toBeNull()

    await act(async () => {
      setTextAreaValue(composer, 'Line one')
    })

    await act(async () => {
      composer.dispatchEvent(
        new KeyboardEvent('keydown', {
          key: 'Enter',
          shiftKey: true,
          bubbles: true,
        }),
      )
    })

    expect(container.textContent).not.toContain(
      'Browser preview only — no backend response was generated. Prompt: Line one',
    )

    await act(async () => {
      composer.dispatchEvent(
        new KeyboardEvent('keydown', {
          key: 'Enter',
          isComposing: true,
          bubbles: true,
        }),
      )
    })

    expect(container.textContent).not.toContain(
      'Browser preview only — no backend response was generated. Prompt: Line one',
    )

    await act(async () => {
      composer.dispatchEvent(
        new KeyboardEvent('keydown', {
          key: 'Enter',
          bubbles: true,
        }),
      )
    })

    expect(container.textContent).toContain(
      'Browser preview only — no backend response was generated. Prompt: Line one',
    )
  })

  it('replaces composer hint with a TTS toggle and invokes backend toggle command', async () => {
    const invoked: string[] = []

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        invoked.push(command)

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
            tts_enabled: false,
          }
        }

        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')

        if (command === 'set_tts_enabled') {
          expect(args).toEqual({ enabled: true })
          return { enabled: true, sample_rate_hz: 22050 }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    await openSettings(container)
    const ttsToggle = getTtsToggle(container)

    expect(container.textContent).not.toContain('Enter to send, Shift+Enter for newline')
    expect(ttsToggle.checked).toBe(false)

    await act(async () => {
      ttsToggle.click()
      await Promise.resolve()
    })
    await closeSettings(container)

    expect(invoked).toContain('set_tts_enabled')
    expect(ttsToggle.checked).toBe(true)
  })

  it('synthesizes assistant response when TTS is enabled', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    const invoked: string[] = []

    let resumeCallCount = 0

    class FakeAudioContext {
      destination = {} as AudioDestinationNode
      state: AudioContextState = 'suspended'

    createBuffer(): AudioBuffer {
        return {
          copyToChannel: () => {},
        } as unknown as AudioBuffer
      }

      createBufferSource(): AudioBufferSourceNode {
        return {
          buffer: null,
          connect: () => {},
          onended: null,
          start: function start(this: AudioBufferSourceNode) {
            this.onended?.(new Event('ended'))
          },
        } as unknown as AudioBufferSourceNode
      }

      createGain(): GainNode {
        return {
          gain: { value: 1 } as AudioParam,
          connect: () => {},
        } as unknown as GainNode
      }

      async resume(): Promise<void> {
        resumeCallCount += 1
        this.state = 'running'
      }

      async close(): Promise<void> {}
    }

    Object.defineProperty(globalThis, 'AudioContext', {
      configurable: true,
      value: FakeAudioContext,
    })

    window.__TAURI_INTERNALS__ = {
      listen: async (_event, handler) => {
        promptEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        invoked.push(command)

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
            tts_enabled: false,
          }
        }

        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')

        if (command === 'set_tts_enabled') {
          return { enabled: true, sample_rate_hz: 22050 }
        }

        if (command === 'submit_prompt') {
          expect(args).toMatchObject({ requestId: expect.any(String), prompt: 'Hello voice' })
          promptEventHandler?.({ payload: { request_id: (args as { requestId: string }).requestId, kind: 'text', text: 'Voice response' } })
          return {
            request_id: (args as { requestId: string }).requestId,
            outcome: 'completed',
            error_message: null,
            runtime_phase: 'sleeping',
          }
        }

        if (command === 'synthesize_local_tts') {
          expect(args).toEqual({
            text: 'Voice response',
          })
          return {
            pcm_f32: [0.0, 0.1, -0.1],
            sample_rate_hz: 22050,
            duration_ms: 1,
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    await openSettings(container)
    await closeSettings(container)
    await openSettings(container)
    const ttsToggle = getTtsToggle(container)
    await act(async () => {
      ttsToggle.click()
      await Promise.resolve()
    })
    await closeSettings(container)
    const composer = getComposer(container)
    const sendButton = getSendButton(container)

    await act(async () => {
      setTextAreaValue(composer, 'Hello voice')
    })

    await act(async () => {
      sendButton.click()
      await Promise.resolve()
    })

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0))
      await new Promise((resolve) => setTimeout(resolve, 0))
      await new Promise((resolve) => setTimeout(resolve, 10))
    })

    expect(invoked).toContain('synthesize_local_tts')
    expect(resumeCallCount).toBe(1)
  })

  it('stops active TTS playback when Escape cancels a prompt', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let finishPrompt: ((value: unknown) => void) | undefined
    let requestId = ''
    const stop = vi.fn()
    const started = vi.fn()
    class FakeAudioContext {
      destination = {} as AudioDestinationNode
      state: AudioContextState = 'running'
      createBuffer(): AudioBuffer { return { copyToChannel: () => {} } as unknown as AudioBuffer }
      createBufferSource(): AudioBufferSourceNode {
        return { buffer: null, connect: () => {}, onended: null, start: started, stop } as unknown as AudioBufferSourceNode
      }
      createGain(): GainNode { return { gain: { value: 1 } as AudioParam, connect: () => {} } as unknown as GainNode }
      async close(): Promise<void> {}
    }
    Object.defineProperty(globalThis, 'AudioContext', { configurable: true, value: FakeAudioContext })
    window.__TAURI_INTERNALS__ = {
      listen: async (_event, handler) => { promptEventHandler = handler; return () => undefined },
      invoke: async (command, args) => {
        if (command === 'get_startup_state') return readyStartupState({ prompt_cancellation_available: true, tts_enabled: false })
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        if (command === 'set_tts_enabled') return { enabled: true }
        if (command === 'submit_prompt') { requestId = (args as { requestId: string }).requestId; return new Promise((resolve) => { finishPrompt = resolve }) }
        if (command === 'synthesize_local_tts') return { pcm_f32: [0], sample_rate_hz: 22050, duration_ms: 1 }
        if (command === 'cancel_prompt') return null
        throw new Error(`unexpected command: ${command}`)
      },
    }
    const { container } = await renderApp()
    await openSettings(container)
    await act(async () => { const toggle = getTtsToggle(container); toggle.click(); await new Promise((resolve) => setTimeout(resolve, 0)); await Promise.resolve() })
    await closeSettings(container)
    await act(async () => { setTextAreaValue(getComposer(container), 'Say this'); getSendButton(container).click(); await Promise.resolve() })
    await act(async () => { promptEventHandler?.({ payload: { request_id: requestId, kind: 'text', text: 'Valid first line\nmore' } }); await new Promise((resolve) => setTimeout(resolve, 0)) })
    await act(async () => { window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' })); await Promise.resolve() })
    expect(stop).toHaveBeenCalledOnce()
    expect(started).toHaveBeenCalledOnce()
    await act(async () => {
      finishPrompt?.({ request_id: requestId, runtime_phase: 'sleeping', outcome: 'cancelled' })
      await Promise.resolve()
    })
  })

  it('does not start TTS playback when synthesis resolves after Escape', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let resolveSynthesis: ((value: unknown) => void) | undefined
    let requestId = ''
    const started = vi.fn()
    let synthesisInvoked = false
    class FakeAudioContext {
      destination = {} as AudioDestinationNode
      state: AudioContextState = 'running'
      createBuffer(): AudioBuffer { return { copyToChannel: () => {} } as unknown as AudioBuffer }
      createBufferSource(): AudioBufferSourceNode { return { buffer: null, connect: () => {}, onended: null, start: started, stop: vi.fn() } as unknown as AudioBufferSourceNode }
      createGain(): GainNode { return { gain: { value: 1 } as AudioParam, connect: () => {} } as unknown as GainNode }
      async close(): Promise<void> {}
    }
    Object.defineProperty(globalThis, 'AudioContext', { configurable: true, value: FakeAudioContext })
    window.__TAURI_INTERNALS__ = {
      listen: async (_event, handler) => { promptEventHandler = handler; return () => undefined },
      invoke: async (command, args) => {
        if (command === 'get_startup_state') return readyStartupState({ prompt_cancellation_available: true })
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        if (command === 'set_tts_enabled') return { enabled: true }
        if (command === 'submit_prompt') { requestId = (args as { requestId: string }).requestId; return new Promise(() => {}) }
        if (command === 'synthesize_local_tts') {
          synthesisInvoked = true
          return new Promise((resolve) => { resolveSynthesis = resolve })
        }
        if (command === 'cancel_prompt') return null
        throw new Error(`unexpected command: ${command}`)
      },
    }
    const { container } = await renderApp()
    await openSettings(container)
    await act(async () => { getTtsToggle(container).click(); await new Promise((resolve) => setTimeout(resolve, 0)); await Promise.resolve() })
    await closeSettings(container)
    await act(async () => { setTextAreaValue(getComposer(container), 'Say this'); getSendButton(container).click(); await Promise.resolve() })
    await act(async () => {
      promptEventHandler?.({ payload: { request_id: requestId, kind: 'text', text: 'Valid first line\nmore' } })
      await new Promise((resolve) => setTimeout(resolve, 0))
    })
    expect(synthesisInvoked).toBe(true)
    expect(resolveSynthesis).toBeDefined()
    await act(async () => { window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' })); await Promise.resolve() })
    await act(async () => { resolveSynthesis?.({ pcm_f32: [0], sample_rate_hz: 22050, duration_ms: 1 }); await Promise.resolve() })
    expect(started).not.toHaveBeenCalled()
  })

  it.each(['cancelled', 'error'] as const)('does not start delayed TTS after backend %s settlement', async (outcome) => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let finishPrompt: ((value: unknown) => void) | undefined
    let resolveSynthesis: ((value: unknown) => void) | undefined
    let requestId = ''
    const started = vi.fn()
    class FakeAudioContext {
      destination = {} as AudioDestinationNode
      state: AudioContextState = 'running'
      createBuffer(): AudioBuffer { return { copyToChannel: () => {} } as unknown as AudioBuffer }
      createBufferSource(): AudioBufferSourceNode { return { buffer: null, connect: () => {}, onended: null, start: started, stop: vi.fn() } as unknown as AudioBufferSourceNode }
      createGain(): GainNode { return { gain: { value: 1 } as AudioParam, connect: () => {} } as unknown as GainNode }
      async close(): Promise<void> {}
    }
    Object.defineProperty(globalThis, 'AudioContext', { configurable: true, value: FakeAudioContext })
    window.__TAURI_INTERNALS__ = {
      listen: async (_event, handler) => { promptEventHandler = handler; return () => undefined },
      invoke: async (command, args) => {
        if (command === 'get_startup_state') return readyStartupState({ tts_enabled: false })
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        if (command === 'set_tts_enabled') return { enabled: true }
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          return new Promise((resolve) => { finishPrompt = resolve })
        }
        if (command === 'synthesize_local_tts') return new Promise((resolve) => { resolveSynthesis = resolve })
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    await openSettings(container)
    await act(async () => {
      getTtsToggle(container).click()
      await new Promise((resolve) => setTimeout(resolve, 0))
    })
    await closeSettings(container)
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Discard this speech')
      getSendButton(container).click()
      await Promise.resolve()
      promptEventHandler?.({ payload: { request_id: requestId, kind: 'text', text: 'Discarded first line\nmore' } })
      await Promise.resolve()
    })
    expect(resolveSynthesis).toBeDefined()
    await act(async () => {
      finishPrompt?.({
        request_id: requestId,
        runtime_phase: 'sleeping',
        outcome,
        ...(outcome === 'error' ? { error_message: 'backend failed' } : {}),
      })
      await Promise.resolve()
      resolveSynthesis?.({ pcm_f32: [0], sample_rate_hz: 22050, duration_ms: 1 })
      await Promise.resolve()
    })
    expect(started).not.toHaveBeenCalled()
  })

  it('renders response profile dropdown from startup state', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command !== 'get_startup_state') {
          throw new Error(`unexpected command: ${command}`)
        }

        return {
          kind: 'ready',
          cue_asset_paths: {
            start_listening: 'resources/start-listening.wav',
            stop_listening: 'resources/stop-listening.wav',
          },
          runtime_phase: 'sleeping',
          voice_input_available: true,
          voice_input_error: null,
          silence_timeout_ms: 1500,
          selected_response_profile: 'fast',
          supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
        }
      },
    }

    const { container } = await renderApp()
    const select = await getResponseProfileSelect(container)

    expect(select.value).toBe('local-fast')
    expect(Array.from(select.options).map((option) => option.text)).toEqual(['Local: Fast', 'Local: Quality', 'Custom: GPT-5.6 Sol High', 'Custom: GPT-5.6 Luna Low', 'OpenCode: GPT-5.6 Sol High', 'OpenCode: GPT-5.6 Luna Low'])
  })

  it('invokes switch_response_profile when selecting Quality profile', async () => {
    const invokedCommands: string[] = []
    let selectedProfile: 'fast' | 'quality' = 'fast'

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        invokedCommands.push(command)

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: selectedProfile,
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'switch_response_profile') {
          expect(args).toEqual({ profile: 'quality' })
          selectedProfile = 'quality'
          return {
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'set_assistant_settings') {
          return (args as { settings: unknown }).settings
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = await getResponseProfileSelect(container)

    await act(async () => {
      setSelectValue(select, 'quality')
      await Promise.resolve()
    })

    expect(invokedCommands).toContain('switch_response_profile')
    expect(select.value).toBe('local-quality')
  })

  it('shows model loading indicator while profile switching is in progress', async () => {
    let selectedProfile: 'fast' | 'quality' = 'fast'
    let switchCommandPending = false
    let resolveSwitchCommand: () => void = () => {
      throw new Error('Expected pending switch command resolver')
    }

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: selectedProfile,
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'switch_response_profile') {
          expect(args).toEqual({ profile: 'quality' })

          return await new Promise((resolve) => {
            switchCommandPending = true
            resolveSwitchCommand = () => {
              switchCommandPending = false
              selectedProfile = 'quality'
              resolve({
                selected_response_profile: 'quality',
                supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
              })
            }
          })
        }

        if (command === 'set_assistant_settings') {
          return (args as { settings: unknown }).settings
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = await getResponseProfileSelect(container)

    await act(async () => {
      setSelectValue(select, 'quality')
      await Promise.resolve()
    })

    expect(container.textContent).toContain('Model loading: Switching response profile to Quality...')
    expect(switchCommandPending).toBe(true)

    await act(async () => {
      resolveSwitchCommand()
      await Promise.resolve()
    })

    expect(select.value).toBe('local-quality')
  })

  it('restores previous profile and hides profile switch errors from chat', async () => {
    const invokedCommands: string[] = []
    let startupStateCallCount = 0

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        invokedCommands.push(command)

        if (command === 'get_startup_state') {
          startupStateCallCount += 1

          if (startupStateCallCount === 1) {
            return {
              kind: 'ready',
              cue_asset_paths: {
                start_listening: 'resources/start-listening.wav',
                stop_listening: 'resources/stop-listening.wav',
              },
              runtime_phase: 'sleeping',
              voice_input_available: false,
              voice_input_error: null,
              silence_timeout_ms: 1500,
              selected_response_profile: 'fast',
              supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
            }
          }

          if (startupStateCallCount === 2) {
            return {
              kind: 'warming_model',
              cue_asset_paths: {
                start_listening: 'resources/start-listening.wav',
                stop_listening: 'resources/stop-listening.wav',
              },
              runtime_phase: 'initializing',
              voice_input_available: false,
              voice_input_error: null,
              silence_timeout_ms: 1500,
              message: 'Loading local Gemma model...',
              selected_response_profile: 'fast',
              supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
            }
          }

          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'executing',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'switch_response_profile') {
          throw new Error('response backend is busy; wait for the active operation to finish')
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = await getResponseProfileSelect(container)

    await act(async () => {
      setSelectValue(select, 'quality')
      await Promise.resolve()
      await new Promise((resolve) => window.setTimeout(resolve, 600))
    })

    expect(startupStateCallCount).toBe(3)
    expect(nonDiagnosticCommands(invokedCommands)).toEqual([
      'get_startup_state',
      'switch_response_profile',
      'get_startup_state',
      'get_startup_state',
    ])
    expect(select.value).toBe('local-fast')
    expect(container.textContent).toContain('Profile switch failed')
    expect(container.textContent).not.toContain(
      'Response profile switch error: response backend is busy; wait for the active operation to finish',
    )
  })

  it('surfaces startup error when profile switch polling settles to error', async () => {
    let startupStateCallCount = 0

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        if (command === 'get_startup_state') {
          startupStateCallCount += 1

          if (startupStateCallCount === 1) {
            return {
              kind: 'ready',
              cue_asset_paths: {
                start_listening: 'resources/start-listening.wav',
                stop_listening: 'resources/stop-listening.wav',
              },
              runtime_phase: 'sleeping',
              voice_input_available: true,
              voice_input_error: null,
              silence_timeout_ms: 1500,
              selected_response_profile: 'fast',
              supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
            }
          }

          return {
            kind: 'error',
            message: 'failed to initialize local llama.cpp runtime: boom',
          }
        }

        if (command === 'switch_response_profile') {
          expect(args).toEqual({ profile: 'quality' })
          return {
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'set_assistant_settings') {
          return (args as { settings: unknown }).settings
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = await getResponseProfileSelect(container)

    await act(async () => {
      setSelectValue(select, 'quality')
      await Promise.resolve()
    })

    expect(startupStateCallCount).toBe(2)
    expect(container.textContent).toContain('Startup error: failed to initialize local llama.cpp runtime: boom')
    expect(container.textContent).not.toContain('Response profile switch error:')
  })

  it('surfaces requested profile failure when the previous runtime is restored', async () => {
    let startupStateCallCount = 0
    const sources: Array<{ stop: ReturnType<typeof vi.fn> }> = []
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      const source = { stop: vi.fn(), onFrame: options.onFrame }
      sources.push(source)
      return source
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_startup_state') {
          startupStateCallCount += 1
          if (startupStateCallCount === 1) return readyStartupState()
          return readyStartupState({
            selected_response_profile: 'fast',
            capabilities: completeCapabilities({
              local_quality: 'failed',
              qwen_prediction: 'warming',
            }).map((capability) => capability['id'] === 'local_quality'
              ? { ...capability, reason: 'failed to initialize requested quality profile: boom' }
              : capability),
          })
        }

        if (command === 'switch_response_profile') return {
          selected_response_profile: 'quality',
          supported_response_profiles: ['fast', 'quality'],
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = await getResponseProfileSelect(container)

    await act(async () => {
      setSelectValue(select, 'quality')
      await Promise.resolve()
    })

    expect(select.value).toBe('local-fast')
    expect(container.textContent).toContain('Profile switch failed')
    expect(container.textContent).toContain('failed to initialize requested quality profile: boom')
    expect(startupStateCallCount).toBe(2)
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0))
    })
    expect(sources).toHaveLength(2)
    expect(sources[0]?.stop).toHaveBeenCalledOnce()
    expect(sources[1]?.stop).not.toHaveBeenCalled()
  })

  it('restarts auto-started microphone once after a failed profile rollback', async () => {
    const sources: Array<{ stop: ReturnType<typeof vi.fn>; onFrame: (frame: readonly number[]) => Promise<void> | void }> = []
    const invokedCommands: string[] = []
    let startupCalls = 0
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      const source = { stop: vi.fn(), onFrame: options.onFrame }
      sources.push(source)
      return source
    })
    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        invokedCommands.push(command)
        if (command === 'get_startup_state') {
          startupCalls += 1
          return readyStartupState({
            selected_response_profile: startupCalls === 1 ? 'fast' : 'fast',
            capabilities: completeCapabilities(startupCalls === 1 ? {} : { local_quality: 'failed' }),
          })
        }
        if (command === 'switch_response_profile') {
          expect(args).toEqual({ profile: 'quality' })
          return null
        }
        if (command === 'ingest_audio_frame') return { runtime_phase: 'sleeping', last_activity_ms: null, transcription_ready_samples: null, transcript_text: null, capturing_utterance: false, preroll_samples: 0, utterance_samples: 0 }
        if (command === 'get_assistant_settings') return defaultAssistantSettings()
        return null
      },
    }
    const { container } = await renderApp()
    const select = await getResponseProfileSelect(container)
    await act(async () => { setSelectValue(select, 'quality'); await Promise.resolve() })
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); await Promise.resolve() })
    expect(sources).toHaveLength(2)
    expect(sources[0]?.stop).toHaveBeenCalledOnce()
    expect(sources[1]?.stop).not.toHaveBeenCalled()
    await act(async () => { await sources[0]?.onFrame([0.1]); await sources[1]?.onFrame([0.1]); await Promise.resolve() })
    expect(invokedCommands.filter((command) => command === 'ingest_audio_frame')).toHaveLength(1)
  })

  it('stays fail-closed when post-switch snapshot refresh fails', async () => {
    let snapshotCalls = 0
    let startupCalls = 0
    let submitCalls = 0
    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          if (snapshotCalls === 1) return appSnapshot()
          throw new Error('post-switch snapshot unavailable')
        }
        if (command === 'get_startup_state') {
          startupCalls += 1
          return readyStartupState({ selected_response_profile: startupCalls === 1 ? 'fast' : 'quality' })
        }
        if (command === 'switch_response_profile') return { selected_response_profile: 'quality' }
        if (command === 'set_assistant_settings') return (args as { settings: unknown }).settings
        if (command === 'submit_prompt') { submitCalls += 1; return null }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setSelectValue(await getResponseProfileSelect(container), 'quality')
      for (let index = 0; index < 8; index += 1) await Promise.resolve()
    })
    await closeSettings(container)
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Must remain blocked after refresh failure')
      getSendButton(container).click()
      await Promise.resolve()
    })

    expect(container.textContent).toContain('Profile state unavailable')
    expect(getSendButton(container).disabled).toBe(true)
    await openSettings(container)
    const settingsSelect = container.querySelector<HTMLSelectElement>('#assistantInstantSelect')
    expect(settingsSelect).not.toBeNull()
    await act(async () => {
      setSelectValue(settingsSelect as HTMLSelectElement, 'custom-sol-high')
      await Promise.resolve()
    })
    await closeSettings(container)
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Custom remains blocked')
      getSendButton(container).click()
      for (let index = 0; index < 6; index += 1) await Promise.resolve()
    })
    await openSettings(container)
    await act(async () => {
      setSelectValue(container.querySelector<HTMLSelectElement>('#assistantInstantSelect') as HTMLSelectElement, 'quality')
      await Promise.resolve()
    })
    await closeSettings(container)
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Local quality remains blocked')
      getSendButton(container).click()
      await Promise.resolve()
    })
    await closeSettings(container)
    expect(submitCalls).toBe(0)
  })

  it('blocks voice submission after post-switch snapshot refresh fails', async () => {
    let snapshotCalls = 0
    let startupCalls = 0
    let submitCalls = 0
    let ingestCalls = 0
    let markSilenceCalls = 0
    let nowMs = 1_000
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const stop = vi.fn()
    Date.now = () => nowMs
    Object.defineProperty(globalThis, 'Audio', {
      configurable: true,
      value: class FakeAudio { play(): Promise<void> { return Promise.resolve() } },
    })
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          if (snapshotCalls === 1) return appSnapshot()
          throw new Error('post-switch snapshot unavailable')
        }
        if (command === 'get_startup_state') {
          startupCalls += 1
          return readyStartupState({ selected_response_profile: startupCalls === 1 ? 'fast' : 'quality' })
        }
        if (command === 'switch_response_profile') return { selected_response_profile: 'quality' }
        if (command === 'ingest_audio_frame') {
          ingestCalls += 1
          return {
          runtime_phase: 'listening', transcription_ready_samples: null, transcript_text: null,
          last_activity_ms: 1_000, capturing_utterance: true, preroll_samples: 4, utterance_samples: 4,
          }
        }
        if (command === 'mark_silence') {
          markSilenceCalls += 1
          return {
          runtime_phase: 'processing', transcription_ready_samples: 3_200, transcript_text: 'Blocked voice prompt',
          last_activity_ms: null, capturing_utterance: false, preroll_samples: 4, utterance_samples: 0,
          }
        }
        if (command === 'submit_prompt') { submitCalls += 1; return null }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setSelectValue(await getResponseProfileSelect(container), 'quality')
      for (let index = 0; index < 8; index += 1) await Promise.resolve()
    })
    const frame = onFrame as unknown as (samples: readonly number[]) => Promise<void> | void
    await act(async () => { await frame([0.2, -0.2]); await Promise.resolve() })
    nowMs = 3_000
    await act(async () => { await frame([0.001, -0.001]); await Promise.resolve(); await Promise.resolve() })

    expect(container.textContent).toContain('Profile state unavailable')
    expect(stop).toHaveBeenCalled()
    expect(ingestCalls).toBe(0)
    expect(markSilenceCalls).toBe(0)
    expect(submitCalls).toBe(0)
  })

  it('keeps a failed active local profile unsendable and retries it explicitly', async () => {
    const invokedCommands: string[] = []
    let recovered = false
    const startupState = (): Record<string, unknown> => readyStartupState({
      capabilities: completeCapabilities(recovered ? {} : { local_fast: 'failed' }).map((capability) =>
        capability['id'] === 'local_fast' && !recovered
          ? { ...capability, reason: 'failed to initialize requested fast profile: boom' }
          : capability),
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        invokedCommands.push(command)
        if (command === 'get_startup_state') return startupState()
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        if (command === 'switch_response_profile') {
          recovered = true
          return { selected_response_profile: 'fast', supported_response_profiles: ['fast', 'quality'] }
        }
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'hello')
      await Promise.resolve()
    })
    expect(getSendButton(container).disabled).toBe(true)

    const select = await getResponseProfileSelect(container)
    expect(select.querySelector<HTMLOptionElement>('option[value="local-fast"]')?.disabled).toBe(false)
    await act(async () => {
      getButtonByLabel(container, 'Retry local profile').click()
      await new Promise((resolve) => setTimeout(resolve, 0))
    })
    await closeSettings(container)

    expect(nonDiagnosticCommands(invokedCommands)).toContain('switch_response_profile')
    await act(async () => { setTextAreaValue(getComposer(container), 'another prompt'); await Promise.resolve(); await Promise.resolve(); })
    expect(getSendButton(container).disabled).toBe(false)
  })

  it('keeps unrelated providers and retry available after a persistent local retry failure', async () => {
    const failedState = readyStartupState({
      capabilities: completeCapabilities({ local_fast: 'failed' }).map((capability) =>
        capability['id'] === 'local_fast'
          ? { ...capability, reason: 'failed to initialize requested fast profile: still broken' }
          : capability),
    })
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_startup_state') return failedState
        if (command === 'switch_response_profile') {
          return { selected_response_profile: 'fast', supported_response_profiles: ['fast'] }
        }
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = await getResponseProfileSelect(container)
    await act(async () => {
      getButtonByLabel(container, 'Retry local profile').click()
      await Promise.resolve()
    })

    expect(container.textContent).toContain('Profile switch failed')
    expect(getButtonByLabel(container, 'Retry local profile')).not.toBeNull()
    expect(select.querySelector<HTMLOptionElement>('option[value="opencode-sol-high"]')?.disabled).toBe(false)
    await closeSettings(container)
    expect(getSendButton(container).disabled).toBe(true)
  })

  it('renders only user and assistant prompt execution output when submit command succeeds', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    window.__TAURI_INTERNALS__ = {
      listen: async (_event, handler) => {
        promptEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-quality')

        expect(command).toBe('submit_prompt')
          expect(args).toMatchObject({ requestId: expect.any(String), prompt: 'Draft release notes' })
        promptEventHandler?.({
          payload: { request_id: args && (args as { requestId: string }).requestId, kind: 'text', text: 'OpenCode response' },
        })

        return {
          request_id: (args as { requestId: string }).requestId,
          outcome: 'completed',
          error_message: null,
          runtime_phase: 'sleeping',
        }
      },
    }

    const { container } = await renderApp()
    const composer = getComposer(container)
    const sendButton = getSendButton(container)

    await act(async () => {
      setTextAreaValue(composer, 'Draft release notes')
    })

    await act(async () => {
      sendButton.click()
      await Promise.resolve()
    })

    expect(container.textContent).toContain('Draft release notes')
    expect(container.textContent).toContain('OpenCode response')
  })

  it('preserves typed prompt bytes in the user display and command payload', async () => {
    let submittedPrompt: string | undefined
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command, args) => {
        if (command === 'get_startup_state') return readyStartupState({ selected_response_profile: 'fast' })
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        expect(command).toBe('submit_prompt')
        submittedPrompt = (args as { prompt: string }).prompt
        return {
          request_id: (args as { requestId: string }).requestId,
          outcome: 'completed',
          error_message: null,
          runtime_phase: 'sleeping',
        }
      },
    }

    const { container } = await renderApp()
    const prompt = '  keep me  \nwith spaces  '
    await act(async () => setTextAreaValue(getComposer(container), prompt))
    await act(async () => {
      getSendButton(container).click()
      await Promise.resolve()
    })

    expect(submittedPrompt).toBe(prompt)
    expect(container.querySelector('.message--user .message__content')?.textContent).toBe(prompt)
  })

  it('does not render no-output execution fallback as an assistant message', async () => {
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command, args) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-quality')

        return {
          request_id: (args as { requestId: string }).requestId,
          outcome: 'completed',
          error_message: null,
          runtime_phase: 'sleeping',
        }
      },
    }

    const { container } = await renderApp()
    const composer = getComposer(container)
    const sendButton = getSendButton(container)

    await act(async () => {
      setTextAreaValue(composer, 'No output prompt')
    })

    await act(async () => {
      sendButton.click()
      await Promise.resolve()
    })

    expect(container.textContent).toContain('No output prompt')
    expect(container.textContent).not.toContain('OpenCode backend returned no output.')
    expect(container.textContent).toContain('No response')
    expect(container.textContent).toContain('No response was returned. Try again.')
  })

  it('streams correlated deltas into one assistant bubble and ignores stale events', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let finishPrompt: ((value: unknown) => void) | undefined
    let activeRequestId = ''
    window.__TAURI_INTERNALS__ = {
      listen: async (_event, handler) => {
        promptEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast'],
            capabilities: completeCapabilities(),
            prompt_cancellation_available: true,
          }
        }
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        if (command === 'submit_prompt') {
          if (
            typeof args === 'object' &&
            args !== null &&
            'requestId' in args &&
            typeof args.requestId === 'string'
          ) {
            activeRequestId = args.requestId
          }
          return new Promise((resolve) => {
            finishPrompt = resolve
          })
        }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Stream this')
      getSendButton(container).click()
      await Promise.resolve()
    })
    const stopButton = Array.from(container.querySelectorAll('button')).find(
      (button) => button.textContent === 'Stop',
    )
    expect(stopButton).toBeDefined()

    await act(async () => {
      promptEventHandler?.({
        payload: { request_id: 'stale-request', kind: 'text', text: 'Wrong' },
      })
      const submitCall = promptEventHandler
      expect(submitCall).toBeDefined()
      expect(container.querySelectorAll('.message--assistant')).toHaveLength(0)
    })

    expect(activeRequestId).not.toBe('')
    await act(async () => {
      promptEventHandler?.({
        payload: { request_id: activeRequestId, kind: 'stage', stage: 'deep', status: 'running' },
      })
      promptEventHandler?.({
        payload: { request_id: activeRequestId, kind: 'text', text: 'Hello' },
      })
      promptEventHandler?.({
        payload: { request_id: activeRequestId, kind: 'text', text: ' world' },
      })
    })

    const assistantMessages = container.querySelectorAll('.message--assistant')
    expect(assistantMessages).toHaveLength(1)
    expect(assistantMessages[0]?.textContent).toContain('Hello world')
    expect(container.textContent).toContain('Deep stage running')
    expect(getResponseAnnouncer(container).textContent).toBe('')
    expect(container.textContent).not.toContain('Wrong')

    await act(async () => {
      finishPrompt?.({
        request_id: activeRequestId,
        runtime_phase: 'sleeping',
        outcome: 'completed',
      })
      await Promise.resolve()
    })
    expect(container.textContent).not.toContain('Executing prompt')
    expect(getResponseAnnouncer(container).textContent).toBe('Assistant response: Hello world')
  })

  it('batches streamed deltas by frame and flushes queued text before completion', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let finishPrompt: ((value: unknown) => void) | undefined
    let requestId = ''
    const scheduledFrames: FrameRequestCallback[] = []
    const requestFrame = vi.fn((callback: FrameRequestCallback) => {
      scheduledFrames.push(callback)
      return 41
    })
    const cancelFrame = vi.fn()
    window.requestAnimationFrame = requestFrame
    window.cancelAnimationFrame = cancelFrame
    window.__TAURI_INTERNALS__ = {
      listen: async (_event, handler) => {
        promptEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'get_assistant_settings') return defaultAssistantSettings()
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          return new Promise((resolve) => { finishPrompt = resolve })
        }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Batch this stream')
      getSendButton(container).click()
      await Promise.resolve()
      promptEventHandler?.({ payload: { request_id: requestId, kind: 'text', text: 'Frame ' } })
      promptEventHandler?.({ payload: { request_id: requestId, kind: 'text', text: 'one' } })
    })

    expect(requestFrame).toHaveBeenCalledTimes(1)
    expect(container.querySelectorAll('.message--assistant')).toHaveLength(0)
    await act(async () => { scheduledFrames[0]?.(16); await Promise.resolve() })
    expect(getConversation(container).textContent).toContain('Frame one')

    await act(async () => {
      promptEventHandler?.({ payload: { request_id: requestId, kind: 'text', text: ' stale' } })
      promptEventHandler?.({ payload: { request_id: requestId, kind: 'correction', stage: 'deep', text: 'Corrected', correction: 'Correction: corrected' } })
      promptEventHandler?.({ payload: { request_id: requestId, kind: 'text', text: ' complete' } })
    })
    expect(requestFrame).toHaveBeenCalledTimes(3)
    await act(async () => { scheduledFrames[1]?.(32); await Promise.resolve() })
    expect(getConversation(container).textContent).not.toContain('Corrected complete')
    await act(async () => { scheduledFrames[2]?.(48); await Promise.resolve() })
    expect(getConversation(container).textContent).toContain('Corrected complete')

    await act(async () => {
      promptEventHandler?.({ payload: { request_id: requestId, kind: 'text', text: ' terminal' } })
      finishPrompt?.({ request_id: requestId, runtime_phase: 'sleeping', outcome: 'completed' })
      await Promise.resolve()
    })

    expect(requestFrame).toHaveBeenCalledTimes(4)
    expect(cancelFrame).toHaveBeenCalledWith(41)
    expect(getConversation(container).textContent).toContain('Corrected complete terminal')
  })

  it('shows string errors rejected by the Tauri prompt command', async () => {
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast'],
            capabilities: completeCapabilities(),
            prompt_cancellation_available: true,
          }
        }
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        if (command === 'submit_prompt') {
          throw 'OpenCode server is not available'
        }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Trigger failure')
      getSendButton(container).click()
      await Promise.resolve()
    })

    expect(container.textContent).toContain('Response failed')
    expect(container.textContent).toContain('OpenCode server is not available')
  })

  it('clears the pending projection when the prompt transport rejects', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let rejectPrompt: ((error: Error) => void) | undefined
    let requestId = ''
    window.__TAURI_INTERNALS__ = {
      listen: async (event, handler) => {
        if (event === 'prompt-execution-event') promptEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          return new Promise((_, reject) => { rejectPrompt = reject })
        }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Retain partial output')
      getSendButton(container).click()
      await Promise.resolve()
      promptEventHandler?.({ payload: { request_id: requestId, kind: 'text', text: 'Partial answer' } })
    })
    await act(async () => {
      rejectPrompt?.(new Error('stream disconnected'))
      await Promise.resolve()
    })

    expect(container.textContent).not.toContain('Retain partial output')
    expect(container.textContent).not.toContain('Partial answer')
    expect(container.querySelector('[aria-label="Instant status: failed"]')).toBeNull()
    expect(container.textContent).toContain('stream disconnected')
  })

  it('shows authoritative OpenCode errors from the final prompt result', async () => {
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command, args) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast'],
            capabilities: completeCapabilities(),
            prompt_cancellation_available: true,
          }
        }
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        if (
          command === 'submit_prompt' &&
          typeof args === 'object' &&
          args !== null &&
          'requestId' in args &&
          typeof args.requestId === 'string'
        ) {
          return {
            request_id: args.requestId,
            runtime_phase: 'error',
            outcome: 'error',
            error_message: 'OpenCode provider authentication failed',
          }
        }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Trigger provider failure')
      getSendButton(container).click()
      await Promise.resolve()
    })

    expect(container.textContent).toContain('Response failed')
    expect(container.textContent).toContain('OpenCode provider authentication failed')
  })

  it('cancels an active stream with Escape and leaves no empty assistant bubble', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let finishPrompt: ((value: unknown) => void) | undefined
    let activeRequestId = ''
    let cancelledRequestId = ''
    window.__TAURI_INTERNALS__ = {
      listen: async (_event, handler) => {
        promptEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast'],
            capabilities: completeCapabilities(),
            prompt_cancellation_available: true,
          }
        }
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        if (command === 'submit_prompt') {
          if (
            typeof args === 'object' &&
            args !== null &&
            'requestId' in args &&
            typeof args.requestId === 'string'
          ) {
            activeRequestId = args.requestId
          }
          return new Promise((resolve) => {
            finishPrompt = resolve
          })
        }
        if (command === 'cancel_prompt') {
          if (
            typeof args === 'object' &&
            args !== null &&
            'requestId' in args &&
            typeof args.requestId === 'string'
          ) {
            cancelledRequestId = args.requestId
          }
          return null
        }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Cancel this')
      getSendButton(container).click()
      await Promise.resolve()
    })
    expect(container.textContent).toContain('Stop')

    await act(async () => {
      window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }))
      await Promise.resolve()
    })
    expect(cancelledRequestId).toBe(activeRequestId)
    expect(container.textContent).toContain('Stopping')

    await act(async () => {
      promptEventHandler?.({
        payload: {
          request_id: activeRequestId,
          kind: 'cancelled',
          runtime_phase: 'sleeping',
        },
      })
      finishPrompt?.({
        request_id: activeRequestId,
        runtime_phase: 'sleeping',
        outcome: 'cancelled',
      })
      await Promise.resolve()
    })

    expect(container.querySelectorAll('.message--assistant')).toHaveLength(0)
    expect(container.textContent).not.toContain('Cancel this')
    expect(container.textContent).toContain('Response cancelled')
    expect(container.textContent).not.toContain('Stopping')
  })

  it('stops from the Stop button and returns to sleeping after cancellation settlement', async () => {
    let finishPrompt: ((value: unknown) => void) | undefined
    let requestId = ''
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command, args) => {
        if (command === 'get_startup_state') return readyStartupState({ prompt_cancellation_available: true })
        if (command === 'get_assistant_settings') return defaultAssistantSettings()
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          return new Promise((resolve) => { finishPrompt = resolve })
        }
        if (command === 'cancel_prompt') return null
        return null
      },
    }
    const { container } = await renderApp()
    await act(async () => { setTextAreaValue(getComposer(container), 'stop me'); getSendButton(container).click(); await Promise.resolve() })
    const stopButton = Array.from(container.querySelectorAll('button')).find((button) => button.textContent === 'Stop')
    expect(stopButton).toBeDefined()
    await act(async () => { stopButton?.click(); await Promise.resolve() })
    expect(container.textContent).toContain('Stopping')
    await act(async () => { finishPrompt?.({ request_id: requestId, runtime_phase: 'sleeping', outcome: 'cancelled' }); await Promise.resolve() })
    expect(container.textContent).not.toContain('Stopping')
    await act(async () => { await Promise.resolve(); await Promise.resolve(); })
    await act(async () => { setTextAreaValue(getComposer(container), 'another prompt'); await Promise.resolve(); await Promise.resolve(); })
    expect(getSendButton(container).disabled).toBe(false)
  })

  it('recovers a rejected Stop request and permits its later completion', async () => {
    let finishPrompt: ((value: unknown) => void) | undefined
    let requestId = ''
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command, args) => {
        if (command === 'get_startup_state') return readyStartupState({ prompt_cancellation_available: true })
        if (command === 'get_assistant_settings') return defaultAssistantSettings()
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          return new Promise((resolve) => { finishPrompt = resolve })
        }
        if (command === 'cancel_prompt') throw new Error('cancel rejected')
        return null
      },
    }
    const { container } = await renderApp()
    await act(async () => { setTextAreaValue(getComposer(container), 'finish me'); getSendButton(container).click(); await Promise.resolve() })
    const stopButton = Array.from(container.querySelectorAll('button')).find((button) => button.textContent === 'Stop')
    await act(async () => { stopButton?.click(); await Promise.resolve() })
    await act(async () => { await Promise.resolve(); await Promise.resolve() })
    expect(container.textContent).toContain('Executing prompt')
    await act(async () => { finishPrompt?.({ request_id: requestId, runtime_phase: 'sleeping', outcome: 'completed' }); await Promise.resolve() })
    expect(container.textContent).not.toContain('Executing prompt')
    await act(async () => { setTextAreaValue(getComposer(container), 'another prompt'); await Promise.resolve(); await Promise.resolve() })
    expect(getSendButton(container).disabled).toBe(false)
  })

  it('uses a correlated stream error when the final result has no message', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    window.__TAURI_INTERNALS__ = {
      listen: async (_event, handler) => {
        promptEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-quality')

        expect(command).toBe('submit_prompt')
        const requestId = (args as { requestId: string }).requestId
        promptEventHandler?.({
          payload: {
            request_id: requestId,
            kind: 'error',
            message: 'Provider failed',
          },
        })
        return {
          request_id: requestId,
          runtime_phase: 'error',
          outcome: 'error',
          error_message: null,
        }
      },
    }

    const { container } = await renderApp()
    const composer = getComposer(container)
    const sendButton = getSendButton(container)

    await act(async () => {
      setTextAreaValue(composer, 'Bad prompt')
    })

    await act(async () => {
      sendButton.click()
      await Promise.resolve()
    })

    expect(container.textContent).not.toContain('Bad prompt')
    expect(container.textContent).toContain('Response failed')
    expect(container.textContent).toContain('Provider failed')
  })

  it('polls startup state while the local model is warming and becomes ready later', async () => {
    let startupCalls = 0

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command !== 'get_startup_state') {
          throw new Error(`unexpected command: ${command}`)
        }

        startupCalls += 1
        if (startupCalls === 1) {
          return {
            kind: 'warming_model',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'initializing',
            voice_input_available: true,
            voice_input_error: null,
            message: 'Loading local Gemma model...',
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        return {
          kind: 'ready',
          cue_asset_paths: {
            start_listening: 'resources/start-listening.wav',
            stop_listening: 'resources/stop-listening.wav',
          },
           runtime_phase: 'sleeping',
           voice_input_available: true,
           voice_input_error: null,
           silence_timeout_ms: 1500,
           selected_response_profile: 'quality',
           supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
         }
      },
    }

    const { container } = await renderApp()

    expect(container.textContent).toContain('Model loading: Loading local Gemma model...')

    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 600))
    })

    expect(container.textContent).not.toContain('Startup ready: runtime=sleeping')
  })

  it('surfaces an error if the model warming state later fails', async () => {
    let startupCalls = 0

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command !== 'get_startup_state') {
          throw new Error(`unexpected command: ${command}`)
        }

        startupCalls += 1
        if (startupCalls === 1) {
          return {
            kind: 'warming_model',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'initializing',
            voice_input_available: true,
            voice_input_error: null,
            message: 'Loading local Gemma model...',
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        return {
          kind: 'error',
          message: 'failed to initialize local llama.cpp runtime: boom',
        }
      },
    }

    const { container } = await renderApp()

    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 600))
    })

    expect(container.textContent).toContain('Startup error: failed to initialize local llama.cpp runtime: boom')
  })

  it('keeps polling ready state until a warming capability becomes available', async () => {
    let startupCalls = 0
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command !== 'get_startup_state') throw new Error(`unexpected command: ${command}`)
        startupCalls += 1
        return readyStartupState({
          capabilities: completeCapabilities({
            qwen_prediction: startupCalls === 1 ? 'warming' : 'available',
          }),
        })
      },
    }

    await renderApp()
    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 600))
    })

    expect(startupCalls).toBe(2)
  })

  it('surfaces a capability that fails after ready state was first published', async () => {
    let startupCalls = 0
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command !== 'get_startup_state') throw new Error(`unexpected command: ${command}`)
        startupCalls += 1
        return readyStartupState({
          capabilities: completeCapabilities({
            qwen_prediction: startupCalls === 1 ? 'warming' : 'failed',
          }).map((capability) => capability['id'] === 'qwen_prediction' && startupCalls > 1
            ? { ...capability, reason: 'completion startup failed' }
            : capability),
        })
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 600))
    })

    expect(container.textContent).toContain('completion startup failed')
  })

  it('continues monitoring a capability after it becomes available', async () => {
    let startupCalls = 0
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command !== 'get_startup_state') throw new Error(`unexpected command: ${command}`)
        startupCalls += 1
        const state = startupCalls === 1 ? 'warming' : startupCalls === 2 ? 'available' : 'failed'
        return readyStartupState({
          capabilities: completeCapabilities({ qwen_prediction: state }).map((capability) =>
            capability['id'] === 'qwen_prediction' && state === 'failed'
              ? { ...capability, reason: 'completion worker stopped' }
              : capability),
        })
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 1_700))
    })

    expect(startupCalls).toBe(3)
    expect(container.textContent).toContain('completion worker stopped')
  })

  it('keeps an active prompt tracked across settled capability polling', async () => {
    let startupCalls = 0
    let submitCalls = 0
    let requestId = ''
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let finishPrompt: ((value: unknown) => void) | undefined
    window.__TAURI_INTERNALS__ = {
      listen: async (_event, handler) => {
        promptEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        if (command === 'get_startup_state') {
          startupCalls += 1
          return readyStartupState({ voice_input_available: false })
        }
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        if (command === 'submit_prompt') {
          submitCalls += 1
          requestId = (args as { requestId: string }).requestId
          return new Promise((resolve) => {
            finishPrompt = resolve
          })
        }
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Long-running prompt')
      getSendButton(container).click()
      await Promise.resolve()
    })

    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 1_100))
      setTextAreaValue(getComposer(container), 'Overlapping prompt')
    })

    expect(startupCalls).toBeGreaterThanOrEqual(2)
    expect(getSendButton(container).disabled).toBe(true)
    await act(async () => {
      getComposer(container).dispatchEvent(
        new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }),
      )
      await Promise.resolve()
    })
    expect(submitCalls).toBe(1)

    await act(async () => {
      promptEventHandler?.({
        payload: { request_id: requestId, kind: 'text', text: 'Original response' },
      })
      finishPrompt?.({
        request_id: requestId,
        runtime_phase: 'sleeping',
        outcome: 'completed',
      })
      await Promise.resolve()
    })

    expect(container.textContent).toContain('Original response')
  })

  it.each(['listening', 'processing'] as const)(
    'does not regress live %s state during capability polling',
    async (runtimePhase) => {
      let startupCalls = 0
      let submitCalls = 0
      window.__TAURI_INTERNALS__ = {
        invoke: async (command) => {
          if (command === 'get_startup_state') {
            startupCalls += 1
            return readyStartupState({
              runtime_phase: startupCalls === 1 ? runtimePhase : 'sleeping',
              voice_input_available: false,
              capabilities: completeCapabilities({
                qwen_prediction: startupCalls === 1 ? 'warming' : 'available',
              }),
            })
          }
          if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
          if (command === 'submit_prompt') {
            submitCalls += 1
            return null
          }
          throw new Error(`unexpected command: ${command}`)
        },
      }

      const { container } = await renderApp()
      await act(async () => {
        setTextAreaValue(getComposer(container), 'Typed while voice is active')
        await new Promise((resolve) => window.setTimeout(resolve, 600))
      })

      expect(startupCalls).toBe(2)
      expect(getSendButton(container).disabled).toBe(true)
      await act(async () => {
        getComposer(container).dispatchEvent(
          new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }),
        )
        await Promise.resolve()
      })
      expect(submitCalls).toBe(0)
    },
  )

  it.each(['listening', 'processing'] as const)(
    'preserves live %s state through a transient startup poll error',
    async (runtimePhase) => {
      let startupCalls = 0
      let submitCalls = 0
      window.__TAURI_INTERNALS__ = {
        invoke: async (command) => {
          if (command === 'get_startup_state') {
            startupCalls += 1
            if (startupCalls === 2) {
              return { kind: 'error', message: 'transient startup poll failure' }
            }
            return readyStartupState({
              runtime_phase: startupCalls === 1 ? runtimePhase : 'sleeping',
              voice_input_available: false,
              capabilities: completeCapabilities({
                qwen_prediction: startupCalls === 1 ? 'warming' : 'available',
              }),
            })
          }
          if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
          if (command === 'submit_prompt') {
            submitCalls += 1
            return null
          }
          throw new Error(`unexpected command: ${command}`)
        },
      }

      const { container } = await renderApp()
      await act(async () => {
        setTextAreaValue(getComposer(container), 'Typed during startup recovery')
        await new Promise((resolve) => window.setTimeout(resolve, 1_600))
      })

      expect(startupCalls).toBe(3)
      expect(getSendButton(container).disabled).toBe(true)
      await act(async () => {
        getComposer(container).dispatchEvent(
          new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }),
        )
        await Promise.resolve()
      })
      expect(submitCalls).toBe(0)
    },
  )

  it('plays the configured start-listening cue path from startup state', async () => {
    const stop = vi.fn()
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const playedSources: string[] = []

    class FakeAudio {
      private readonly source: string

      constructor(source: string) {
        this.source = source
      }

      play(): Promise<void> {
        playedSources.push(this.source)
        return Promise.resolve()
      }
    }

    Object.defineProperty(globalThis, 'Audio', {
      configurable: true,
      value: FakeAudio,
    })
    let nowMs = 100
    Date.now = () => {
      nowMs += 1
      return nowMs
    }

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'test-assets/configured-start.mp3',
              stop_listening: 'test-assets/configured-stop.mp3',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-quality')

        if (command === 'ingest_audio_frame') {
          return {
            runtime_phase: 'listening',
            transcription_ready_samples: null,
            transcript_text: null,
            last_activity_ms: 100,
            capturing_utterance: true,
            preroll_samples: 4,
            utterance_samples: 4,
          }
        }

        if (command === 'mark_silence') {
          return {
            runtime_phase: 'processing',
            transcription_ready_samples: 3200,
            transcript_text: null,
            last_activity_ms: null,
            capturing_utterance: false,
            preroll_samples: 4,
            utterance_samples: 0,
          }
        }

        return {
          runtime_phase: 'processing',
          transcription_ready_samples: null,
          transcript_text: null,
          last_activity_ms: null,
          capturing_utterance: false,
          preroll_samples: 3,
          utterance_samples: 0,
        }
      },
    }

    const { container } = await renderApp()

    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })

    expect(playedSources).toEqual(['test-assets/configured-start.mp3'])
    nowMs = 2_000

    await act(async () => {
      await onFrame?.([0.001, -0.001, 0.001, -0.001])
      await Promise.resolve()
      await new Promise((resolve) => setTimeout(resolve, 0))
    })

    expect(playedSources).toEqual([
      'test-assets/configured-start.mp3',
      'test-assets/configured-stop.mp3',
    ])
    expect(container.textContent).not.toContain('transcription_ready:\n3200 samples captured')
  })

  it('holds a wake confidence peak for one second before showing the latest lower score', async () => {
    vi.useFakeTimers()
    const stop = vi.fn()
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    let confidence: number | null = 0.00114
    let runtimePhase: 'sleeping' | 'listening' = 'sleeping'

    class FakeAudio {
      play(): Promise<void> {
        return Promise.resolve()
      }
    }

    Object.defineProperty(globalThis, 'Audio', {
      configurable: true,
      value: FakeAudio,
    })

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-quality')

        if (command === 'ingest_audio_frame') {
          return {
            runtime_phase: runtimePhase,
            transcription_ready_samples: null,
            transcript_text: null,
            last_activity_ms: Date.now(),
            capturing_utterance: true,
            preroll_samples: 0,
            utterance_samples: 3,
            telemetry: {
              wake_detected_ms: null,
               wake_confidence: confidence,
            },
          }
        }

        if (command === 'ingest_audio_frame') {
          return { runtime_phase: 'sleeping', last_activity_ms: null, transcription_ready_samples: null, transcript_text: null, capturing_utterance: false, preroll_samples: 0, utterance_samples: 0 }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    expect(container.textContent).not.toContain('wake: 0.00%')

    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })

    expect(container.textContent).toContain('wake: 0.11%')

    confidence = 0.8
    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })
    expect(container.textContent).toContain('wake: 80.00%')

    confidence = null
    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })
    expect(container.textContent).not.toContain('wake:')

    confidence = 0.2
    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })
    expect(container.textContent).toContain('wake: 20.00%')

    confidence = 0.3
    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
      await vi.advanceTimersByTimeAsync(999)
    })
    expect(container.textContent).toContain('wake: 30.00%')

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1)
    })
    expect(container.textContent).toContain('wake: 30.00%')

    confidence = 0.9
    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })
    expect(container.textContent).toContain('wake: 90.00%')

    confidence = 0.1
    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
      await vi.advanceTimersByTimeAsync(1_000)
    })
    expect(container.textContent).toContain('wake: 10.00%')

    runtimePhase = 'listening'
    confidence = 0.99
    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })
    expect(container.textContent).not.toContain('wake:')

    confidence = 0.88
    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })
    expect(container.textContent).not.toContain('wake:')

    runtimePhase = 'sleeping'
    confidence = 0
    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })
    expect(container.textContent).toContain('wake: 0.00%')
  })

  it('starts and stops default microphone capture and forwards live frames', async () => {
    const stop = vi.fn()
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        expect(command).toBe('ingest_audio_frame')
        expect(args).toEqual({ frame: [0.1, 0.2, 0.3] })

        return {
          runtime_phase: 'sleeping',
          transcription_ready_samples: null,
          transcript_text: null,
          last_activity_ms: null,
          capturing_utterance: false,
          preroll_samples: 3,
          utterance_samples: 0,
        }
      },
    }

    const { container } = await renderApp()

    expect(startLiveAudioSourceMock).toHaveBeenCalledTimes(1)
    expect(container.textContent).not.toContain('live_audio:\ndefault microphone started')

    await act(async () => {
      await onFrame?.([0.1, 0.2, 0.3])
      await Promise.resolve()
    })

    const stopMicButton = getControlButton(container, 'Stop mic')

    await act(async () => {
      stopMicButton.click()
      await Promise.resolve()
    })

    expect(stop).toHaveBeenCalledTimes(1)
    expect(container.textContent).not.toContain('live_audio:\ndefault microphone stopped')
  })

  it('ignores stale live frames after profile switch stops capture', async () => {
    const originalStop = vi.fn()
    const replacementStop = vi.fn()
    let originalFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    let replacementFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    let sourceCount = 0
    const invokedCommands: string[] = []
    let selectedProfile: 'fast' | 'quality' = 'fast'

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      sourceCount += 1
      if (sourceCount === 1) {
        originalFrame = options.onFrame
        return { stop: originalStop }
      }
      replacementFrame = options.onFrame
      return { stop: replacementStop }
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        invokedCommands.push(command)

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: selectedProfile,
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'switch_response_profile') {
          expect(args).toEqual({ profile: 'quality' })
          selectedProfile = 'quality'
          return {
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'ingest_audio_frame') {
          return {
            runtime_phase: 'sleeping',
            transcription_ready_samples: null,
            transcript_text: null,
            last_activity_ms: null,
            capturing_utterance: false,
            preroll_samples: 0,
            utterance_samples: 0,
          }
        }

        if (command === 'ingest_audio_frame') {
          return { runtime_phase: 'sleeping', last_activity_ms: null, transcription_ready_samples: null, transcript_text: null, capturing_utterance: false, preroll_samples: 0, utterance_samples: 0 }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = await getResponseProfileSelect(container)

    await act(async () => {
      setSelectValue(select, 'quality')
      await Promise.resolve()
    })

    expect(originalStop).toHaveBeenCalledTimes(1)
    expect(startLiveAudioSourceMock.mock.calls.length).toBeGreaterThan(0)

    await act(async () => {
      await originalFrame?.([0.2, -0.2, 0.2, -0.2])
      await replacementFrame?.([0.2, -0.2, 0.2, -0.2])
      await Promise.resolve()
    })

    expect(invokedCommands).toContain('switch_response_profile')
    expect(invokedCommands.filter((command) => command === 'ingest_audio_frame')).toHaveLength(1)
  })

  it('stops live audio and rejects stale frames before resetting the session', async () => {
    const stop = vi.fn()
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let resolveIngestFrame: () => void = () => undefined

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        invokedCommands.push(command)
        if (command === 'get_startup_state') {
          return readyStartupState({ voice_input_available: true })
        }
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        if (command === 'reset_session') return null
        if (command === 'ingest_audio_frame') {
          await new Promise<void>((resolve) => { resolveIngestFrame = resolve })
          return {
            runtime_phase: 'sleeping', transcription_ready_samples: null, transcript_text: null,
            last_activity_ms: null, capturing_utterance: false, preroll_samples: 0, utterance_samples: 0,
          }
        }
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    expect(onFrame).not.toBeNull()
    const frameHandler = onFrame as unknown as (frame: readonly number[]) => Promise<void> | void

    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })

    const framePromise = frameHandler([0.2, -0.2, 0.2, -0.2])
    await Promise.resolve()
    expect(invokedCommands).toContain('ingest_audio_frame')

    await closeSettings(container)
    await act(async () => {
      getNewChatButton(container).click()
      await Promise.resolve()
    })

    expect(invokedCommands).not.toContain('reset_session')
    await act(async () => {
      resolveIngestFrame()
      await framePromise
      await new Promise((resolve) => setTimeout(resolve, 20))
    })

    expect(stop).toHaveBeenCalledTimes(1)
    expect(invokedCommands.indexOf('ingest_audio_frame')).toBeLessThan(invokedCommands.indexOf('reset_session'))
    expect(invokedCommands.filter((command) => command === 'ingest_audio_frame')).toHaveLength(1)
  })

  it('blocks microphone restart and preserves newer runtime activity while reset settles', async () => {
    let resolveReset: () => void = () => undefined
    let snapshotCalls = 0
    startLiveAudioSourceMock.mockResolvedValue({ stop: vi.fn() })
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          return appSnapshot([], {}, { runtime_phase: 'sleeping' })
        }
        if (command === 'get_startup_state') return readyStartupState({ voice_input_available: true })
        if (command === 'reset_session') return new Promise<void>((resolve) => { resolveReset = resolve })
        if (command === 'switch_response_profile') return new Promise(() => undefined)
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })
    await closeSettings(container)
    await act(async () => {
      getNewChatButton(container).click()
      await Promise.resolve()
    })
    await closeSettings(container)
    expect(startLiveAudioSourceMock).toHaveBeenCalledTimes(1)
    expect(getControlButton(container, 'Start mic').disabled).toBe(true)

    await act(async () => {
      setSelectValue(await getResponseProfileSelect(container), 'quality')
      await Promise.resolve()
    })
    expect(getShell(container).dataset['runtimeStatus']).toBe('initializing')
    await act(async () => {
      resolveReset()
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(snapshotCalls).toBeGreaterThanOrEqual(2)
    expect(getShell(container).dataset['runtimeStatus']).toBe('initializing')
  })

  it('restarts auto-started microphone capture once after reset settles', async () => {
    let resolveReset: () => void = () => undefined
    startLiveAudioSourceMock.mockResolvedValue({ stop: vi.fn() })
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') return appSnapshot()
        if (command === 'get_startup_state') return readyStartupState({ voice_input_available: true })
        if (command === 'reset_session') return new Promise<void>((resolve) => { resolveReset = resolve })
        return null
      },
    }

    const { container } = await renderApp()
    expect(startLiveAudioSourceMock).toHaveBeenCalledTimes(1)
    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })
    await closeSettings(container)
    await act(async () => {
      getNewChatButton(container).click()
      await Promise.resolve()
    })
    expect(startLiveAudioSourceMock).toHaveBeenCalledTimes(1)
    await act(async () => {
      resolveReset()
      await Promise.resolve()
      await Promise.resolve()
      await new Promise((resolve) => setTimeout(resolve, 0))
    })
    expect(startLiveAudioSourceMock).toHaveBeenCalledTimes(2)
  })

  it('correlates partial transcription to the active listening session', async () => {
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    let partialEventHandler: ((event: { payload: unknown }) => void) | undefined
    let ingestCount = 0
    class FakeAudio {
      play(): Promise<void> { return Promise.resolve() }
    }
    Object.defineProperty(globalThis, 'Audio', { configurable: true, value: FakeAudio })
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop: vi.fn() }
    })
    window.__TAURI_INTERNALS__ = {
      listen: async (event, handler) => {
        if (event === 'partial-transcription-event') partialEventHandler = handler
        return () => undefined
      },
      invoke: async (command) => {
        if (command === 'get_startup_state') return readyStartupState({ voice_input_available: true })
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        if (command === 'ingest_audio_frame') {
          ingestCount += 1
          return {
            runtime_phase: ingestCount === 2 ? 'sleeping' : 'listening',
            transcription_ready_samples: null,
            transcript_text: null,
            last_activity_ms: null,
            capturing_utterance: ingestCount !== 2,
            preroll_samples: 0,
            utterance_samples: 0,
          }
        }
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const emitPartial = async (sessionId: number, revision: number, text: string) => {
      await act(async () => {
        partialEventHandler?.({ payload: { session_id: sessionId, revision, text } })
        await Promise.resolve()
      })
    }
    await emitPartial(1, 1, 'before listening')
    expect(container.textContent).not.toContain('before listening')

    const frame = onFrame as unknown as (samples: readonly number[]) => Promise<void> | void
    await act(async () => { await frame([0.2, -0.2]); await Promise.resolve() })
    await emitPartial(1, 1, 'current partial')
    expect(container.textContent).toContain('current partial')
    await emitPartial(2, 2, 'wrong session')
    await emitPartial(1, 1, 'stale revision')
    expect(container.textContent).not.toContain('wrong session')
    expect(container.textContent).not.toContain('stale revision')

    await act(async () => { await frame([0.2, -0.2]); await Promise.resolve() })
    await emitPartial(1, 2, 'after listening')
    expect(container.textContent).not.toContain('after listening')

    await act(async () => { await frame([0.2, -0.2]); await Promise.resolve() })
    await emitPartial(1, 3, 'previous session')
    await emitPartial(2, 3, 'next session')
    expect(container.textContent).not.toContain('previous session')
    expect(container.textContent).toContain('next session')
  })

  it('invalidates a pending prompt before reset and ignores late output', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let partialEventHandler: ((event: { payload: unknown }) => void) | undefined
    let activeRequestId = ''
    let resolveReset: () => void = () => undefined
    let snapshotCalls = 0
    const invokedCommands: string[] = []

    window.__TAURI_INTERNALS__ = {
      listen: async (event, handler) => {
        if (event === 'prompt-execution-event') promptEventHandler = handler
        if (event === 'partial-transcription-event') partialEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        invokedCommands.push(command)
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          return appSnapshot([], {}, { session_id: String(snapshotCalls) })
        }
        if (command === 'get_startup_state') return readyStartupState({ prompt_cancellation_available: true })
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        if (command === 'submit_prompt') {
          activeRequestId = (args as { requestId: string }).requestId
          return new Promise(() => undefined)
        }
        if (command === 'cancel_prompt') return null
        if (command === 'reset_session') {
          await new Promise<void>((resolve) => { resolveReset = resolve })
          return null
        }
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Reset this prompt')
      getSendButton(container).click()
      await Promise.resolve()
    })
    expect(activeRequestId).not.toBe('')

    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })
    await closeSettings(container)
    await act(async () => {
      getNewChatButton(container).click()
      await Promise.resolve()
    })
    if (container.querySelector('[role="dialog"]') !== null) await closeSettings(container)
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Blocked during reset')
      getComposer(container).dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }))
      await Promise.resolve()
    })

    expect(invokedCommands).toContain('cancel_prompt')
    expect(invokedCommands).toContain('reset_session')
    expect(invokedCommands.filter((command) => command === 'reset_session')).toHaveLength(1)
    expect(invokedCommands.filter((command) => command === 'submit_prompt')).toHaveLength(1)
    expect(invokedCommands.indexOf('cancel_prompt')).toBeLessThan(invokedCommands.indexOf('reset_session'))
    promptEventHandler?.({ payload: { request_id: activeRequestId, kind: 'text', text: 'late output' } })
    promptEventHandler?.({ payload: { request_id: activeRequestId, kind: 'error', message: 'late failure' } })
    partialEventHandler?.({ payload: { session_id: 999, revision: 1, text: 'late partial' } })
    expect(container.textContent).not.toContain('late output')
    expect(container.textContent).not.toContain('late failure')
    expect(container.textContent).not.toContain('late partial')

    await act(async () => {
      resolveReset()
      await Promise.resolve()
    })
    expect(container.textContent).not.toContain('Reset this prompt')
    expect(container.textContent).not.toContain('late output')
    expect(container.textContent).not.toContain('late failure')
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Prompt after reset')
    })
    expect(getSendButton(container).disabled).toBe(false)
  })

  it('ignores in-flight frame results that resolve after profile switch', async () => {
    const stop = vi.fn()
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let selectedProfile: 'fast' | 'quality' = 'fast'
    let ingestFramePending = false
    let resolveIngestFrame: () => void = () => {
      throw new Error('Expected an in-flight ingest frame promise')
    }

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        invokedCommands.push(command)

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: selectedProfile,
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'ingest_audio_frame') {
          expect(args).toBeDefined()
          ingestFramePending = true

          return await new Promise((resolve) => {
            resolveIngestFrame = () => {
              ingestFramePending = false
              resolve({
                runtime_phase: 'error',
                transcription_ready_samples: null,
                transcript_text: null,
                last_activity_ms: null,
                capturing_utterance: false,
                preroll_samples: 0,
                utterance_samples: 0,
              })
            }
          })
        }

        if (command === 'switch_response_profile') {
          expect(args).toEqual({ profile: 'quality' })
          selectedProfile = 'quality'
          return {
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'set_assistant_settings') {
          return (args as { settings: unknown }).settings
        }

        if (command === 'set_assistant_settings') {
          return (args as { settings: unknown }).settings
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = await getResponseProfileSelect(container)

    expect(onFrame).not.toBeNull()
    const frameHandler = onFrame as unknown as (frame: readonly number[]) => Promise<void> | void

    let pendingFrame: Promise<void> | void
    await act(async () => {
      pendingFrame = frameHandler([0.2, -0.2, 0.2, -0.2])
      await Promise.resolve()
    })

    await act(async () => {
      setSelectValue(select, 'quality')
      await Promise.resolve()
    })

    expect(stop).toHaveBeenCalledTimes(1)
    expect(ingestFramePending).toBe(true)

    resolveIngestFrame()

    await act(async () => {
      await pendingFrame
      await new Promise((resolve) => setTimeout(resolve, 50))
    })

    expect(invokedCommands).toContain('switch_response_profile')
    expect(invokedCommands.filter((command) => command === 'ingest_audio_frame')).toHaveLength(1)
    expect(container.textContent).not.toContain('Reset to idle')
    expect(container.textContent).not.toContain('Runtime control error')
  })

  it('waits for in-flight ingest to drain before invoking profile switch command', async () => {
    const stop = vi.fn()
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let selectedProfile: 'fast' | 'quality' = 'fast'
    let ingestPending = false
    let resolveIngestFrame: () => void = () => {
      throw new Error('Expected delayed ingest resolver')
    }

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        invokedCommands.push(command)

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: selectedProfile,
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'ingest_audio_frame') {
          ingestPending = true

          return await new Promise((resolve) => {
            resolveIngestFrame = () => {
              ingestPending = false
              resolve({
                runtime_phase: 'sleeping',
                transcription_ready_samples: null,
                transcript_text: null,
                last_activity_ms: null,
                capturing_utterance: false,
                preroll_samples: 0,
                utterance_samples: 0,
              })
            }
          })
        }

        if (command === 'switch_response_profile') {
          expect(ingestPending).toBe(false)
          expect(args).toEqual({ profile: 'quality' })
          selectedProfile = 'quality'
          return {
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = await getResponseProfileSelect(container)

    expect(onFrame).not.toBeNull()
    const frameHandler = onFrame as unknown as (frame: readonly number[]) => Promise<void> | void

    let pendingFrame: Promise<void> | void
    await act(async () => {
      pendingFrame = frameHandler([0.2, -0.2, 0.2, -0.2])
      await Promise.resolve()
    })

    expect(ingestPending).toBe(true)

    await act(async () => {
      setSelectValue(select, 'quality')
      await Promise.resolve()
    })

    expect(invokedCommands).not.toContain('switch_response_profile')
    expect(stop).toHaveBeenCalledTimes(1)

    resolveIngestFrame()

    await act(async () => {
      await pendingFrame
      await new Promise((resolve) => setTimeout(resolve, 50))
    })

    expect(invokedCommands).toContain('switch_response_profile')
    expect(container.textContent).not.toContain('Response profile switch error: response backend is busy')
  })

  it('ignores re-entrant profile switch attempts while draining in-flight ingest', async () => {
    const stop = vi.fn()
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let selectedProfile: 'fast' | 'quality' = 'fast'
    let resolveIngestFrame: () => void = () => {
      throw new Error('Expected delayed ingest resolver')
    }

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        invokedCommands.push(command)

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: selectedProfile,
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'ingest_audio_frame') {
          return await new Promise((resolve) => {
            resolveIngestFrame = () => {
              resolve({
                runtime_phase: 'sleeping',
                transcription_ready_samples: null,
                transcript_text: null,
                last_activity_ms: null,
                capturing_utterance: false,
                preroll_samples: 0,
                utterance_samples: 0,
              })
            }
          })
        }

        if (command === 'switch_response_profile') {
          expect(args).toEqual({ profile: 'quality' })
          selectedProfile = 'quality'
          return {
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'set_assistant_settings') {
          return (args as { settings: unknown }).settings
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = await getResponseProfileSelect(container)

    expect(onFrame).not.toBeNull()
    const frameHandler = onFrame as unknown as (frame: readonly number[]) => Promise<void> | void

    let pendingFrame: Promise<void> | void
    await act(async () => {
      pendingFrame = frameHandler([0.2, -0.2, 0.2, -0.2])
      await Promise.resolve()
    })

    await act(async () => {
      setSelectValue(select, 'quality')
      setSelectValue(select, 'quality')
      await Promise.resolve()
    })

    expect(invokedCommands).not.toContain('switch_response_profile')

    resolveIngestFrame()

    await act(async () => {
      await pendingFrame
      await new Promise((resolve) => setTimeout(resolve, 50))
    })

    expect(invokedCommands.filter((command) => command === 'switch_response_profile')).toHaveLength(1)
  })

  it('ignores stale delayed microphone start after profile switch invalidates session', async () => {
    const staleStop = vi.fn()
    const replacementStop = vi.fn()
    let delayedStartCount = 0
    let resolveStaleStart: (source: { stop: () => void }) => void = () => { throw new Error('Expected stale resolver') }
    let resolveReplacementStart: (source: { stop: () => void }) => void = () => { throw new Error('Expected replacement resolver') }
    let staleFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    let replacementFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let selectedProfile: 'fast' | 'quality' = 'fast'

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      delayedStartCount += 1
      if (delayedStartCount === 1) {
        staleFrame = options.onFrame
        return await new Promise((resolve) => { resolveStaleStart = resolve })
      }
      replacementFrame = options.onFrame
      return await new Promise((resolve) => { resolveReplacementStart = resolve })
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        invokedCommands.push(command)

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: selectedProfile,
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'switch_response_profile') {
          expect(args).toEqual({ profile: 'quality' })
          selectedProfile = 'quality'
          return {
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'ingest_audio_frame') {
          return { runtime_phase: 'sleeping', last_activity_ms: null, transcription_ready_samples: null, transcript_text: null, capturing_utterance: false, preroll_samples: 0, utterance_samples: 0 }
        }
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = await getResponseProfileSelect(container)

    await act(async () => {
      setSelectValue(select, 'quality')
      await Promise.resolve()
    })

    expect(delayedStartCount).toBe(1)

    await act(async () => {
      resolveStaleStart({ stop: staleStop })
      await Promise.resolve()
    })

    expect(invokedCommands).toContain('switch_response_profile')
    expect(invokedCommands.filter((command) => command === 'ingest_audio_frame')).toHaveLength(0)
    expect(staleStop).toHaveBeenCalledTimes(1)
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); await Promise.resolve() })
    expect(delayedStartCount).toBe(2)
    expect(container.textContent).not.toContain('live_audio:\ndefault microphone started')
    await act(async () => {
      await staleFrame?.([0.2, -0.2])
      await Promise.resolve()
    })
    expect(invokedCommands.filter((command) => command === 'ingest_audio_frame')).toHaveLength(0)
    await act(async () => {
      resolveReplacementStart({ stop: replacementStop })
      await Promise.resolve()
      await replacementFrame?.([0.2, -0.2])
      await Promise.resolve()
    })
    expect(invokedCommands.filter((command) => command === 'ingest_audio_frame')).toHaveLength(1)
  })

  it('ignores stale delayed microphone start rejection after profile switch', async () => {
    let delayedStartPending = false
    let startCount = 0
    let rejectLiveAudioSource: (error: unknown) => void = () => {
      throw new Error('Expected delayed microphone start rejector')
    }
    let replacementFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let selectedProfile: 'fast' | 'quality' = 'fast'

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      startCount += 1
      if (startCount === 1) {
        return await new Promise((_resolve, reject) => {
          delayedStartPending = true
          rejectLiveAudioSource = reject
        })
      }
      replacementFrame = options.onFrame
      return { stop: vi.fn() }
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        invokedCommands.push(command)

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: selectedProfile,
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'switch_response_profile') {
          expect(args).toEqual({ profile: 'quality' })
          selectedProfile = 'quality'
          return {
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'ingest_audio_frame') {
          return { runtime_phase: 'sleeping', last_activity_ms: null, transcription_ready_samples: null, transcript_text: null, capturing_utterance: false, preroll_samples: 0, utterance_samples: 0 }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = await getResponseProfileSelect(container)

    await act(async () => {
      setSelectValue(select, 'quality')
      await Promise.resolve()
    })

    expect(delayedStartPending).toBe(true)

    await act(async () => {
      delayedStartPending = false
      rejectLiveAudioSource(new Error('Permission denied'))
      await Promise.resolve()
    })

    expect(invokedCommands).toContain('switch_response_profile')
    expect(container.textContent).not.toContain('live_audio_error:\nPermission denied')
    expect(container.textContent).not.toContain('live_audio:\ndefault microphone started')
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0))
      await Promise.resolve()
    })
    expect(startCount).toBe(2)
    await act(async () => {
      await replacementFrame?.([0.2, -0.2])
      await Promise.resolve()
    })
    expect(invokedCommands.filter((command) => command === 'ingest_audio_frame')).toHaveLength(1)
  })

  it('automatically marks silence from backend speech activity updates', async () => {
    const stop = vi.fn()
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let nowMs = 1_000

    class FakeAudio {
      play(): Promise<void> {
        return Promise.resolve()
      }
    }

    Date.now = () => nowMs
    Object.defineProperty(globalThis, 'Audio', {
      configurable: true,
      value: FakeAudio,
    })

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        invokedCommands.push(command)

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'ingest_audio_frame') {
          return {
            runtime_phase: 'listening',
            transcription_ready_samples: null,
            transcript_text: null,
            last_activity_ms: 1_000,
            capturing_utterance: true,
            preroll_samples: 4,
            utterance_samples: 4,
          }
        }

        expect(command).toBe('mark_silence')

        return {
          runtime_phase: 'processing',
          transcription_ready_samples: 3200,
          transcript_text: null,
          last_activity_ms: null,
          capturing_utterance: false,
          preroll_samples: 4,
          utterance_samples: 0,
        }
      },
    }

    const { container } = await renderApp()

    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })

    nowMs = 3_600

    await act(async () => {
      await onFrame?.([0.001, -0.001, 0.001, -0.001])
      await Promise.resolve()
    })

    expect(nonDiagnosticCommands(invokedCommands)).toEqual([
      'get_startup_state',
      'ingest_audio_frame',
      'ingest_audio_frame',
      'mark_silence',
    ])
    expect(container.textContent).not.toContain('transcription_ready:\n3200 samples captured')
  })

  it('submits the transcribed voice prompt after silence and returns to wake-word waiting', async () => {
    const stop = vi.fn()
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let nowMs = 1_000

    class FakeAudio {
      play(): Promise<void> {
        return Promise.resolve()
      }
    }

    Date.now = () => nowMs
    Object.defineProperty(globalThis, 'Audio', {
      configurable: true,
      value: FakeAudio,
    })

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })

    window.__TAURI_INTERNALS__ = {
      listen: async (_event, handler) => {
        promptEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        invokedCommands.push(command)

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-quality')

        if (command === 'ingest_audio_frame') {
          return {
            runtime_phase: 'listening',
            transcription_ready_samples: null,
            transcript_text: null,
            last_activity_ms: 1_000,
            capturing_utterance: true,
            preroll_samples: 4,
            utterance_samples: 4,
          }
        }

        if (command === 'mark_silence') {
          return {
            runtime_phase: 'processing',
            transcription_ready_samples: 3200,
            transcript_text: 'Open the pull request',
            last_activity_ms: null,
            capturing_utterance: false,
            preroll_samples: 4,
            utterance_samples: 0,
          }
        }

        if (command === 'submit_prompt') {
          expect(args).toMatchObject({ requestId: expect.any(String), prompt: 'Open the pull request' })
          promptEventHandler?.({
            payload: { request_id: (args as { requestId: string }).requestId, kind: 'text', text: 'Voice execution response' },
          })

          return {
            request_id: (args as { requestId: string }).requestId,
            outcome: 'completed',
            error_message: null,
            runtime_phase: 'sleeping',
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()

    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })

    nowMs = 3_600

    await act(async () => {
      await onFrame?.([0.001, -0.001, 0.001, -0.001])
      await Promise.resolve()
    })

    expect(nonDiagnosticCommands(invokedCommands)).toEqual([
      'get_startup_state',
      'ingest_audio_frame',
      'ingest_audio_frame',
      'mark_silence',
      'submit_prompt',
    ])
    expect(container.textContent).not.toContain('transcript:\nOpen the pull request')
    expect(container.textContent).toContain('Open the pull request')
    expect(container.textContent).toContain('Voice execution response')
    expect(container.textContent).toContain('Stop mic')
  })

  it('does not submit a voice transcript through an unavailable Instant provider', async () => {
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let nowMs = 1_000

    class FakeAudio {
      play(): Promise<void> {
        return Promise.resolve()
      }
    }

    Date.now = () => nowMs
    Object.defineProperty(globalThis, 'Audio', { configurable: true, value: FakeAudio })
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop: vi.fn() }
    })
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        invokedCommands.push(command)
        if (command === 'get_startup_state') {
          return readyStartupState({
            capabilities: completeCapabilities({ opencode: 'unavailable' }),
          })
        }
        if (command === 'get_assistant_settings') {
          return {
            instant: 'opencode-sol-high', deep: 'opencode-sol-high', review: 'opencode-sol-high',
            deep_enabled: false, review_enabled: false, prefetch: false, completion: false,
          }
        }
        if (command === 'ingest_audio_frame') {
          return {
            runtime_phase: 'listening', transcription_ready_samples: null, transcript_text: null,
            last_activity_ms: 1_000, capturing_utterance: true, preroll_samples: 4, utterance_samples: 4,
          }
        }
        if (command === 'mark_silence') {
          return {
            runtime_phase: 'processing', transcription_ready_samples: 3200,
            transcript_text: 'Do not send this transcript', last_activity_ms: null,
            capturing_utterance: false, preroll_samples: 4, utterance_samples: 0,
          }
        }
        if (command === 'submit_prompt') throw new Error('voice prompt bypassed provider gate')
        throw new Error(`unexpected command: ${command}`)
      },
    }

    await renderApp()
    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
    })
    nowMs = 3_600
    await act(async () => {
      await onFrame?.([0.001, -0.001, 0.001, -0.001])
    })

    expect(invokedCommands).toContain('mark_silence')
    expect(invokedCommands).not.toContain('submit_prompt')
  })

  it('keeps typed and voice submission fail-closed when native settings fail to load', async () => {
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let nowMs = 1_000

    class FakeAudio {
      play(): Promise<void> {
        return Promise.resolve()
      }
    }

    Date.now = () => nowMs
    Object.defineProperty(globalThis, 'Audio', { configurable: true, value: FakeAudio })
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop: vi.fn() }
    })
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        invokedCommands.push(command)
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'get_assistant_settings') throw new Error('settings are unreadable')
        if (command === 'ingest_audio_frame') {
          return {
            runtime_phase: 'listening', transcription_ready_samples: null, transcript_text: null,
            last_activity_ms: 1_000, capturing_utterance: true, preroll_samples: 4, utterance_samples: 4,
          }
        }
        if (command === 'mark_silence') {
          return {
            runtime_phase: 'processing', transcription_ready_samples: 3200,
            transcript_text: 'Blocked voice prompt', last_activity_ms: null,
            capturing_utterance: false, preroll_samples: 4, utterance_samples: 0,
          }
        }
        if (command === 'submit_prompt') throw new Error('settings failure gate was bypassed')
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Blocked typed prompt')
    })
    expect(getSendButton(container).disabled).toBe(true)
    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
    })
    nowMs = 3_600
    await act(async () => {
      await onFrame?.([0.001, -0.001, 0.001, -0.001])
    })
    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })

    expect(invokedCommands).not.toContain('submit_prompt')
    expect(invokedCommands).not.toContain('request_completion')
    expect(container.textContent).toContain('Assistant settings unavailable: settings are unreadable')
    expect(Array.from(container.querySelectorAll<HTMLSelectElement>('.settings-panel__assistant select')).every((control) => control.disabled)).toBe(true)
    expect(Array.from(container.querySelectorAll<HTMLInputElement>('.settings-panel__assistant input')).every((control) => control.disabled)).toBe(true)
  })

  it('blocks typed and voice prompts when the selected local profile is not loaded', async () => {
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let nowMs = 1_000

    class FakeAudio {
      play(): Promise<void> {
        return Promise.resolve()
      }
    }

    Date.now = () => nowMs
    Object.defineProperty(globalThis, 'Audio', { configurable: true, value: FakeAudio })
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop: vi.fn() }
    })
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        invokedCommands.push(command)
        if (command === 'get_startup_state') return readyStartupState({ selected_response_profile: 'fast' })
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-quality')
        if (command === 'ingest_audio_frame') {
          return {
            runtime_phase: 'listening', transcription_ready_samples: null, transcript_text: null,
            last_activity_ms: 1_000, capturing_utterance: true, preroll_samples: 4, utterance_samples: 4,
          }
        }
        if (command === 'mark_silence') {
          return {
            runtime_phase: 'processing', transcription_ready_samples: 3200,
            transcript_text: 'Mismatched voice prompt', last_activity_ms: null,
            capturing_utterance: false, preroll_samples: 4, utterance_samples: 0,
          }
        }
        if (command === 'submit_prompt') throw new Error('profile gate was bypassed')
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Mismatched typed prompt')
    })
    expect(getSendButton(container).disabled).toBe(true)
    expect(container.textContent).toContain('selected local model profile is not loaded')
    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
    })
    nowMs = 3_600
    await act(async () => {
      await onFrame?.([0.001, -0.001, 0.001, -0.001])
    })

    expect(invokedCommands).not.toContain('submit_prompt')
  })

  it('uses TTS and assistant settings updated after the microphone starts', async () => {
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let resolveSettings: (settings: unknown) => void = () => undefined
    const pendingSettings = new Promise<unknown>((resolve) => { resolveSettings = resolve })
    const invokedCommands: string[] = []
    let nowMs = 1_000

    class FakeAudio {
      play(): Promise<void> {
        return Promise.resolve()
      }
    }

    class FakeAudioContext {
      destination = {} as AudioDestinationNode
      state: AudioContextState = 'running'

      createBuffer(): AudioBuffer {
        return { copyToChannel: () => {} } as unknown as AudioBuffer
      }

      createBufferSource(): AudioBufferSourceNode {
        return {
          buffer: null,
          connect: () => {},
          onended: null,
          start: function start(this: AudioBufferSourceNode) {
            this.onended?.(new Event('ended'))
          },
        } as unknown as AudioBufferSourceNode
      }

      createGain(): GainNode {
        return {
          gain: { value: 1 } as AudioParam,
          connect: () => {},
        } as unknown as GainNode
      }

      async resume(): Promise<void> {}
      async close(): Promise<void> {}
    }

    Date.now = () => nowMs
    Object.defineProperty(globalThis, 'Audio', { configurable: true, value: FakeAudio })
    Object.defineProperty(globalThis, 'AudioContext', { configurable: true, value: FakeAudioContext })
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop: vi.fn() }
    })
    window.__TAURI_INTERNALS__ = {
      listen: async (event, handler) => {
        if (event === 'prompt-execution-event') promptEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        invokedCommands.push(command)
        if (command === 'get_startup_state') return readyStartupState({ tts_enabled: false })
        if (command === 'get_assistant_settings') return pendingSettings
        if (command === 'set_tts_enabled') return { enabled: true, sample_rate_hz: 22050 }
        if (command === 'ingest_audio_frame') {
          return {
            runtime_phase: 'listening', transcription_ready_samples: null, transcript_text: null,
            last_activity_ms: 1_000, capturing_utterance: true, preroll_samples: 4, utterance_samples: 4,
          }
        }
        if (command === 'mark_silence') {
          return {
            runtime_phase: 'processing', transcription_ready_samples: 3200,
            transcript_text: 'Use current voice settings', last_activity_ms: null,
            capturing_utterance: false, preroll_samples: 4, utterance_samples: 0,
          }
        }
        if (command === 'submit_prompt') {
          const requestId = (args as { requestId: string }).requestId
          promptEventHandler?.({
            payload: { request_id: requestId, kind: 'text', text: 'Current settings response' },
          })
          return {
            request_id: requestId, outcome: 'completed', error_message: null, runtime_phase: 'sleeping',
          }
        }
        if (command === 'synthesize_local_tts') {
          return { pcm_f32: [0, 0.1], sample_rate_hz: 22050, duration_ms: 1 }
        }
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    expect(startLiveAudioSourceMock).toHaveBeenCalledTimes(1)
    await act(async () => {
      await openSettings(container)
      getTtsToggle(container).click()
      resolveSettings({
        instant: 'local-fast', deep: 'opencode-sol-high', review: 'opencode-sol-high',
        deep_enabled: true, review_enabled: false, prefetch: false, completion: false,
      })
      await Promise.resolve()
    })
    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
    })
    nowMs = 3_600
    await act(async () => {
      await onFrame?.([0.001, -0.001, 0.001, -0.001])
      await new Promise((resolve) => setTimeout(resolve, 0))
    })

    expect(invokedCommands).toContain('synthesize_local_tts')
    expect(container.querySelector('[aria-label="Deep status: stale"]')).not.toBeNull()
  })

  it('uses configured startup silence timeout for auto-stop timing', async () => {
    const stop = vi.fn()
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let nowMs = 1_000

    class FakeAudio {
      play(): Promise<void> {
        return Promise.resolve()
      }
    }

    Date.now = () => nowMs
    Object.defineProperty(globalThis, 'Audio', {
      configurable: true,
      value: FakeAudio,
    })

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        invokedCommands.push(command)

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 3000,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'ingest_audio_frame') {
          return {
            runtime_phase: 'listening',
            transcription_ready_samples: null,
            transcript_text: null,
            last_activity_ms: 1_000,
            capturing_utterance: true,
            preroll_samples: 4,
            utterance_samples: 4,
          }
        }

        if (command === 'mark_silence') {
          return {
            runtime_phase: 'processing',
            transcription_ready_samples: 3200,
            transcript_text: null,
            last_activity_ms: null,
            capturing_utterance: false,
            preroll_samples: 4,
            utterance_samples: 0,
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    await renderApp()

    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })

    nowMs = 3_600

    await act(async () => {
      await onFrame?.([0.001, -0.001, 0.001, -0.001])
      await Promise.resolve()
    })

    expect(nonDiagnosticCommands(invokedCommands)).toEqual([
      'get_startup_state',
      'ingest_audio_frame',
      'ingest_audio_frame',
    ])

    nowMs = 4_000

    await act(async () => {
      await onFrame?.([0.001, -0.001, 0.001, -0.001])
      await Promise.resolve()
    })

    expect(nonDiagnosticCommands(invokedCommands)).toEqual([
      'get_startup_state',
      'ingest_audio_frame',
      'ingest_audio_frame',
      'ingest_audio_frame',
      'mark_silence',
    ])
  })

  it('returns to waiting when mark_silence transcription fails', async () => {
    const stop = vi.fn()
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let nowMs = 1_000

    class FakeAudio {
      play(): Promise<void> {
        return Promise.resolve()
      }
    }

    Date.now = () => nowMs
    Object.defineProperty(globalThis, 'Audio', {
      configurable: true,
      value: FakeAudio,
    })

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        invokedCommands.push(command)

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'ingest_audio_frame') {
          return {
            runtime_phase: 'listening',
            transcription_ready_samples: null,
            transcript_text: null,
            last_activity_ms: 1_000,
            capturing_utterance: true,
            preroll_samples: 4,
            utterance_samples: 4,
          }
        }

        if (command === 'mark_silence') {
          throw 'utterance transcription failed: InvalidTranscript(EmptyText)'
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()

    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })

    nowMs = 3_600

    await act(async () => {
      await onFrame?.([0.001, -0.001, 0.001, -0.001])
      await Promise.resolve()
    })

    expect(nonDiagnosticCommands(invokedCommands)).toEqual([
      'get_startup_state',
      'ingest_audio_frame',
      'ingest_audio_frame',
      'mark_silence',
    ])
    expect(container.textContent).not.toContain(
      'Runtime control error (mark_silence): utterance transcription failed: InvalidTranscript(EmptyText)',
    )
    expect(container.textContent).toContain('Runtime control failed')
    expect(container.textContent).toContain('Stop mic')
  })

  it('waits for the stop cue before starting silence processing', async () => {
    const stop = vi.fn()
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let nowMs = 1_000
    let hasPendingStopCue = false
    let resolveStopCue: () => void = () => {
      throw new Error('stop cue was not pending')
    }

    class FakeAudio {
      source: string

      constructor(source: string) {
        this.source = source
      }

      play(): Promise<void> {
        if (this.source === 'resources/stop-listening.wav') {
          return new Promise<void>((resolve) => {
            hasPendingStopCue = true
            resolveStopCue = resolve
          })
        }

        return Promise.resolve()
      }
    }

    Date.now = () => nowMs
    Object.defineProperty(globalThis, 'Audio', {
      configurable: true,
      value: FakeAudio,
    })

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        invokedCommands.push(command)

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'ingest_audio_frame') {
          return {
            runtime_phase: 'listening',
            transcription_ready_samples: null,
            transcript_text: null,
            last_activity_ms: 1_000,
            capturing_utterance: true,
            preroll_samples: 4,
            utterance_samples: 4,
          }
        }

        expect(command).toBe('mark_silence')

        return {
          runtime_phase: 'processing',
          transcription_ready_samples: 3200,
          transcript_text: null,
          last_activity_ms: null,
          capturing_utterance: false,
          preroll_samples: 4,
          utterance_samples: 0,
        }
      },
    }

    await renderApp()

    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })

    nowMs = 3_600

    let pendingFrame: Promise<void> | void
    await act(async () => {
      pendingFrame = onFrame?.([0.001, -0.001, 0.001, -0.001])
      await Promise.resolve()
    })

    expect(nonDiagnosticCommands(invokedCommands)).toEqual([
      'get_startup_state',
      'ingest_audio_frame',
      'ingest_audio_frame',
    ])

    if (hasPendingStopCue) {
      resolveStopCue()
    }

    await act(async () => {
      await pendingFrame
      await Promise.resolve()
    })

    expect(nonDiagnosticCommands(invokedCommands)).toEqual([
      'get_startup_state',
      'ingest_audio_frame',
      'ingest_audio_frame',
      'mark_silence',
    ])
  })

  it('hides microphone capture errors from chat without changing the backend contract', async () => {
    startLiveAudioSourceMock.mockRejectedValue(new Error('Permission denied'))

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') return appSnapshot([], { instant: 'local-quality', response_profile: 'quality' })
        expect(command).toBe('get_startup_state')

        return {
          kind: 'ready',
          cue_asset_paths: {
            start_listening: 'resources/start-listening.wav',
            stop_listening: 'resources/stop-listening.wav',
          },
          runtime_phase: 'sleeping',
          voice_input_available: true,
          voice_input_error: null,
          silence_timeout_ms: 1500,
          selected_response_profile: 'quality',
          supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
        }
      },
    }

    const { container } = await renderApp()

    expect(container.textContent).toContain('Microphone unavailable')
    expect(container.textContent).not.toContain('live_audio_error:\nPermission denied')
    expect(container.textContent).toContain('Start mic')
  })

  it('auto-starts microphone capture when voice input is ready', async () => {
    const stop = vi.fn()

    startLiveAudioSourceMock.mockResolvedValue({ stop })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        expect(command).toBe('get_startup_state')

        return {
          kind: 'ready',
          cue_asset_paths: {
            start_listening: 'resources/start-listening.wav',
            stop_listening: 'resources/stop-listening.wav',
          },
          runtime_phase: 'sleeping',
          voice_input_available: true,
          voice_input_error: null,
          silence_timeout_ms: 1500,
          selected_response_profile: 'quality',
          supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
        }
      },
    }

    const { container } = await renderApp()

    expect(startLiveAudioSourceMock).toHaveBeenCalledTimes(1)
    expect(container.textContent).not.toContain('live_audio:\ndefault microphone started')
    expect(container.querySelector('details.shell__manual-controls')).toBeNull()
    expect(container.textContent).not.toContain('Stop listening and process')
  })

  it('persists and restarts capture with a selected microphone', async () => {
    const stop = vi.fn()
    startLiveAudioSourceMock.mockResolvedValue({ stop })
    listAudioInputDevicesMock.mockResolvedValue([
      { deviceId: 'studio-device', label: 'Studio Microphone' },
    ])
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'get_assistant_settings') return defaultAssistantSettings()
        if (command === 'get_ui_text_size') return 'medium'
        if (command === 'get_ui_theme') return 'dark'
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })
    const select = container.querySelector<HTMLSelectElement>('#audioInputDevice')
    expect(select).not.toBeNull()
    expect(select?.textContent).toContain('Studio Microphone')

    await act(async () => {
      setSelectValue(select as HTMLSelectElement, 'studio-device')
      await Promise.resolve()
    })

    expect(window.localStorage.getItem('voxgolem.audioInputDeviceId')).toBe('studio-device')
    expect(stop).toHaveBeenCalledTimes(1)
    expect(startLiveAudioSourceMock).toHaveBeenLastCalledWith(expect.objectContaining({
      deviceId: 'studio-device',
    }))

    await act(async () => {
      setSelectValue(select as HTMLSelectElement, '')
      await Promise.resolve()
    })
    expect(window.localStorage.getItem('voxgolem.audioInputDeviceId')).toBeNull()
    expect(startLiveAudioSourceMock).toHaveBeenLastCalledWith(expect.not.objectContaining({
      deviceId: expect.anything(),
    }))

    const firstRoot = mountedRoots.pop()
    await act(async () => {
      firstRoot?.unmount()
    })
    const remounted = await renderApp()
    await act(async () => {
      getButtonByLabel(remounted.container, 'Settings').click()
      await Promise.resolve()
    })
    expect(remounted.container.querySelector<HTMLSelectElement>('#audioInputDevice')?.value).toBe('')
    expect(startLiveAudioSourceMock).toHaveBeenLastCalledWith(expect.not.objectContaining({
      deviceId: expect.anything(),
    }))
  })

  it('retains an explicit microphone when fallback capture fails', async () => {
    window.localStorage.setItem('voxgolem.audioInputDeviceId', 'stale-device')
    startLiveAudioSourceMock.mockRejectedValue(new DOMException('permission denied', 'NotAllowedError'))
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_startup_state') return readyStartupState()
        throw new Error(`unexpected command: ${command}`)
      },
    }

    await renderApp()

    expect(window.localStorage.getItem('voxgolem.audioInputDeviceId')).toBe('stale-device')
  })

  it('ignores delayed fallback metadata from a microphone session that was replaced', async () => {
    const firstStop = vi.fn()
    let firstOptions: StartLiveAudioSourceOptions | undefined
    let callCount = 0
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      callCount += 1
      if (callCount === 1) {
        firstOptions = options
        return { stop: firstStop }
      }
      return new Promise(() => undefined)
    })
    window.localStorage.setItem('voxgolem.audioInputDeviceId', 'first-device')
    listAudioInputDevicesMock.mockResolvedValue([
      { deviceId: 'first-device', label: 'First Microphone' },
      { deviceId: 'second-device', label: 'Second Microphone' },
    ])
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_startup_state') return readyStartupState()
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })
    const select = container.querySelector<HTMLSelectElement>('#audioInputDevice')
    await act(async () => {
      setSelectValue(select as HTMLSelectElement, 'second-device')
      await Promise.resolve()
    })

    const delayedFirstOptions = firstOptions
    if (delayedFirstOptions !== undefined) {
      delayedFirstOptions.onSelectedDeviceFallback?.('first-device')
    }

    expect(select?.value).toBe('second-device')
    expect(window.localStorage.getItem('voxgolem.audioInputDeviceId')).toBe('first-device')
  })

  it('does not auto-start microphone capture when wake-word capability is unavailable', async () => {
    const wakeReason = 'wake word model is unavailable for this test'
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        expect(command).toBe('get_startup_state')
        return readyStartupState({
          voice_input_available: true,
          capabilities: completeCapabilities({ wake_word: 'unavailable' }).map((capability) =>
            capability['id'] === 'wake_word' ? { ...capability, reason: wakeReason } : capability,
          ),
        })
      },
    }

    const { container } = await renderApp()
    await openSettings(container)
    const autoStop = getAutoStopToggle(container)

    expect(startLiveAudioSourceMock).not.toHaveBeenCalled()
    expect(Array.from(container.querySelectorAll('button')).some((button) => button.textContent?.trim() === 'Start mic')).toBe(false)
    expect(autoStop.disabled).toBe(true)
    expect(container.textContent).toContain(wakeReason)
  })

  it('preserves an accepted completion prefetch through typed submission', async () => {
    let completionEventHandler: ((event: { payload: unknown }) => void) | undefined
    const invocations: Array<{ command: string, args?: unknown }> = []
    window.__TAURI_INTERNALS__ = {
      listen: async (event, handler) => {
        if (event === 'completion-event') completionEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        invocations.push({ command, args })
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'get_assistant_settings') {
          return {
            instant: 'local-fast', deep: 'opencode-sol-high', review: 'opencode-sol-high',
            deep_enabled: false, review_enabled: false, prefetch: true, completion: true,
          }
        }
        if (command === 'request_completion' || command === 'clear_completion') return null
        if (command === 'submit_prompt') {
          return {
            request_id: (args as { requestId: string }).requestId,
            outcome: 'completed', error_message: null, runtime_phase: 'sleeping',
          }
        }
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const composer = getComposer(container)
    await act(async () => {
      setTextAreaValue(composer, 'draft')
      await Promise.resolve()
    })
    await act(async () => {
      completionEventHandler?.({
        payload: {
          source: 'typed', revision: 1, voice_session_id: null, suffix: ' completion',
        },
      })
      await Promise.resolve()
    })
    await act(async () => {
      composer.dispatchEvent(new KeyboardEvent('keydown', { key: 'Tab', bubbles: true }))
      await Promise.resolve()
    })

    expect(composer.value).toBe('draft completion')
    expect(invocations.filter(({ command }) => command === 'request_completion')).toHaveLength(1)

    await act(async () => {
      getSendButton(container).click()
      await Promise.resolve()
    })

    expect(invocations.filter(({ command }) => command === 'clear_completion')).toHaveLength(0)
    expect(invocations.find(({ command }) => command === 'submit_prompt')?.args).toMatchObject({
      prompt: 'draft completion',
      source: 'typed',
    })
  })

  it('ignores typed completion events from before prompt submission', async () => {
    let completionEventHandler: ((event: { payload: unknown }) => void) | undefined
    window.__TAURI_INTERNALS__ = {
      listen: async (event, handler) => {
        if (event === 'completion-event') completionEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'get_assistant_settings') {
          return {
            instant: 'local-fast', deep: 'opencode-sol-high', review: 'opencode-sol-high',
            deep_enabled: false, review_enabled: false, prefetch: false, completion: true,
          }
        }
        if (command === 'request_completion' || command === 'clear_completion') return null
        if (command === 'submit_prompt') {
          return {
            request_id: (args as { requestId: string }).requestId,
            outcome: 'completed', error_message: null, runtime_phase: 'sleeping',
          }
        }
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      completionEventHandler?.({
        payload: {
          source: 'typed', revision: 0, voice_session_id: null, suffix: ' UNSOLICITED_COMPLETION',
        },
      })
      await Promise.resolve()
    })
    expect(container.textContent).not.toContain('UNSOLICITED_COMPLETION')
    await act(async () => {
      setTextAreaValue(getComposer(container), 'submit before completion')
    })
    await act(async () => {
      getSendButton(container).click()
      await Promise.resolve()
    })
    await act(async () => {
      completionEventHandler?.({
        payload: {
          source: 'typed', revision: 1, voice_session_id: null, suffix: ' STALE_COMPLETION',
        },
      })
      await Promise.resolve()
    })

    expect(container.textContent).not.toContain('STALE_COMPLETION')
    expect(getComposer(container).value).toBe('')
  })

  it('does not synthesize a pending response after TTS is disabled', async () => {
    let finishPrompt: ((value: unknown) => void) | undefined
    let promptRequestId = ''
    const invoked: string[] = []

    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command, args) => {
        invoked.push(command)
        if (command === 'get_startup_state') return readyStartupState({ tts_enabled: false })
        if (command === 'set_tts_enabled') return { enabled: args && (args as { enabled: boolean }).enabled }
        if (command === 'submit_prompt') {
          promptRequestId = (args as { requestId: string }).requestId
          return new Promise((resolve) => { finishPrompt = resolve })
        }
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    await openSettings(container); await act(async () => { getTtsToggle(container).click(); await Promise.resolve() })
    await closeSettings(container)
    await act(async () => {
      setTextAreaValue(getComposer(container), 'pending response')
      getSendButton(container).click()
      await Promise.resolve()
    })
    await openSettings(container)
    await act(async () => { getTtsToggle(container).click(); await Promise.resolve() })
    await closeSettings(container)
    await act(async () => {
      finishPrompt?.({ request_id: promptRequestId, outcome: 'completed', runtime_phase: 'sleeping' })
      await Promise.resolve()
    })

    expect(invoked).not.toContain('synthesize_local_tts')
  })

  it('marks a Deep-only correction as Deep corrected without requiring Review', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let finishPrompt: ((value: unknown) => void) | undefined
    let requestId = ''
    let synthesizedText = ''
    window.__TAURI_INTERNALS__ = {
      listen: async (event, handler) => {
        if (event !== 'prompt-execution-event') return () => undefined
        promptEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        if (command === 'get_startup_state') return readyStartupState({ tts_enabled: true })
        if (command === 'get_assistant_settings') return {
          instant: 'local-fast', deep: 'opencode-sol-high', review: 'opencode-sol-high',
          deep_enabled: true, review_enabled: false, prefetch: false, completion: false,
        }
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          return new Promise((resolve) => { finishPrompt = resolve })
        }
        if (command === 'synthesize_local_tts') {
          synthesizedText = (args as { text: string }).text
          return { pcm_f32: [0], sample_rate_hz: 22050, duration_ms: 1 }
        }
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'correct this')
      getSendButton(container).click()
      await Promise.resolve()
      await Promise.resolve()
      promptEventHandler?.({ payload: { request_id: requestId, kind: 'stage', stage: 'deep', status: 'running' } })
      promptEventHandler?.({ payload: { request_id: requestId, kind: 'sources', sources: [{ url: 'https://example.com/deep', title: 'Deep source' }] } })
      promptEventHandler?.({ payload: { request_id: requestId, kind: 'correction', stage: 'deep', text: 'Corrected answer', correction: 'Correction: Deep correction' } })
      await Promise.resolve()
    })

    expect(container.textContent).toContain('Deep')
    expect(container.textContent).toContain('Corrected answer')
    expect(synthesizedText).toBe('Deep correction')
    expect(container.querySelector('[aria-label="Deep status: corrected"]')).not.toBeNull()
    expect(container.querySelector('[aria-label="Review status: corrected"]')).toBeNull()
    expect(container.querySelector('a[href="https://example.com/deep"]')).not.toBeNull()
    await act(async () => {
      finishPrompt?.({ request_id: requestId, outcome: 'cancelled', runtime_phase: 'sleeping' })
      await Promise.resolve()
    })
  })

  it('does not auto-stop on silence when the toggle is disabled', async () => {
    const stop = vi.fn()
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let nowMs = 1_000

    class FakeAudio {
      play(): Promise<void> {
        return Promise.resolve()
      }
    }

    Date.now = () => nowMs
    Object.defineProperty(globalThis, 'Audio', {
      configurable: true,
      value: FakeAudio,
    })

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        invokedCommands.push(command)

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'ingest_audio_frame') {
          return {
            runtime_phase: 'listening',
            transcription_ready_samples: null,
            transcript_text: null,
            last_activity_ms: 1_000,
            capturing_utterance: true,
            preroll_samples: 4,
            utterance_samples: 4,
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()

    await act(async () => {
      await openSettings(container)
      getAutoStopToggle(container).click()
      await Promise.resolve()
    })

    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })

    nowMs = 3_600

    await act(async () => {
      await onFrame?.([0.001, -0.001, 0.001, -0.001])
      await Promise.resolve()
    })

    expect(nonDiagnosticCommands(invokedCommands)).toEqual([
      'get_startup_state',
      'ingest_audio_frame',
      'ingest_audio_frame',
    ])
    expect(container.textContent).toContain('Stop mic')
  })

  it('hides raw runtime control rejection messages from chat', async () => {
    const stop = vi.fn()
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    let nowMs = 1_000

    class FakeAudio {
      play(): Promise<void> {
        return Promise.resolve()
      }
    }

    Object.defineProperty(globalThis, 'Audio', {
      configurable: true,
      value: FakeAudio,
    })

    Date.now = () => nowMs

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'ingest_audio_frame') {
          return {
            runtime_phase: 'listening',
            transcription_ready_samples: null,
            transcript_text: null,
            last_activity_ms: 1_000,
            capturing_utterance: true,
            preroll_samples: 4,
            utterance_samples: 4,
          }
        }

        if (command === 'mark_silence') {
          throw 'utterance transcription failed: InvalidTranscript(EmptyText)'
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()

    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })

    nowMs = 3_000

    await act(async () => {
      await onFrame?.([0.001, -0.001, 0.001, -0.001])
      await Promise.resolve()
    })

    expect(container.textContent).not.toContain(
      'Runtime control error (mark_silence): utterance transcription failed: InvalidTranscript(EmptyText)',
    )
  })
})

async function renderApp(strict = false): Promise<{ container: HTMLElement }> {
  const container = document.createElement('div')
  document.body.append(container)
  mountedContainers.push(container)
  const root = createRoot(container)
  mountedRoots.push(root)

  await act(async () => {
    root.render(strict ? <StrictMode><App /></StrictMode> : <App />)
  })

  for (let index = 0; index < 5; index += 1) {
    await act(async () => {
      await Promise.resolve()
    })
  }

  return { container }
}

function completeCapabilities(overrides: Record<string, string> = {}): Array<Record<string, unknown>> {
  return ['custom_provider', 'opencode', 'local_fast', 'local_quality', 'qwen_prediction', 'wake_word', 'vad', 'parakeet', 'tts', 'deep', 'review']
    .map((id) => ({ id, state: overrides[id] ?? 'available', reason: 'test fixture', actual_provider: null }))
}

function readyStartupState(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    kind: 'ready',
    cue_asset_paths: { start_listening: 'start.wav', stop_listening: 'stop.wav' },
    runtime_phase: 'sleeping',
    voice_input_available: true,
    voice_input_error: null,
    silence_timeout_ms: 1500,
    selected_response_profile: 'fast',
    supported_response_profiles: ['fast', 'quality'],
    tts_enabled: false,
    capabilities: completeCapabilities(),
    ...overrides,
  }
}

function defaultAssistantSettings(
  instant: 'local-fast' | 'local-quality' = 'local-fast',
): Record<string, unknown> {
  return {
    instant,
    deep: 'opencode-sol-high',
    review: 'opencode-sol-high',
    deep_enabled: false,
    review_enabled: false,
    prefetch: false,
    completion: false,
  }
}

function appSnapshot(
  transcript: readonly Record<string, unknown>[] = [],
  preferenceOverrides: Record<string, unknown> = {},
  snapshotOverrides: Record<string, unknown> = {},
): Record<string, unknown> {
  return {
    session_id: '1',
    runtime_phase: 'sleeping',
    transcript,
    active_request: null,
    capabilities: completeCapabilities(),
    model_options: [
      { id: 'local-fast', label: 'Local: Fast', available: true, unavailable_reason: null },
      { id: 'local-quality', label: 'Local: Quality', available: true, unavailable_reason: null },
      { id: 'custom-sol-high', label: 'Custom: GPT-5.6 Sol High', available: true, unavailable_reason: null },
      { id: 'custom-luna-low', label: 'Custom: GPT-5.6 Luna Low', available: true, unavailable_reason: null },
      { id: 'opencode-sol-high', label: 'OpenCode: GPT-5.6 Sol High', available: true, unavailable_reason: null },
      { id: 'opencode-luna-low', label: 'OpenCode: GPT-5.6 Luna Low', available: true, unavailable_reason: null },
    ],
    preferences: {
      instant: 'local-fast',
      deep: 'opencode-sol-high',
      review: 'opencode-sol-high',
      deep_enabled: false,
      review_enabled: false,
      prefetch: false,
      completion: false,
      response_profile: 'fast',
      tts_enabled: false,
      ui_theme: 'dark',
      ui_text_size: 'medium',
      ...preferenceOverrides,
    },
    ...snapshotOverrides,
  }
}

function canonicalTurn(
  userId: string,
  answerId: string,
  prompt: string,
  answer: string,
  requestId = 'request-hydrated',
  contentKind: 'text' | 'refusal' = 'text',
  finalStage: 'instant' | 'deep' | 'review' = 'instant',
  sessionId = '1',
): Record<string, unknown> {
  return {
    request_id: requestId,
    execution_id: { session_id: sessionId, generation: answerId },
    user: { id: userId, role: 'user', content: { kind: 'text', text: prompt } },
    answer: { id: answerId, role: 'assistant', content: { kind: contentKind, text: answer } },
    final_stage: finalStage,
  }
}

function getComposer(container: HTMLElement): HTMLTextAreaElement {
  assertSettingsClosed(container)
  const composer = container.querySelector<HTMLTextAreaElement>('#promptComposer')

  if (composer === null) {
    throw new Error('Missing composer textarea')
  }

  return composer
}

function getShell(container: HTMLElement): HTMLElement {
  const shell = container.querySelector<HTMLElement>('.shell')

  if (shell === null) {
    throw new Error('Missing app shell')
  }

  return shell
}

function getConversation(container: HTMLElement): HTMLElement {
  const conversation = container.querySelector<HTMLElement>('main.conversation')

  if (conversation === null) {
    throw new Error('Missing conversation timeline')
  }

  return conversation
}

function getResponseAnnouncer(container: HTMLElement): HTMLElement {
  const announcer = container.querySelector<HTMLElement>('[aria-label="Response announcements"]')
  if (announcer === null) throw new Error('Missing response announcer')
  return announcer
}

function getSendButton(container: HTMLElement): HTMLButtonElement {
  assertSettingsClosed(container)
  const button = container.querySelector<HTMLButtonElement>('button[type="submit"]')

  if (button === null) {
    throw new Error('Missing send button')
  }

  return button
}

function getButtonByLabel(container: HTMLElement, label: string): HTMLButtonElement {
  const button = container.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`)

  if (button === null) {
    throw new Error(`Missing ${label} button`)
  }

  return button
}

function getNewChatButton(container: HTMLElement): HTMLButtonElement {
  assertSettingsClosed(container)
  return getButtonByLabel(container, 'New Chat')
}

function getControlButton(
  container: HTMLElement,
  label: 'Start mic' | 'Stop mic',
): HTMLButtonElement {
  assertSettingsClosed(container)
  const buttons = Array.from(container.querySelectorAll<HTMLButtonElement>('button'))
  const button = buttons.find((candidate) => candidate.textContent?.trim() === label)

  if (button === undefined) {
    throw new Error(`Missing ${label} control button`)
  }

  return button
}

function assertSettingsClosed(container: HTMLElement): void {
  if (container.querySelector('[role="dialog"]') !== null) {
    throw new Error('Background interaction requested while Settings dialog is open')
  }
}

function getAutoStopToggle(container: HTMLElement): HTMLInputElement {
  const labels = Array.from(container.querySelectorAll<HTMLLabelElement>('.settings-panel label.shell__toggle'))
  const autoStopLabel = labels.find((label) => label.textContent?.includes('Auto Stop'))
  const toggle = autoStopLabel?.querySelector<HTMLInputElement>('input[type="checkbox"]') ?? null

  if (toggle === null) {
    throw new Error('Missing auto stop on silence toggle')
  }

  return toggle
}

function getTtsToggle(container: HTMLElement): HTMLInputElement {
  const toggle = container.querySelector<HTMLInputElement>('.settings-panel #tts-toggle')

  if (toggle === null) {
    throw new Error('Missing TTS toggle')
  }

  return toggle
}

async function openSettings(container: HTMLElement): Promise<void> {
  expect(container.querySelector('[role="dialog"]')).toBeNull()
  await act(async () => {
    getButtonByLabel(container, 'Settings').click()
    await Promise.resolve()
  })
}

async function closeSettings(container: HTMLElement): Promise<void> {
  if (container.querySelector('[role="dialog"]') === null) return
  await act(async () => {
    getButtonByLabel(container, 'Close settings').click()
    await Promise.resolve()
  })
  expect(container.querySelector('[role="dialog"]')).toBeNull()
}

async function getResponseProfileSelect(container: HTMLElement): Promise<HTMLSelectElement> {
  await act(async () => {
    getButtonByLabel(container, 'Settings').click()
    await Promise.resolve()
  })
  const select = container.querySelector<HTMLSelectElement>('#assistantInstantSelect')

  if (select === null) {
    throw new Error('Missing response profile select')
  }

  return select
}

function setSelectValue(select: HTMLSelectElement, value: string): void {
  if (select.id === 'assistantInstantSelect') value = value === 'quality' ? 'local-quality' : value === 'fast' ? 'local-fast' : value
  select.value = value
  select.dispatchEvent(new Event('change', { bubbles: true }))
}

function nonDiagnosticCommands(commands: readonly string[]): readonly string[] {
  return commands.filter(
    (command) =>
      command !== 'write_runtime_log' &&
      command !== 'record_frontend_runtime_diagnostic' &&
      command !== 'get_ui_text_size' &&
      command !== 'set_ui_text_size' &&
      command !== 'get_ui_theme' &&
      command !== 'set_ui_theme' &&
      command !== 'get_app_snapshot' &&
      command !== 'get_assistant_settings' &&
      command !== 'set_assistant_settings' &&
      command !== 'check_for_update',
  )
}

function setTextAreaValue(textArea: HTMLTextAreaElement, value: string): void {
  const descriptor = Object.getOwnPropertyDescriptor(
    HTMLTextAreaElement.prototype,
    'value',
  )

  if (descriptor?.set === undefined) {
    throw new Error('Textarea value setter is unavailable')
  }

  descriptor.set.call(textArea, value)
  textArea.dispatchEvent(new Event('input', { bubbles: true }))
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null
}

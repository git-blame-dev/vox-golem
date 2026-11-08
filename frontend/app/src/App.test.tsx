import { StrictMode, act } from 'react'
import { readFileSync } from 'node:fs'
import { createRoot } from 'react-dom/client'
import { flushSync } from 'react-dom'
import type { Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import * as liveAudioSourceModule from './lib/liveAudioSource'
import * as instantModelSelectModule from './components/InstantModelSelect'
import { configureBrowserPreviewFixture } from './lib/browserRuntime'
import { MODEL_CATALOG } from './lib/modelCatalog'
import { useSettingsNavigation } from './lib/useSettingsNavigation'
import App from './App'
import { FrontendSession } from './FrontendSession'
import { sessionAuthority } from './SessionAuthority'
import { appSnapshot, canonicalTurn, completeCapabilities, defaultAssistantSettings, readyStartupState, savedOutcome } from './test/sessionFixtures'
import { installHistoryHarness, type TestHistoryHarness } from './test/historyHarness'
import headerIcon from '../../../apps/desktop-tauri/src-tauri/icons/icon.svg?url'

const startLiveAudioSourceMock = vi.spyOn(liveAudioSourceModule, 'startLiveAudioSource')
const listAudioInputDevicesMock = vi.spyOn(liveAudioSourceModule, 'listAudioInputDevices')
  .mockResolvedValue([])

const mountedContainers: HTMLElement[] = []
const mountedRoots: Root[] = []
const originalDateNow = Date.now
const originalMathRandom = Math.random
const originalRequestAnimationFrame = window.requestAnimationFrame
const originalCancelAnimationFrame = window.cancelAnimationFrame
const originalBodyOverflow = document.body.style.getPropertyValue('overflow')
const originalBodyOverflowPriority = document.body.style.getPropertyPriority('overflow')
const originalBodyOverflowX = document.body.style.getPropertyValue('overflow-x')
const originalBodyOverflowXPriority = document.body.style.getPropertyPriority('overflow-x')
const originalBodyOverflowY = document.body.style.getPropertyValue('overflow-y')
const originalBodyOverflowYPriority = document.body.style.getPropertyPriority('overflow-y')

let historyHarness: TestHistoryHarness | null = null
const obsoleteRendererAudioCommands = new Set([
  'set_native_audio_active', 'reserve_local_tts_playback_id', 'speak_local_tts', 'finish_tts_playback',
  'reserve_cue_playback_id', 'activate_voice_cue', 'finish_cue_playback', 'prepare_voice_cues',
])
let rendererAudioCalls: string[] = []

// Explicit synthetic native projection. No PCM, endpointing, cue or speech
// scheduler is recreated in the renderer's App tests.
let nativeProjection = {
  revision: 1,
  voice: { capture_id: null as number | null, state: 'stopped', runtime_phase: 'sleeping',
    auto_stop_on_silence: true, auto_start_available: false, wake_confidence: null,
    partial_transcript: null, error: null },
  prompt: null,
  speech: { playback_id: null, playing: false, pending: false },
}
let nativeObservationEnabled = false

beforeEach(() => {
  rendererAudioCalls = []
  nativeObservationEnabled = false
  nativeProjection = { revision: 1, voice: { capture_id: null, state: 'stopped', runtime_phase: 'sleeping',
    auto_stop_on_silence: true, auto_start_available: false, wake_confidence: null, partial_transcript: null, error: null },
    prompt: null, speech: { playback_id: null, playing: false, pending: false } }
  historyHarness = installHistoryHarness()
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

  Reflect.deleteProperty(window, '__TAURI_INTERNALS__')
  delete document.documentElement.dataset['uiTheme']
  Date.now = originalDateNow
  Math.random = originalMathRandom
  window.requestAnimationFrame = originalRequestAnimationFrame
  window.cancelAnimationFrame = originalCancelAnimationFrame
  historyHarness?.restore()
  document.body.style.setProperty('overflow', originalBodyOverflow, originalBodyOverflowPriority)
  document.body.style.setProperty('overflow-x', originalBodyOverflowX, originalBodyOverflowXPriority)
  document.body.style.setProperty('overflow-y', originalBodyOverflowY, originalBodyOverflowYPriority)
  vi.useRealTimers()
  startLiveAudioSourceMock.mockReset()
  listAudioInputDevicesMock.mockReset()
  listAudioInputDevicesMock.mockResolvedValue([])
  window.localStorage.removeItem('voxgolem.audioInputDeviceId')
  window.localStorage.removeItem('voxgolem.voiceTelemetry')
  Reflect.deleteProperty(window, '__VOXGOLEM_VOICE_TELEMETRY__')
  window.history.replaceState(null, '', window.location.href)
  historyHarness = null
  expect(rendererAudioCalls).toEqual([])
})

describe('App', () => {
  it('preserves failed Review and corrected fallback stage details through canonical completion and a later snapshot', async () => {
    const committed: Record<string, unknown>[] = []
    let send: (payload: unknown) => void = () => undefined
    window.__TAURI_INTERNALS__ = {
      createChannel: <T,>(handler: (payload: T) => void) => { send = handler as (payload: unknown) => void; return 'synthetic-stage-channel' },
      invoke: async (command, args) => {
        if (command === 'get_startup_state') return readyStartupState({ voice_input_available: false })
        if (command === 'get_app_snapshot') return appSnapshot(committed, { instant: 'custom-sol-high', deep_enabled: true, review_enabled: true })
        if (command === 'submit_prompt') {
          const request = args as { requestId: string; prompt: string }
          if (committed.length === 0) {
            send({ kind: 'text', text: 'Synthetic provisional answer' })
            send({ kind: 'stage', stage: 'instant', status: 'completed' })
            send({ kind: 'correction', stage: 'deep', text: 'Synthetic corrected fallback answer', correction: 'Correction: synthetic evidence changed the answer' })
            send({ kind: 'stage', stage: 'review', status: 'failed', detail: 'Synthetic Review provider failed' })
            committed.push(canonicalTurn('401', '402', request.prompt, 'Synthetic corrected fallback answer', request.requestId, 'text', 'deep'))
          } else committed.push(canonicalTurn('403', '404', request.prompt, 'Synthetic next answer', request.requestId))
          return { kind: 'completed', save_disposition: 'saved', turn: committed.at(-1)! }
        }
        return null
      },
    }
    const { container } = await renderApp()
    await act(async () => { setTextAreaValue(getComposer(container), 'Synthetic fallback question') })
    await act(async () => { getSendButton(container).click() })
    const assertSettled = (): void => {
      const answer = container.querySelectorAll('.answer-stage')[0]!
      expect(answer.querySelector('.answer-stage__answer')?.textContent).toBe('Synthetic corrected fallback answer')
      expect(answer.querySelector('[data-stage="review"][data-status="failed"]')).not.toBeNull()
      expect(answer.querySelector('[data-stage="deep"][data-status="corrected"]')).not.toBeNull()
      expect(answer.querySelector('details')?.textContent).toContain('Synthetic Review provider failed')
      expect(answer.querySelector('details')?.textContent).toContain('Correction: synthetic evidence changed the answer')
      expect(answer.querySelector('details')?.textContent).toContain('Synthetic provisional answer')
    }
    assertSettled()
    await act(async () => { setTextAreaValue(getComposer(container), 'Synthetic later snapshot question') })
    await act(async () => { getSendButton(container).click() })
    assertSettled()
    expect(container.querySelectorAll('.message--assistant')).toHaveLength(2)
  })
  it.each([
    ['sleeping', false], ['listening', true], ['processing', true],
    ['executing', true], ['error', false],
  ])('animates the canonical accessible header icon only for active work: %s', async (phase, busy) => {
    window.__TAURI_INTERNALS__ = { invoke: async (command) => {
      if (command === 'get_startup_state') return readyStartupState({ runtime_phase: phase, voice_input_available: false })
      if (command === 'get_assistant_settings') return defaultAssistantSettings()
      return null
    } }
    const { container } = await renderApp()
    const logo = container.querySelector<HTMLImageElement>('header img[alt="VoxGolem"]')
    expect(logo).not.toBeNull()
    expect(logo?.getAttribute('src')).toBe(headerIcon)
    expect(logo?.dataset['busy']).toBe(String(busy))
  })

  it('uses native reduced-motion CSS to suppress even the busy header animation', () => {
    const appStyles = readFileSync('src/App.css', 'utf8')
    const sheet = new CSSStyleSheet()
    sheet.replaceSync(appStyles)
    const motionRule = Array.from(sheet.cssRules).find((rule) =>
      rule instanceof CSSMediaRule && rule.cssText.includes('(prefers-reduced-motion: reduce)')) as CSSMediaRule | undefined
    expect(motionRule).toBeDefined()
    const logoRule = Array.from(motionRule!.cssRules).find((rule) =>
      rule instanceof CSSStyleRule && rule.selectorText === '.shell__logo[data-busy="true"]') as CSSStyleRule | undefined
    expect(logoRule?.style.animation).toBe('none')
    const activeRule = Array.from(sheet.cssRules).find((rule) =>
      rule instanceof CSSStyleRule && rule.selectorText === '.shell__logo[data-busy="true"]') as CSSStyleRule | undefined
    expect(activeRule?.style.animation).toBe('voxgolem-header-spin 8.533s linear infinite')
    expect(appStyles).toContain('rotate(-360deg)')
  })

  it('shares inline Instant persistence and rollback with Settings, locking both during a write', async () => {
    const calls: string[] = []
    let rejectWrite: (reason: unknown) => void = () => undefined
    window.__TAURI_INTERNALS__ = { invoke: async (command, args) => {
      calls.push(command)
      if (command === 'get_startup_state') return readyStartupState({ voice_input_available: false })
      if (command === 'get_assistant_settings') return { ...defaultAssistantSettings(), instant: 'custom-sol-high' }
      if (command === 'set_assistant_settings') {
        expect(args).toMatchObject({ settings: { instant: 'custom-luna-none' } })
        return new Promise((_resolve, reject) => { rejectWrite = reject })
      }
      return null
    } }
    const { container } = await renderApp()
    const inline = container.querySelector<HTMLSelectElement>('#composerInstantSelect')!
    expect(inline?.value).toBe('custom-sol-high')
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Synthetic preserved draft')
      setSelectValue(inline, 'custom-luna-none')
      await Promise.resolve()
    })
    expect(inline.disabled).toBe(true)
    expect(getSendButton(container).disabled).toBe(true)
    await act(async () => { flushSync(() => getButtonByLabel(container, 'Settings').click()); navigateSettings(container, 'models'); await Promise.resolve() })
    const settings = container.querySelector<HTMLSelectElement>('#assistantInstantSelect')!
    expect(settings.value).toBe('custom-luna-none')
    expect(settings.disabled).toBe(true)
    await act(async () => { setSelectValue(settings, 'custom-luna-low'); await Promise.resolve() })
    expect(calls.filter((command) => command === 'set_assistant_settings')).toHaveLength(1)
    await act(async () => { rejectWrite(new Error('Synthetic settings failure')); await Promise.resolve() })
    expect(inline.value).toBe('custom-sol-high')
    expect(settings.value).toBe('custom-sol-high')
    expect(inline.disabled).toBe(false)
    const dialog = container.querySelector<HTMLElement>('[role="dialog"]')!
    dialog.scrollTop = 400
    const error = dialog.querySelector('[role="alert"]')!
    expect(error.textContent).toContain('Assistant settings could not be saved: Synthetic settings failure')
    expect(error.closest('.settings-panel__header')).not.toBeNull()
    expect(error.closest('details')).toBeNull()
    await closeSettings(container)
    expect(getComposer(container).value).toBe('Synthetic preserved draft')
    expect(calls).not.toContain('cancel_prompt')
    expect(calls).not.toContain('submit_prompt')
  })

  it('locks inline model actions throughout request execution and explicit Stop without implicit cancellation or resubmission', async () => {
    const calls: string[] = []
    let finishPrompt: (value: unknown) => void = () => undefined
    let finishStop: (value: unknown) => void = () => undefined
    window.__TAURI_INTERNALS__ = { invoke: async (command) => {
      calls.push(command)
      if (command === 'get_app_snapshot') return appSnapshot()
      if (command === 'get_startup_state') return readyStartupState({ voice_input_available: false, prompt_cancellation_available: true })
      if (command === 'submit_prompt') return new Promise((resolve) => { finishPrompt = resolve })
      if (command === 'cancel_prompt') return new Promise((resolve) => { finishStop = resolve })
      return null
    } }
    const { container } = await renderApp()
    const inline = container.querySelector<HTMLSelectElement>('#composerInstantSelect')!
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Synthetic pending prompt')
      getSendButton(container).click()
      await Promise.resolve()
    })
    expect(inline.disabled).toBe(true)
    expect(container.querySelector<HTMLImageElement>('.shell__logo')?.dataset['busy']).toBe('true')
    await act(async () => { setSelectValue(inline, 'local-quality'); await Promise.resolve() })
    expect(calls).not.toContain('cancel_prompt')
    expect(calls).not.toContain('switch_response_profile')
    expect(calls).not.toContain('set_assistant_settings')
    await act(async () => { getButtonByText(container, 'Stop').click(); await Promise.resolve() })
    expect(inline.disabled).toBe(true)
    await act(async () => { setSelectValue(inline, 'custom-luna-none'); await Promise.resolve() })
    expect(calls.filter((command) => command === 'cancel_prompt')).toHaveLength(1)
    expect(calls.filter((command) => command === 'submit_prompt')).toHaveLength(1)
    await act(async () => {
      finishStop({ cancelled: true })
      finishPrompt({ kind: 'cancelled' })
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(inline.disabled).toBe(false)
    expect(inline.value).toBe('local-fast')
    expect(container.querySelector<HTMLImageElement>('.shell__logo')?.dataset['busy']).toBe('false')
  })
  it.each([[450, 1250, '450 ms'], [999, 1000, '999 ms'], [1100, 1234, '1.1 s']])('settles desktop Channel updates against canonical snapshots without losing timing or source details (%s/%s)', async (firstText, completed, firstLabel) => {
    const committed: Record<string, unknown>[] = []
    let send: ((payload: unknown) => void) | undefined
    window.__TAURI_INTERNALS__ = {
      createChannel: <T,>(handler: (payload: T) => void) => {
        send = handler as (payload: unknown) => void
        return 'desktop-channel'
      },
      listen: async (event) => {
        expect(event).not.toBe('prompt-execution-event')
        return () => undefined
      },
      invoke: async (command, args) => {
        if (command === 'get_startup_state') return readyStartupState({ voice_input_available: false })
        if (command === 'get_app_snapshot') return appSnapshot(committed, { instant: 'custom-luna-none' })
        if (command === 'submit_prompt') {
          const request = args as { requestId: string; updates: unknown }
          expect(request.updates).toBe('desktop-channel')
          if (committed.length > 0) {
            committed.push(canonicalTurn('303', '304', 'Second synthetic question', 'Second canonical answer', request.requestId))
            return { kind: 'completed', save_disposition: 'saved', turn: committed.at(-1)! }
          }
          send?.({ kind: 'stage', stage: 'instant', status: 'running', detail: null })
          send?.({ kind: 'text', text: 'Provisional answer' })
          send?.({ kind: 'custom_direct_timings', first_text_ms: firstText, completed_ms: completed })
          send?.({ kind: 'sources', sources: [{ url: 'https://example.com/evidence', title: 'Synthetic evidence' }] })
          committed.push({ ...canonicalTurn('301', '302', 'Synthetic question', 'Canonical answer', request.requestId),
            sources: [{ url: 'https://example.com/evidence', title: 'Synthetic evidence' }] })
          return { kind: 'completed', save_disposition: 'saved', turn: committed.at(-1)! }
        }
        return null
      },
    }
    const { container } = await renderApp()
    await act(async () => { setTextAreaValue(getComposer(container), 'Synthetic question') })
    await act(async () => {
      expect(getSendButton(container).disabled, container.textContent ?? '').toBe(false)
      getSendButton(container).click()
    })
    expect(container.querySelectorAll('.message--assistant')).toHaveLength(1)
    expect(getConversation(container).textContent).toContain('Canonical answer')
    expect(getConversation(container).textContent).not.toContain('Provisional answer')
    expect(container.textContent).toContain(firstLabel)
    expect(container.textContent).not.toContain('Completed:')
    const details = container.querySelector<HTMLDetailsElement>('.answer-stage__details')
    expect(details).not.toBeNull()
    expect(container.querySelector('a[href="https://example.com/evidence"]')).not.toBeNull()
    await act(async () => { send?.({ kind: 'text', text: 'late text' }); await Promise.resolve() })
    expect(container.textContent).not.toContain('late text')
    await act(async () => { setTextAreaValue(getComposer(container), 'Second synthetic question') })
    await act(async () => { getSendButton(container).click() })
    expect(container.querySelectorAll('.message--assistant')).toHaveLength(2)
    expect(container.textContent).toContain('Second canonical answer')
    expect(container.textContent).toContain(firstLabel)
    expect(container.textContent).not.toContain('Completed:')
    expect(container.querySelector('a[href="https://example.com/evidence"]')).not.toBeNull()
  })

  it('auto-follows the latest message when conversation grows', async () => {
    const unexpected: string[] = []
    let updates: (value: unknown) => void = () => undefined
    let frame: FrameRequestCallback | undefined
    window.requestAnimationFrame = vi.fn((callback) => { frame = callback; return 1 })
    window.__TAURI_INTERNALS__ = {
      createChannel: <T,>(handler: (value: T) => void) => { updates = handler as (value: unknown) => void; return {} },
      invoke: async (command) => {
        if (command === 'get_startup_state') return readyStartupState({ voice_input_available: false })
        if (command === 'get_app_snapshot') return appSnapshot()
        if (command === 'submit_prompt') return new Promise(() => undefined)
        if (command === 'cancel_prompt') return null
        if (command === 'get_update_snapshot' || command === 'check_for_update') return updateSnapshot()
        if (command === 'record_frontend_runtime_diagnostic') return null
        unexpected.push(command)
        throw new Error(`Unexpected scroll fixture command: ${command}`)
      },
    }
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

    await act(async () => {
      setTextAreaValue(composer, 'Scroll check prompt')
    })

    await act(async () => {
      sendButton.click()
    })
    expect(composer.value).toBe('')
    expect(container.querySelector('.message__thinking')).not.toBeNull()
    scrollToSpy.mockClear()
    await act(async () => { updates({ kind: 'text', text: 'Synthetic answer-only growth' }) })
    expect(container.querySelector('.answer-stage__answer')?.textContent).toBe('Synthetic answer-only growth')
    expect(scrollToSpy).toHaveBeenCalledOnce()
    scrollToSpy.mockClear()
    await act(async () => { updates({ kind: 'text', text: ' with a later delta' }) })
    expect(scrollToSpy).not.toHaveBeenCalled()
    expect(frame).toBeTypeOf('function')
    await act(async () => { frame!(16) })
    expect(container.querySelector('.answer-stage__answer')?.textContent).toBe('Synthetic answer-only growth with a later delta')
    expect(composer.value).toBe('')
    expect(scrollToSpy).toHaveBeenCalledOnce()
    expect(unexpected).toEqual([])
  })

  it('renders one response-first semantic shell with a single reset affordance', async () => {
    const { container } = await renderApp()
    const header = container.querySelector('header.shell__header')
    const shell = getShell(container)

    expect(header).not.toBeNull()
    expect(header?.querySelector('[data-shell-identity] img')?.getAttribute('alt')).toBe('VoxGolem')
    expect(header?.querySelector('[data-browser-preview]')?.textContent).toBe('Browser preview')
    expect(header?.querySelector('[data-runtime-phase]')).toBeNull()
    expect(header?.querySelector('[aria-label="Theme"]')).toBeNull()
    expect(header?.querySelector('[aria-label="Settings"]')).not.toBeNull()
    expect(container.querySelector('.composer__utility-actions [aria-label="New Chat"]')).not.toBeNull()
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
      'Browser preview only: no backend response was generated. Prompt: Draft release notes',
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

  it('cancels a delayed browser preview through the shared Stop control', async () => {
    vi.useFakeTimers()
    configureBrowserPreviewFixture({ delayMs: 50, chunks: ['Delayed browser response'] })
    const { container } = await renderApp()

    await act(async () => {
      setTextAreaValue(getComposer(container), 'Cancel browser preview')
      getSendButton(container).click()
      await Promise.resolve()
    })
    const stopButton = container.querySelector<HTMLButtonElement>('[aria-label="Stop"]')
    expect(stopButton).not.toBeUndefined()

    await act(async () => {
      stopButton?.click()
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(getConversation(container).querySelectorAll('.message')).toHaveLength(0)
    expect(container.textContent).toContain('Response cancelled')
    await act(async () => { await vi.advanceTimersByTimeAsync(200) })
    expect(getConversation(container).querySelectorAll('.message')).toHaveLength(0)
  })

  it('starts with an empty chat transcript', async () => {
    const { container } = await renderApp()
    const conversation = getConversation(container)

    expect(conversation.querySelectorAll('.message')).toHaveLength(0)
    expect(conversation.querySelector('[aria-label="New conversation"]')).not.toBeNull()
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
      flushSync(() => getButtonByLabel(container, 'Settings').click()); navigateSettings(container, 'voice')
      await Promise.resolve()
    })

    expect(getConversation(container).textContent).toContain('Hydrated question')
    expect(getConversation(container).textContent).toContain('Hydrated answer')
    expect(getShell(container).dataset['uiTheme']).toBe('light')
    expect(getShell(container).dataset['uiTextSize']).toBe('large')
    expect(getTtsToggle(container).checked).toBe(true)
    await act(async () => { getButtonByLabel(container, 'Back to settings').click(); await Promise.resolve() })
    await act(async () => { navigateSettings(container, 'models') })
    expect(container.querySelector<HTMLSelectElement>('#assistantInstantSelect')?.value).toBe('custom-sol-high')
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
      flushSync(() => getButtonByLabel(container, 'Settings').click()); navigateSettings(container, 'models')
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
    })
    const availability = container.querySelector('.composer__surface .composer__availability')
    expect(availability?.textContent).toContain('Sending disabled: Detailed provider authentication failure')
    expect(container.querySelectorAll('.composer__availability')).toHaveLength(1)
    expect(Array.from(container.querySelectorAll('p')).filter((paragraph) => paragraph.textContent?.includes('Detailed provider authentication failure'))).toHaveLength(1)
    expect(availability?.querySelector('[aria-label="Retry Custom"]')).toBeNull()
    expect(getSendButton(container).disabled).toBe(true)
    await act(async () => {
      flushSync(() => getButtonByLabel(container, 'Settings').click()); navigateSettings(container, 'models')
      await Promise.resolve()
    })
    const selected = container.querySelector<HTMLOptionElement>('#assistantInstantSelect option[value="custom-sol-high"]')
    expect(selected?.title).toBe('Detailed provider authentication failure')
  })

  it('hides a model omitted from the authoritative snapshot without silently selecting another', async () => {
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
      flushSync(() => getButtonByLabel(container, 'Settings').click()); navigateSettings(container, 'models')
      await Promise.resolve()
    })
    const localFast = container.querySelector<HTMLOptionElement>('#assistantInstantSelect option[value="local-fast"]')
    expect(localFast).toBeNull()
    expect(container.querySelector<HTMLSelectElement>('#assistantInstantSelect')?.value).toBe('')
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
      flushSync(() => getButtonByLabel(container, 'Settings').click()); navigateSettings(container, 'models')
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
      flushSync(() => getButtonByLabel(container, 'Settings').click()); navigateSettings(container, 'models')
      getButtonByLabel(container, 'Retry local profile').click()
    })
    expect(snapshotCalls).toBe(2)
    expect(container.querySelector('[aria-label="Retry local profile"]')).toBeNull()
    await closeSettings(container)
    await act(async () => setTextAreaValue(getComposer(container), 'Available after retry'))
    expect(getSendButton(container).disabled).toBe(false)
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
          return { kind: 'refused', save_disposition: 'saved', turn: canonicalTurn('85', '86', 'New reused prompt', 'New reused refusal', requestId, 'refusal') }
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

  it('bounds cancellation reconciliation retries and removes only the pending projection', async () => {
    vi.useFakeTimers()
    let finishPrompt: ((value: unknown) => void) | undefined
    let snapshotCalls = 0
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          if (snapshotCalls === 1) {
            return appSnapshot([canonicalTurn('75', '76', 'Retained question', 'Retained answer')])
          }
          throw new Error('snapshot unavailable')
        }
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'submit_prompt') {
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
      finishPrompt?.({ kind: 'cancelled' })
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

  it('locks both model selectors until canonical reconciliation settles, without cancelling or switching', async () => {
    const calls: string[] = []
    let finishPrompt: ((value: unknown) => void) | undefined
    let resolveReconciliation: (value: unknown) => void = () => undefined
    let requestId = ''
    let snapshotCalls = 0
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command, args) => {
        calls.push(command)
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
      finishPrompt?.(savedOutcome(requestId, 'Reconcile during profile switch', 'Canonical answer', '1', '80'))
      await Promise.resolve()
      await Promise.resolve()
      flushSync(() => getButtonByLabel(container, 'Settings').click()); navigateSettings(container, 'models')
      await Promise.resolve()
      setSelectValue(container.querySelector<HTMLSelectElement>('#assistantInstantSelect')!, 'quality')
      await Promise.resolve()
    })
    expect(container.querySelector<HTMLSelectElement>('#assistantInstantSelect')?.disabled).toBe(true)
    expect(container.querySelector<HTMLSelectElement>('#composerInstantSelect')?.disabled).toBe(true)
    expect(calls).not.toContain('switch_response_profile')
    expect(calls).not.toContain('cancel_prompt')
    expect(calls.filter((command) => command === 'submit_prompt')).toHaveLength(1)

    await act(async () => {
      resolveReconciliation(appSnapshot(
        [canonicalTurn('79', '80', 'Reconcile during profile switch', 'Canonical answer', requestId)],
        {},
        { runtime_phase: 'sleeping' },
      ))
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(getShell(container).dataset['runtimeStatus']).toBe('sleeping')
    expect(container.querySelector<HTMLSelectElement>('#assistantInstantSelect')?.disabled).toBe(false)
    expect(container.querySelector<HTMLSelectElement>('#composerInstantSelect')?.disabled).toBe(false)
  })

  it.each(['text', 'refusal'] as const)('accepts a newly committed canonical %s turn after transport rejection', async (kind) => {
    let snapshotCalls = 0
    let requestId = ''
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
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Committed after rejection')
      getSendButton(container).click()
    })

    expect(getConversation(container).textContent).toContain('Canonical accepted answer')
    expect(container.textContent).not.toContain('Response failed')
    expect(getResponseAnnouncer(container).textContent).toBe(
      `${kind === 'refusal' ? 'Assistant refusal' : 'Assistant response'}: Canonical accepted answer`,
    )
    expect(rendererAudioCalls).toEqual([])
  })

  it('does not accept an old canonical turn with a reused request ID', async () => {
    Date.now = () => 2_000
    Math.random = () => 0
    const requestId = 'request-2000-'
    const oldTurn = canonicalTurn('201', '202', 'Old reused question', 'Old reused answer', requestId)
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command) => {
        if (command === 'get_app_snapshot') return appSnapshot([oldTurn], { tts_enabled: true })
        if (command === 'get_startup_state') return readyStartupState({ tts_enabled: true })
        if (command === 'submit_prompt') throw new Error('transport failed without commit')
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'New reused question')
      getSendButton(container).click()
    })

    expect(getConversation(container).textContent).toContain('Old reused answer')
    expect(container.textContent).toContain('Response failed')
    expect(getResponseAnnouncer(container).textContent).toBe('')
    expect(rendererAudioCalls).toEqual([])
  })

  it.each(['cancelled', 'error'] as const)('does not speak an uncommitted correction after %s settlement', async (outcome) => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let finishPrompt: (value: unknown) => void = () => undefined
    let requestId = ''
    let snapshotCalls = 0
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
      finishPrompt(outcome === 'error' ? { kind: 'error', request_id: requestId, runtime_phase: 'sleeping', error_message: 'explicit failure' } : { kind: 'cancelled' })
    })

    expect(snapshotCalls).toBe(2)
    expect(rendererAudioCalls).toEqual([])
    expect(getResponseAnnouncer(container).textContent).toBe('')
  })

  it('publishes the canonical answer after a provisional correction without renderer speech replay', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let requestId = ''
    let snapshotCalls = 0
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
          return savedOutcome(requestId, 'Corrected canonical prompt', 'Canonical corrected answer', '1', '232')
        }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Corrected canonical prompt')
      getSendButton(container).click()
    })

    expect(rendererAudioCalls).toEqual([])
    expect(getResponseAnnouncer(container).textContent).toBe('Assistant response: Canonical corrected answer')
  })

  it.each(['cancelled', 'error'] as const)('preserves explicit %s despite a new matching canonical turn', async (outcome) => {
    let requestId = ''
    let snapshotCalls = 0
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
          return outcome === 'error' ? { kind: 'error', request_id: requestId, runtime_phase: 'sleeping', error_message: 'authoritative failure' } : { kind: 'cancelled' }
        }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Explicit terminal prompt')
      getSendButton(container).click()
    })

    expect(container.textContent).toContain(outcome === 'error' ? 'Response failed' : 'Response cancelled')
    expect(getResponseAnnouncer(container).textContent).toBe('')
    expect(rendererAudioCalls).toEqual([])
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
      finishPrompt?.(savedOutcome(activeRequestId, 'Canonical question', 'Canonical answer', '1', '12'))
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
          return savedOutcome(requestId, 'Session two prompt', 'Session two answer', '2', '212')
        }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      flushSync(() => getButtonByLabel(container, 'Settings').click()); navigateSettings(container, 'voice')
    })
    await closeSettings(container)
    await act(async () => {
      getNewChatButton(container).click()
    })
    await act(async () => {
      resolveInitialSnapshot(appSnapshot([
        canonicalTurn('221', '222', 'Stale session one', 'Stale session answer'),
      ]))
    })
    await closeSettings(container)
    await openSettings(container); await act(async () => { navigateSettings(container, 'voice') })
    await act(async () => {
      getTtsToggle(container).click()
      await Promise.resolve()
    })
    await closeSettings(container)
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Session two prompt')
      getSendButton(container).click()
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
          return savedOutcome(requestId, 'Prompt after reset retry', 'Answer after reset retry', '2', '252')
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
    })
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Prompt after reset retry')
      getSendButton(container).click()
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
      flushSync(() => getButtonByLabel(container, 'Settings').click()); navigateSettings(container, 'models')
    })
    await closeSettings(container)
    await act(async () => {
      getNewChatButton(container).click()
    })

    expect(container.textContent).not.toContain('Assistant settings unavailable: legacy settings failed')
    await openSettings(container); await act(async () => { navigateSettings(container, 'models') })
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
      finishPrompt?.(savedOutcome(requestId, 'First prompt', 'Committed answer', '1', '32'))
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
    nativeObservationEnabled = true
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
          return cancelledRequests.length === 0
            ? appSnapshot([], {}, { runtime_phase: 'executing', active_request: activeRequest })
            : appSnapshot()
        }
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'get_desktop_request_owner') {
          expect(args).toEqual({ requestId: 'orphan-request', executionId: activeRequest.execution_id })
          return 'renderer'
        }
        if (command === 'cancel_prompt') {
          expect(args).toEqual({ requestId: 'orphan-request', expectedExecutionId: activeRequest.execution_id })
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
      changeTheme(container, 'light')
      await Promise.resolve()
      flushSync(() => getButtonByLabel(container, 'Settings').click()); navigateSettings(container, 'text')
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
      await openSettings(container); await act(async () => { navigateSettings(container, 'voice') })
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
      await openSettings(container); await act(async () => { navigateSettings(container, 'voice') })
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
    await openSettings(container); await act(async () => { navigateSettings(container, 'voice') })
    expect(getTtsToggle(container).checked).toBe(true)
    await act(async () => {
      resolveStartup(readyStartupState({ tts_enabled: false }))
      await Promise.resolve()
      await Promise.resolve()
    })
    await closeSettings(container)
    await openSettings(container); await act(async () => { navigateSettings(container, 'voice') })
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
    await openSettings(container); await act(async () => { navigateSettings(container, 'voice') })
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
      await openSettings(container); await act(async () => { navigateSettings(container, 'voice') })
      getTtsToggle(container).click()
      await Promise.resolve()
    })
    expect(getTtsToggle(container).checked).toBe(true)
    await closeSettings(container)
    await act(async () => {
      await openSettings(container); await act(async () => { navigateSettings(container, 'voice') })
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

  it('resets browser preview history through the simulated adapter', async () => {
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

    expect(getConversation(container).querySelectorAll('.message')).toHaveLength(0)
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
      promptEventHandler?.({ payload: { request_id: requestId, kind: 'text', text: 'Visible first output' } })
      promptEventHandler?.({ payload: { request_id: requestId, kind: 'text', text: 'Queued output' } })
    })
    expect(requestFrame).toHaveBeenCalledTimes(1)
    const root = mountedRoots.pop()
    expect(root).toBeDefined()
    act(() => root?.unmount())
    expect(cancelFrame).toHaveBeenCalledWith(73)
    await act(async () => {
      scheduledFrame?.(16)
      finishPrompt?.(savedOutcome(requestId, 'Unmount prompt', 'Visible first outputQueued output'))
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
      finishPrompt?.(savedOutcome(requestId, 'Strict prompt', 'Strict answer', '1', '42'))
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(getConversation(container).textContent).toContain('Strict answer')
    expect(container.textContent).not.toContain('Synchronizing response')
  })

  it('refreshes provider controls from the post-terminal snapshot', async () => {
    let snapshotCalls = 0
    const expiredCapabilities = completeCapabilities().map((capability) => capability['id'] === 'custom_provider'
      ? { ...capability, state: 'unavailable', reason: 'OpenCode authentication has expired' }
      : capability)
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          return snapshotCalls === 1
            ? appSnapshot([], { instant: 'custom-sol-high' })
            : appSnapshot([], { instant: 'custom-sol-high' }, {
                capabilities: expiredCapabilities,
                model_options: [
                  { id: 'custom-sol-high', label: 'Sol High', available: false, unavailable_reason: 'OpenCode authentication has expired' },
                  { id: 'custom-luna-low', label: 'Luna Low', available: false, unavailable_reason: 'OpenCode authentication has expired' },
                ],
              })
        }
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'submit_prompt') throw { code: 'unavailable', message: 'OpenCode authentication has expired' }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Expired auth prompt')
    })
    expect(getSendButton(container).disabled).toBe(false)
    await act(async () => {
      getSendButton(container).click()
      await Promise.resolve()
      await Promise.resolve()
      await Promise.resolve()
    })
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Retry after expiry')
    })

    expect(snapshotCalls).toBe(2)
    expect(getSendButton(container).disabled).toBe(true)
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
    await act(async () => { changeTheme(container, 'light') })
    await act(async () => { changeTheme(container, 'dark') })
    await act(async () => {
      resolveFirstTheme('light')
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(getShell(container).dataset['uiTheme']).toBe('light')

    await act(async () => {
      flushSync(() => getButtonByLabel(container, 'Settings').click()); navigateSettings(container, 'text')
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
    await act(async () => { changeTheme(container, 'light') })
    await act(async () => { changeTheme(container, 'dark') })
    await act(async () => {
      rejectFirstTheme(new Error('first theme write failed'))
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(themeWrites).toBe(2)
    expect(getShell(container).dataset['uiTheme']).toBe('dark')

    await act(async () => {
      flushSync(() => getButtonByLabel(container, 'Settings').click()); navigateSettings(container, 'text')
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
    await openSettings(container); await act(async () => { navigateSettings(container, 'voice') }); await act(async () => { getTtsToggle(container).click() })
    await closeSettings(container)
    await openSettings(container); await act(async () => { navigateSettings(container, 'voice') })
    await act(async () => { getTtsToggle(container).click() })
    await act(async () => {
      rejectFirstTts(new Error('first TTS write failed'))
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(ttsWrites).toBe(2)
    expect(getTtsToggle(container).checked).toBe(false)
  })

  it.each([
    { kind: 'text' as const, finalStage: 'deep' as const, answer: 'Canonical deep answer' },
    { kind: 'refusal' as const, finalStage: 'review' as const, answer: 'Canonical refusal' },
  ])('projects canonical $kind semantics and $finalStage completion without streamed text', async ({ kind, finalStage, answer }) => {
    let requestId = ''
    let committed = false
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
          return { kind: kind === 'refusal' ? 'refused' : 'completed', save_disposition: 'saved', turn: canonicalTurn('51', '52', 'Canonical terminal prompt', answer, requestId, kind, finalStage) }
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
    expect(rendererAudioCalls).toEqual([])
    expect(getResponseAnnouncer(container).textContent).toBe(
      `${kind === 'refusal' ? 'Assistant refusal' : 'Assistant response'}: ${answer}`,
    )
  })

  it('ignores late prompt events after unmount', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let requestId = ''
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
    expect(rendererAudioCalls).toEqual([])
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

  it('keeps the typed shell nonfatal with speech diagnostics in Settings and other unavailable capabilities in one notice', async () => {
    const capabilityIds = [
      'custom_provider', 'opencode', 'local_fast', 'local_quality', 'qwen_prediction',
      'wake_word', 'vad', 'parakeet', 'tts', 'deep', 'review',
    ]
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_ui_text_size') return 'medium'
        if (command === 'get_ui_theme') return 'dark'
        if (command === 'get_assistant_settings') return defaultAssistantSettings()
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
      if (['wake_word', 'vad', 'parakeet', 'tts'].includes(id)) expect(container.textContent).not.toContain(`${id} unavailable`)
      else expect(container.textContent).toContain(`${id} unavailable`)
    }
    const settingsButton = getButtonByLabel(container, 'Settings')
    await act(async () => { settingsButton.click(); await Promise.resolve() })
    await act(async () => { navigateSettings(container, 'voice') })
    for (const id of ['wake_word', 'vad', 'parakeet', 'tts']) expect(container.querySelector('[role="dialog"]')?.textContent).toContain(`${id} unavailable`)
    await act(async () => { getButtonByLabel(container, 'Back to settings').click(); await Promise.resolve() })
    await act(async () => { navigateSettings(container, 'models') })
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
      flushSync(() => getButtonByLabel(container, 'Settings').click()); navigateSettings(container, 'text')
      await Promise.resolve()
    })

    expect(container.textContent).toContain('Text size')
    expect(container.querySelector('[aria-label="Text size preview"]')?.textContent).toContain('A clear view of your conversation')
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
      flushSync(() => getButtonByLabel(container, 'Settings').click()); navigateSettings(container, 'text')
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

  it('starts in persisted dark mode and roundtrips Dark and Light through Settings', async () => {
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
    await act(async () => { getButtonByLabel(container, 'Settings').click() })
    expect(getThemeRadio(container, 'dark').checked).toBe(true)

    expect(Array.from(container.querySelectorAll<HTMLInputElement>('input[name="uiTheme"]')).map((radio) => radio.value)).toEqual(['dark', 'light'])

    await act(async () => {
      changeTheme(container, 'light')
      await Promise.resolve()
    })

    expect(shell.dataset['uiTheme']).toBe('light')
    expect(getThemeRadio(container, 'light').checked).toBe(true)
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
      changeTheme(container, 'light')
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
    await act(async () => { getButtonByLabel(container, 'Settings').click() })
    expect(getThemeRadio(container, 'dark').checked).toBe(true)
  })

  it.each(['light'] as const)('reverts %s theme and shows a notice when theme persistence fails', async (theme) => {
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
      changeTheme(container, theme)
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(getShell(container).dataset['uiTheme']).toBe('dark')
    expect(container.textContent).toContain('Theme not saved')
    expect(container.textContent).toContain('theme write failed')
  })

  it('keeps desktop theme failures in the open Settings header and clears them on retry without stale failures', async () => {
    let writes = 0
    let rejectEarlier: (error: Error) => void = () => undefined
    let resolveLatest: (theme: string) => void = () => undefined
    window.__TAURI_INTERNALS__ = { invoke: async (command) => {
      if (command === 'get_startup_state') return readyStartupState({ voice_input_available: false })
      if (command === 'get_ui_theme') return 'dark'
      if (command === 'get_assistant_settings') return defaultAssistantSettings()
      if (command === 'set_ui_theme') {
        writes += 1
        if (writes === 1) throw new Error('Synthetic theme failure')
        if (writes === 2) return new Promise((_, reject) => { rejectEarlier = reject })
        return new Promise((resolve) => { resolveLatest = resolve })
      }
      return null
    } }
    const { container } = await renderApp()
    await act(async () => { getButtonByLabel(container, 'Settings').click() })
    await act(async () => { changeTheme(container, 'light'); await Promise.resolve() })
    const dialog = container.querySelector<HTMLElement>('[role="dialog"]')!
    expect(dialog).not.toBeNull()
    expect(getShell(container).dataset['uiTheme']).toBe('dark')
    const error = dialog.querySelector('.settings-panel__write-error[role="alert"]')
    expect(error?.textContent).toContain('Theme could not be saved: Synthetic theme failure')
    expect(error?.closest('.settings-panel__header')).not.toBeNull()
    await act(async () => { changeTheme(container, 'light') })
    expect(dialog.querySelector('.settings-panel__write-error')).toBeNull()
    await act(async () => { changeTheme(container, 'dark') })
    await act(async () => { rejectEarlier(new Error('Synthetic stale failure')); await Promise.resolve(); await Promise.resolve() })
    expect(writes).toBe(3)
    expect(dialog.querySelector('.settings-panel__write-error')).toBeNull()
    expect(getShell(container).dataset['uiTheme']).toBe('dark')
    await act(async () => { resolveLatest('dark'); await Promise.resolve() })
    expect(dialog.querySelector('.settings-panel__write-error')).toBeNull()
    expect(getShell(container).dataset['uiTheme']).toBe('dark')
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
      flushSync(() => getButtonByLabel(container, 'Settings').click()); navigateSettings(container, 'text')
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

  it.each([false, true])('traps Settings focus at visible disclosure boundaries (open=%s)', async (open) => {
    function SettingsFocusHarness() {
      const navigation = useSettingsNavigation()
      return <>
        <button ref={navigation.settingsButtonRef} onClick={navigation.openSettings}>Open synthetic Settings</button>
        {navigation.settingsOpen ? <div ref={(panel) => { navigation.settingsPanelRef.current = panel }} onKeyDown={navigation.onSettingsKeyDown}>
          <button ref={navigation.settingsCloseButtonRef}>Close synthetic Settings</button>
          <details><summary>Advanced model settings</summary><input aria-label="Collapsed advanced input" /></details>
          <details open={open}>
            <summary><span data-focus-boundary="summary">Speech details and diagnostics</span></summary>
            <button data-focus-boundary="body">Synthetic speech action</button>
            <details><summary data-focus-boundary="nested">Nested diagnostics</summary><input aria-label="Nested collapsed input" /></details>
          </details>
          <div hidden><button>Hidden descendant</button><summary>Hidden summary</summary></div>
          <div inert><a href="#synthetic">Inert descendant</a></div>
          <input type="hidden" />
          <button hidden>Hidden button</button>
          <button disabled>Disabled button</button>
          <fieldset disabled><input aria-label="Disabled fieldset input" /></fieldset>
          <button tabIndex={-1}>Explicitly unfocusable button</button>
          <summary tabIndex={-1}>Explicitly unfocusable summary</summary>
        </div> : null}
      </>
    }
    const container = document.createElement('div')
    document.body.append(container)
    const root = createRoot(container)
    mountedContainers.push(container)
    mountedRoots.push(root)
    await act(async () => { root.render(<SettingsFocusHarness />) })
    await act(async () => { container.querySelector('button')!.click() })
    const close = Array.from(container.querySelectorAll('button')).find((button) => button.textContent === 'Close synthetic Settings')!
    const last = open
      ? container.querySelector<HTMLElement>('[data-focus-boundary="nested"]')!
      : container.querySelector('[data-focus-boundary="summary"]')!.parentElement!
    expect(document.activeElement).toBe(close)
    await act(async () => { close.dispatchEvent(new KeyboardEvent('keydown', { key: 'Tab', shiftKey: true, bubbles: true, cancelable: true })) })
    expect(document.activeElement).toBe(last)
    await act(async () => { last.dispatchEvent(new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true })) })
    expect(document.activeElement).toBe(close)
    if (open) {
      const outer = container.querySelector<HTMLDetailsElement>('details[open]')!
      outer.querySelector('details')!.remove()
      await act(async () => { close.dispatchEvent(new KeyboardEvent('keydown', { key: 'Tab', shiftKey: true, bubbles: true })) })
      const body = outer.querySelector<HTMLButtonElement>('[data-focus-boundary="body"]')!
      expect(document.activeElement).toBe(body)
      await act(async () => { body.dispatchEvent(new KeyboardEvent('keydown', { key: 'Tab', bubbles: true })) })
      expect(document.activeElement).toBe(close)
      await act(async () => { outer.querySelector('summary')!.click() })
      expect(outer.open).toBe(false)
      await act(async () => { close.dispatchEvent(new KeyboardEvent('keydown', { key: 'Tab', shiftKey: true, bubbles: true })) })
      expect(document.activeElement).toBe(outer.querySelector('summary'))
    }
  })

  it('supports keyboard focus and Escape close for the settings overlay', async () => {
    vi.useFakeTimers()
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
      settingsButton.closest('details')!.querySelector('summary')!.click()
      settingsButton.click()
      await Promise.resolve()
    })

    const closeButton = getButtonByLabel(container, 'Close settings')
    expect(document.activeElement).toBe(closeButton)

    await act(async () => {
      closeButton.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
      await vi.advanceTimersByTimeAsync(0)
      await vi.advanceTimersByTimeAsync(0)
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
    vi.useFakeTimers()
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
      await vi.advanceTimersByTimeAsync(0)
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
      flushSync(() => getButtonByLabel(container, 'Settings').click()); navigateSettings(container, 'text')
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
      'Browser preview only: no backend response was generated. Prompt: Line one',
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
      'Browser preview only: no backend response was generated. Prompt: Line one',
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
      'Browser preview only: no backend response was generated. Prompt: Line one',
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
    await openSettings(container); await act(async () => { navigateSettings(container, 'voice') })
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

  it('disables normal runtime controls while update installation owns the lifecycle', async () => {
    vi.useFakeTimers()
    let updateStateHandler: ((event: { payload: unknown }) => void) | undefined
    window.__TAURI_INTERNALS__ = {
      listen: async (event, handler) => {
        if (event === 'app-update-state') updateStateHandler = handler
        return () => undefined
      },
      invoke: async (command) => {
        if (command === 'get_startup_state') return readyStartupState({ tts_enabled: false })
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        if (command === 'get_update_snapshot') return updateSnapshot({ phase: 'up_to_date', version: null })
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0)
      setTextAreaValue(getComposer(container), 'Do not submit during installation')
    })
    expect(getSendButton(container).disabled).toBe(false)

    await act(async () => {
      updateStateHandler?.({
        payload: {
          ...updateSnapshot({
            revision: 3,
            phase: 'restart_required',
            reason: 'Restart to finish the update.',
          }),
        },
      })
      await Promise.resolve()
    })

    expect(getSendButton(container).disabled).toBe(true)
    const microphoneControl = Array.from(container.querySelectorAll<HTMLButtonElement>('button'))
      .find((button) => ['Start mic', 'Stop mic'].includes(button.getAttribute('aria-label') ?? ''))
    expect(microphoneControl?.disabled).toBe(true)
    await openSettings(container); await act(async () => { navigateSettings(container, 'voice') })
    expect(getTtsToggle(container).disabled).toBe(true)
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
    expect(Array.from(select.options).map((option) => option.text)).toEqual(
      MODEL_CATALOG.filter((item) => item.id !== 'gemini-nano').map((item) => item.label),
    )
  })

  it('invokes switch_response_profile when selecting Quality profile', async () => {
    const invokedCommands: string[] = []
    let selectedProfile: 'fast' | 'quality' = 'fast'

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        invokedCommands.push(command)

        if (command === 'get_assistant_settings') return defaultAssistantSettings()

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
        if (command === 'get_assistant_settings') return defaultAssistantSettings()
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
    vi.useFakeTimers()
    const invokedCommands: string[] = []
    let startupStateCallCount = 0

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        invokedCommands.push(command)

        if (command === 'chatgpt_auth') return { phase: 'signed_out' }

        if (command === 'get_assistant_settings') return defaultAssistantSettings()

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
      await vi.advanceTimersByTimeAsync(600)
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
        if (command === 'get_assistant_settings') return defaultAssistantSettings()
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

  it('keeps local prompts disabled when profile startup and restoration both fail', async () => {
    let startupStateCallCount = 0
    let submitCalls = 0

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') {
          if (startupStateCallCount < 2) return appSnapshot()
          return appSnapshot([], {}, { capabilities: completeCapabilities({ local_fast: 'failed', local_quality: 'failed' }),
            model_options: (appSnapshot()['model_options'] as Record<string, unknown>[]).map((option) => ['local-fast', 'local-quality'].includes(String(option['id']))
              ? { ...option, available: false, unavailable_reason: 'Synthetic native local runtime unavailable' } : option) })
        }
        if (command === 'get_startup_state') {
          startupStateCallCount += 1
          if (startupStateCallCount === 1) return readyStartupState()
          return readyStartupState({
            selected_response_profile: 'fast',
            capabilities: completeCapabilities({
              local_fast: 'failed',
              local_quality: 'failed',
            }).map((capability) => ['local_fast', 'local_quality'].includes(String(capability['id']))
              ? { ...capability, reason: 'an externally managed runtime cannot restore the previous profile' }
              : capability),
          })
        }
        if (command === 'switch_response_profile') return null
        if (command === 'get_assistant_settings') return defaultAssistantSettings()
        if (command === 'submit_prompt') {
          submitCalls += 1
          return null
        }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setSelectValue(await getResponseProfileSelect(container), 'quality')
      await Promise.resolve()
    })
    await closeSettings(container)
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Must remain blocked without a local runtime')
      await Promise.resolve()
    })

    expect(container.textContent).toContain('Synthetic native local runtime unavailable')
    expect(getSendButton(container).disabled).toBe(true)
    expect(submitCalls).toBe(0)
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
    })
    await closeSettings(container)
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Must remain blocked after refresh failure')
      getSendButton(container).click()
      await Promise.resolve()
    })

    expect(container.textContent).toContain('Profile state unavailable')
    expect(getSendButton(container).disabled).toBe(true)
    await openSettings(container); await act(async () => { navigateSettings(container, 'models') })
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
    })
    await openSettings(container); await act(async () => { navigateSettings(container, 'models') })
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

  it('keeps a failed active local profile unsendable and retries it explicitly', async () => {
    vi.useFakeTimers()
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
      await vi.advanceTimersByTimeAsync(0)
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
        if (command === 'get_assistant_settings') return defaultAssistantSettings()
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

  it('isolates Settings details behind overview rows and preserves theme radio selection on Back', async () => {
    window.__TAURI_INTERNALS__ = { invoke: async (command) => {
      if (command === 'get_startup_state') return readyStartupState({ voice_input_error: 'Synthetic speech initialization failure' })
      if (command === 'get_assistant_settings') return defaultAssistantSettings()
      return null
    } }
    const { container } = await renderApp()
    await openSettings(container)
    expect(getThemeRadio(container, 'dark').checked).toBe(true)
    expect(getThemeRadio(container, 'light').checked).toBe(false)
    expect(container.querySelector('#assistantInstantSelect')).toBeNull()
    expect(container.querySelector('#tts-toggle')).toBeNull()
    expect(container.querySelector('[aria-label="Increase text size"]')).toBeNull()
    expect(Array.from(container.querySelectorAll('[data-settings-page]')).map((row) => row.getAttribute('data-settings-page'))).toEqual(['text', 'models', 'account', 'voice', 'web', 'updates', 'advanced'])
    await act(async () => { navigateSettings(container, 'models') })
    expect(container.querySelector('#assistantInstantSelect')).not.toBeNull()
    expect(container.querySelector('input[name="uiTheme"]')).toBeNull()
    expect(container.querySelector('#tts-toggle')).toBeNull()
    expect(container.querySelector('[aria-label="Deep model"]')).toBeNull()
    expect(container.querySelector('[aria-label="Review model"]')).toBeNull()
    await act(async () => { getButtonByLabel(container, 'Back to settings').click(); await Promise.resolve() })
    expect(getThemeRadio(container, 'dark').checked).toBe(true)
    await act(async () => { navigateSettings(container, 'voice') })
    const speech = container.querySelector('[aria-labelledby="speech-settings-title"]')!
    const error = Array.from(speech.querySelectorAll('p')).find((paragraph) => paragraph.textContent === 'Synthetic speech initialization failure')!
    expect(error).toBeDefined()
    expect(error.closest('details')).toBeNull()
    const diagnostics = Array.from(speech.querySelectorAll('details')).find((details) => details.querySelector('summary')?.textContent === 'Speech details and diagnostics')!
    expect(diagnostics.open).toBe(false)
    await act(async () => { diagnostics.querySelector('summary')!.click() })
    expect(diagnostics.open).toBe(true)
    await act(async () => { getButtonByLabel(container, 'Back to settings').click(); await Promise.resolve() })
    await act(async () => { navigateSettings(container, 'advanced') })
    const completionLabel = Array.from(container.querySelectorAll('label')).find((label) => label.textContent?.includes('Completion'))!
    expect(completionLabel.querySelector('input[type="checkbox"][role="switch"]')).not.toBeNull()
    expect(container.querySelector('#assistantInstantSelect')).toBeNull()
    expect(container.querySelector('#tts-toggle')).toBeNull()
    await closeSettings(container)
  })

  it('shows the actual Custom capability reason beside disabled direct models', async () => {
    const startup = readyStartupState({ capabilities: completeCapabilities({ custom_provider: 'unavailable' })
      .map((capability) => capability['id'] === 'custom_provider'
        ? { ...capability, reason: 'WSL OpenCode auth file is unavailable' }
        : capability) })
    window.__TAURI_INTERNALS__ = { invoke: async (command) => {
      if (command === 'get_startup_state') return startup
      if (command === 'get_assistant_settings') return defaultAssistantSettings()
      throw new Error(`unexpected command: ${command}`)
    } }
    const { container } = await renderApp()
    await act(async () => { flushSync(() => getButtonByLabel(container, 'Settings').click()); navigateSettings(container, 'account'); await Promise.resolve() })
    expect(container.textContent).toContain('WSL OpenCode auth file is unavailable')
    await act(async () => { getButtonByLabel(container, 'Back to settings').click(); await Promise.resolve() })
    await act(async () => { navigateSettings(container, 'models') })
    const select = container.querySelector<HTMLSelectElement>('select[aria-label="Model"]')!
    expect(select.querySelector<HTMLOptionElement>('option[value="custom-sol-low"]')?.disabled).toBe(true)
    expect(select.querySelector<HTMLOptionElement>('option[value="custom-luna-none"]')?.disabled).toBe(true)
  })

  it('keeps unused Custom failure in Settings, not global chat', async () => {
    const reason = 'Sign in with ChatGPT to restore access'
    const state = () => readyStartupState({ capabilities: completeCapabilities({ custom_provider: 'unavailable' })
      .map((capability) => capability['id'] === 'custom_provider' ? { ...capability, reason } : capability) })
    window.__TAURI_INTERNALS__ = { invoke: async (command) => {
      if (command === 'get_startup_state') return state()
      if (command === 'get_assistant_settings') return defaultAssistantSettings()
      throw new Error(`unexpected command: ${command}`)
    } }
    const { container } = await renderApp()
    expect(container.textContent).not.toContain(`Custom connection: ${reason}`)
    await act(async () => { flushSync(() => getButtonByLabel(container, 'Settings').click()); navigateSettings(container, 'account'); await Promise.resolve() })
    expect(container.querySelector('[role="dialog"]')?.textContent).toContain(reason)
    expect(container.querySelector('[aria-label="Retry Custom"]')).toBeNull()
    await act(async () => { getButtonByLabel(container, 'Back to settings').click(); await Promise.resolve() })
    await act(async () => { navigateSettings(container, 'models') })
    const select = container.querySelector<HTMLSelectElement>('select[aria-label="Model"]')!
    expect(select.querySelector<HTMLOptionElement>('option[value="custom-luna-none"]')?.disabled).toBe(true)
  })

  it('shows preloaded and updated Instant choice to the right of the mic without claiming Deep or Review', async () => {
    let selected = 'custom-luna-none'
    window.__TAURI_INTERNALS__ = { invoke: async (command, args) => {
      if (command === 'get_startup_state') return readyStartupState()
      if (command === 'get_assistant_settings') return { ...defaultAssistantSettings(), instant: selected }
      if (command === 'set_assistant_settings') {
        selected = (args as { settings: { instant: string } }).settings.instant
        return { ...defaultAssistantSettings(), instant: selected }
      }
      throw new Error(`unexpected command: ${command}`)
    } }
    const { container } = await renderApp()
    const indicator = container.querySelector<HTMLElement>('.composer__model-pill')
    const mic = getControlButton(container, 'Start mic')
    expect(indicator?.querySelector<HTMLSelectElement>('select')?.selectedOptions[0]?.textContent).toBe('Custom: GPT-6 Luna No Reasoning')
    expect(indicator!.compareDocumentPosition(mic) & Node.DOCUMENT_POSITION_FOLLOWING).not.toBe(0)
    const select = await getResponseProfileSelect(container)
    expect(select.value).toBe('custom-luna-none')
    expect(select.querySelector('option[value="custom-luna-none"]')).not.toBeNull()
    await act(async () => { setSelectValue(select, 'custom-sol-high'); await Promise.resolve() })
    expect(selected).toBe('custom-sol-high')
    expect(indicator?.querySelector<HTMLSelectElement>('select')?.selectedOptions[0]?.textContent).toBe('Custom: GPT-6.1 Sol High')
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

        if (command === 'get_app_snapshot') return appSnapshot([], { instant: 'local-quality', response_profile: 'quality' })
        expect(command).toBe('submit_prompt')
          expect(args).toMatchObject({ requestId: expect.any(String), prompt: 'Draft release notes' })
        promptEventHandler?.({
          payload: { request_id: args && (args as { requestId: string }).requestId, kind: 'text', text: 'OpenCode response' },
        })

        return savedOutcome((args as { requestId: string }).requestId, 'Draft release notes', 'OpenCode response')
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
        if (command === 'get_app_snapshot') return appSnapshot()
        expect(command).toBe('submit_prompt')
        submittedPrompt = (args as { prompt: string }).prompt
        return savedOutcome((args as { requestId: string }).requestId, submittedPrompt, 'Synthetic answer')
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

  it('rejects a saved receipt with no answer instead of inventing an assistant fallback', async () => {
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

        return { kind: 'completed', save_disposition: 'saved', turn: null, request_id: (args as { requestId: string }).requestId }
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

    expect(container.textContent).not.toContain('OpenCode backend returned no output.')
    expect(container.textContent).toContain('Response failed')
    expect(container.querySelector('.message__thinking')).toBeNull()
    expect(container.querySelectorAll('.message--assistant')).toHaveLength(0)
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
        if (command === 'get_app_snapshot') return appSnapshot()
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
      (button) => button.getAttribute('aria-label') === 'Stop',
    )
    expect(stopButton).toBeDefined()

    await act(async () => {
      promptEventHandler?.({
        payload: { request_id: 'stale-request', kind: 'text', text: 'Wrong' },
      })
      const submitCall = promptEventHandler
      expect(submitCall).toBeDefined()
      expect(container.querySelectorAll('.message--assistant')).toHaveLength(1)
      expect(getConversation(container).textContent).not.toContain('Wrong')
      expect(container.querySelector('.message__thinking')).not.toBeNull()
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
      finishPrompt?.(savedOutcome(activeRequestId, 'Stream this', 'Hello world'))
      await Promise.resolve()
    })
    expect(container.textContent).not.toContain('Executing prompt')
    expect(getResponseAnnouncer(container).textContent).toBe('Assistant response: Hello world')
  })

  it('fences queued and late stream presentation immediately on Stop while native retirement is pending', async () => {
    let send: ((event: { payload: unknown }) => void) | undefined
    let requestId = ''
    let finish: ((value: unknown) => void) | undefined
    let stop: ((value: unknown) => void) | undefined
    const frames: FrameRequestCallback[] = []
    window.requestAnimationFrame = vi.fn((callback) => { frames.push(callback); return frames.length })
    window.cancelAnimationFrame = vi.fn()
    const invoke = vi.fn(async (command: string, args?: unknown) => {
      if (command === 'get_startup_state') return readyStartupState({ prompt_cancellation_available: true, voice_input_available: false })
      if (command === 'get_app_snapshot') return appSnapshot()
      if (command === 'submit_prompt') {
        requestId = (args as { requestId: string }).requestId
        return new Promise((resolve) => { finish = resolve })
      }
      if (command === 'cancel_prompt') return new Promise((resolve) => { stop = resolve })
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke, listen: async (_event, handler) => { send = handler; return () => undefined } }
    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Synthetic cancellable question')
      getSendButton(container).click()
      await Promise.resolve()
      send?.({ payload: { request_id: requestId, kind: 'text', text: 'Synthetic visible prefix' } })
      send?.({ payload: { request_id: requestId, kind: 'text', text: 'Synthetic queued answer' } })
    })
    expect(getConversation(container).textContent).toContain('Synthetic visible prefix')
    expect(frames).toHaveLength(1)
    await act(async () => {
      getButtonByLabel(container, 'Stop').click()
      frames[0]?.(16)
      send?.({ payload: { request_id: requestId, kind: 'text', text: 'Synthetic late answer' } })
    })
    expect(getConversation(container).textContent).not.toContain('Synthetic queued answer')
    expect(getConversation(container).textContent).not.toContain('Synthetic late answer')
    expect(frames).toHaveLength(1)
    expect(getButtonByLabel(container, 'Stopping…').disabled).toBe(true)
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Synthetic second question')
      getSendButton(container).click()
    })
    expect(invoke.mock.calls.filter(([command]) => command === 'submit_prompt')).toHaveLength(1)
    await act(async () => {
      stop?.(true)
      finish?.({ kind: 'cancelled' })
    })
    expect(getConversation(container).textContent).not.toContain('Synthetic queued answer')
  })

  it('admits same-tick Send only once and presents frames without republishing unrelated session slices or resizing the composer', async () => {
    const session = new FrontendSession()
    const preserved = canonicalTurn('31', '32', 'Synthetic preserved question', 'Synthetic preserved answer', 'synthetic-preserved')
    let committed: Record<string, unknown> | null = null
    let requestId = ''
    let send: (value: unknown) => void = () => undefined
    let finish: (value: unknown) => void = () => undefined
    const frames: FrameRequestCallback[] = []
    window.requestAnimationFrame = vi.fn((callback) => { frames.push(callback); return frames.length })
    window.cancelAnimationFrame = vi.fn()
    const native = vi.fn(async (command: string, args?: unknown) => {
      if (command === 'get_app_snapshot') return appSnapshot(committed === null ? [preserved] : [preserved, committed])
      if (command === 'get_startup_state') return readyStartupState({ voice_input_available: false, prompt_cancellation_available: true })
      if (command === 'submit_prompt') {
        requestId = (args as { requestId: string }).requestId
        return new Promise((resolve) => { finish = resolve })
      }
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke: native, createChannel: <T,>(callback: (value: T) => void) => {
      send = callback as (value: unknown) => void
      return {}
    } }
    const { container } = await renderApp(false, session)
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Synthetic current question')
      const form = container.querySelector('form')!
      form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }))
      form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }))
    })
    expect(native.mock.calls.filter(([command]) => command === 'submit_prompt')).toHaveLength(1)
    await act(async () => { getButtonByLabel(container, 'Settings').click() })
    const snapshots = [session.getToolbarSnapshot(), session.getComposerSnapshot(), session.getSettingsSnapshot()]
    const priorAnswer = session.getMessagesSnapshot().find((message) => message.content === 'Synthetic preserved answer')
    expect(priorAnswer).toBeDefined()
    const layout = vi.spyOn(window, 'getComputedStyle')
    const runtimeBindings = vi.spyOn(sessionAuthority(session), 'fieldState')
    const modelRenders = vi.spyOn(instantModelSelectModule, 'InstantModelSelect')
    const oldIntents = session.intents
    let stageMetadata: ReturnType<typeof session.getMessagesSnapshot>[number]['answerStage']
    for (const [index, chunks] of [['Synthetic ', 'frame one'], [' and ', 'frame two']].entries()) {
      await act(async () => {
        for (const text of chunks) send({ kind: 'text', text })
      })
      expect(frames).toHaveLength(index + 1)
      await act(async () => { frames[index]!(16 * (index + 1)) })
      expect(session.getToolbarSnapshot()).toBe(snapshots[0])
      expect(session.getComposerSnapshot()).toBe(snapshots[1])
      expect(session.getSettingsSnapshot()).toBe(snapshots[2])
      expect(session.getMessagesSnapshot().find((message) => message.content === 'Synthetic preserved answer')).toBe(priorAnswer)
      const nextMetadata = session.getMessagesSnapshot().find((message) => message.content.startsWith('Synthetic frame one'))?.answerStage
      if (stageMetadata !== undefined) {
        expect(nextMetadata?.stages).toBe(stageMetadata.stages)
        expect(nextMetadata?.priorVersions).toBe(stageMetadata.priorVersions)
        expect(nextMetadata?.sources).toBe(stageMetadata.sources)
      }
      stageMetadata = nextMetadata
    }
    expect(getConversation(container).textContent).toContain('Synthetic frame one and frame two')
    expect(layout).not.toHaveBeenCalled()
    expect(runtimeBindings).not.toHaveBeenCalled()
    expect(modelRenders).not.toHaveBeenCalled()
    expect(session.intents).toBe(oldIntents)
    await act(async () => { send({ kind: 'reasoning', text: 'Synthetic bounded reasoning progress' }) })
    expect(container.textContent).toContain('Synthetic bounded reasoning progress')
    expect(modelRenders.mock.calls.filter(([props]) => props.id === 'assistantInstantSelect')).toHaveLength(0)
    expect(session.getSettingsSnapshot()).toBe(snapshots[2])
    modelRenders.mockRestore()
    layout.mockRestore()
    await act(async () => {
      committed = canonicalTurn('33', '34', 'Synthetic current question', 'Synthetic canonical answer', requestId)
      finish({ kind: 'completed', save_disposition: 'saved', turn: committed })
    })
    expect(getConversation(container).textContent).toContain('Synthetic canonical answer')
    expect(native.mock.calls.filter(([command]) => command === 'get_app_snapshot')).toHaveLength(2)
    expect(runtimeBindings).toHaveBeenCalled()
    runtimeBindings.mockRestore()
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
        if (command === 'get_app_snapshot') return appSnapshot()
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
    expect(container.querySelectorAll('.message--assistant')).toHaveLength(1)
    expect(getConversation(container).textContent).not.toContain('Frame one')
    expect(container.querySelector('.answer-stage__answer')?.textContent).toBe('Frame ')
    expect(container.querySelector('.message__thinking')).toBeNull()
    await act(async () => { scheduledFrames[0]?.(16); await Promise.resolve() })
    expect(getConversation(container).textContent).toContain('Frame one')
    expect(container.querySelector('.message__thinking')).toBeNull()

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
      finishPrompt?.(savedOutcome(requestId, 'Batch this stream', 'Corrected complete terminal'))
      await Promise.resolve()
    })

    expect(requestFrame).toHaveBeenCalledTimes(4)
    expect(cancelFrame).toHaveBeenCalledWith(41)
    expect(getConversation(container).textContent).toContain('Corrected complete terminal')
  })

  it('shows measured Custom direct request timing with its response, never from another request', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let submissions = 0
    const committed: Record<string, unknown>[] = []
    window.__TAURI_INTERNALS__ = {
      listen: async (_event, handler) => { promptEventHandler = handler; return () => undefined },
      invoke: async (command, args) => {
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'get_assistant_settings') return { ...defaultAssistantSettings(), instant: 'custom-luna-none' }
        if (command === 'get_app_snapshot') return appSnapshot(committed, { instant: 'custom-luna-none' })
        expect(command).toBe('submit_prompt')
        const requestId = (args as { requestId: string }).requestId
        submissions += 1
        promptEventHandler?.({ payload: { request_id: 'unrelated', kind: 'custom_direct_timings', first_text_ms: 1, completed_ms: 2 } })
        promptEventHandler?.({ payload: { request_id: requestId, kind: 'text', text: `Response ${submissions}` } })
        if (submissions === 1) promptEventHandler?.({ payload: { request_id: requestId, kind: 'custom_direct_timings', first_text_ms: 450, completed_ms: 1250 } })
        const receipt = savedOutcome(requestId, (args as { prompt: string }).prompt, `Response ${submissions}`, '1', String(2 * submissions))
        committed.push(receipt['turn'] as Record<string, unknown>)
        return receipt
      },
    }
    const { container } = await renderApp()
    for (const prompt of ['First', 'Second']) {
      await act(async () => setTextAreaValue(getComposer(container), prompt))
      await act(async () => { getSendButton(container).click(); await Promise.resolve() })
    }
    expect(container.textContent).toContain('450 ms')
    expect(container.textContent).not.toContain('Completed: 1.25 s')
    expect(container.textContent).not.toContain('Custom direct request ·')
    expect(container.textContent).not.toContain('First text:')
    const answers = container.querySelectorAll('.message--assistant')
    expect(answers).toHaveLength(2)
    expect(answers[1]?.textContent).toContain('Response 2')
    expect(answers[1]?.parentElement?.querySelector('[data-custom-direct-timings]')).toBeNull()
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
            kind: 'error',
            request_id: args.requestId,
            runtime_phase: 'error',
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
    expect(getButtonByLabel(container, 'Stop')).not.toBeNull()

    await act(async () => {
      window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }))
      await Promise.resolve()
    })
    expect(cancelledRequestId).toBe(activeRequestId)
    expect(getButtonByLabel(container, 'Stopping…').disabled).toBe(true)

    await act(async () => {
      promptEventHandler?.({
        payload: {
          request_id: activeRequestId,
          kind: 'cancelled',
          runtime_phase: 'sleeping',
        },
      })
      finishPrompt?.({ kind: 'cancelled' })
      await Promise.resolve()
    })

    expect(container.querySelectorAll('.message--assistant')).toHaveLength(0)
    expect(container.textContent).not.toContain('Cancel this')
    expect(container.textContent).toContain('Response cancelled')
    expect(container.textContent).not.toContain('Stopping')
  })

  it('stops from the Stop button and returns to sleeping after cancellation settlement', async () => {
    let finishPrompt: ((value: unknown) => void) | undefined
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command) => {
        if (command === 'get_startup_state') return readyStartupState({ prompt_cancellation_available: true })
        if (command === 'get_assistant_settings') return defaultAssistantSettings()
        if (command === 'submit_prompt') {
          return new Promise((resolve) => { finishPrompt = resolve })
        }
        if (command === 'cancel_prompt') return null
        return null
      },
    }
    const { container } = await renderApp()
    await act(async () => { setTextAreaValue(getComposer(container), 'stop me'); getSendButton(container).click(); await Promise.resolve() })
    const stopButton = container.querySelector<HTMLButtonElement>('[aria-label="Stop"]')
    expect(stopButton).toBeDefined()
    await act(async () => { stopButton?.click(); await Promise.resolve() })
    expect(getButtonByLabel(container, 'Stopping…').disabled).toBe(true)
    await act(async () => { finishPrompt?.({ kind: 'cancelled' }); await Promise.resolve() })
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
        if (command === 'get_app_snapshot') return appSnapshot()
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
    const stopButton = container.querySelector<HTMLButtonElement>('[aria-label="Stop"]')
    await act(async () => { stopButton?.click(); await Promise.resolve() })
    await act(async () => { await Promise.resolve(); await Promise.resolve() })
    expect(container.textContent).toContain('Executing prompt')
    await act(async () => { finishPrompt?.(savedOutcome(requestId, 'finish me', 'Synthetic completed answer')); await Promise.resolve() })
    expect(container.textContent).not.toContain('Executing prompt')
    await act(async () => { setTextAreaValue(getComposer(container), 'another prompt'); await Promise.resolve(); await Promise.resolve() })
    expect(getSendButton(container).disabled).toBe(false)
  })

  it('keeps stopped presentation fenced after rejected cancellation but adopts trusted canonical completion', async () => {
    let requestId = ''
    let channel: (value: unknown) => void = () => undefined
    let finish: (value: unknown) => void = () => undefined
    let committed: Record<string, unknown> | null = null
    const frames: FrameRequestCallback[] = []
    window.requestAnimationFrame = vi.fn((callback) => { frames.push(callback); return frames.length })
    window.cancelAnimationFrame = vi.fn()
    window.__TAURI_INTERNALS__ = {
      createChannel: <T,>(handler: (value: T) => void) => { channel = handler as (value: unknown) => void; return {} },
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') return appSnapshot(committed === null ? [] : [committed])
        if (command === 'get_startup_state') return readyStartupState({ voice_input_available: false, prompt_cancellation_available: true })
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          return new Promise((resolve) => { finish = resolve })
        }
        if (command === 'cancel_prompt') throw new Error('Synthetic rejected cancellation')
        return null
      },
    }
    const { container } = await renderApp()
    await act(async () => { setTextAreaValue(getComposer(container), 'Synthetic cancelled presentation'); getSendButton(container).click() })
    await act(async () => {
      channel({ kind: 'text', text: 'Synthetic visible prefix' })
      channel({ kind: 'text', text: 'Synthetic obsolete queued text' })
    })
    expect(frames).toHaveLength(1)
    await act(async () => { getButtonByLabel(container, 'Stop').click() })
    await act(async () => {
      frames[0]!(16)
      channel({ kind: 'text', text: 'Synthetic obsolete Channel text' })
    })
    expect(frames).toHaveLength(1)
    expect(getConversation(container).textContent).not.toContain('Synthetic obsolete')
    expect(getSendButton(container).type).toBe('button')
    expect(getSendButton(container).getAttribute('aria-label')).toBe('Stop')
    await act(async () => {
      committed = canonicalTurn('41', '42', 'Synthetic cancelled presentation', 'Synthetic trusted canonical completion', requestId)
      finish({ kind: 'completed', save_disposition: 'saved', turn: committed })
    })
    expect(getConversation(container).textContent).toContain('Synthetic trusted canonical completion')
    await act(async () => { setTextAreaValue(getComposer(container), 'Synthetic next prompt') })
    expect(getSendButton(container).disabled).toBe(false)
  })

  it('uses the bounded authoritative error message rather than an earlier stream error', async () => {
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
            message: 'Synthetic obsolete stream error',
          },
        })
        return {
          kind: 'error',
          request_id: requestId,
          runtime_phase: 'error',
          error_message: 'Provider failed',
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
    expect(container.textContent).not.toContain('Synthetic obsolete stream error')
  })

  it('polls startup state while the local model is warming and becomes ready later', async () => {
    vi.useFakeTimers()
    let startupCalls = 0

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-quality')
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
             voice_input_available: false,
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

    const session = new FrontendSession()
    const { container } = await renderApp(false, session)

    expect(container.textContent).toContain('Model loading: Loading local Gemma model...')

    await act(async () => {
      await vi.advanceTimersByTimeAsync(600)
    })

    expect(startupCalls).toBe(2)
    expect(session.getControlsSnapshot()).toMatchObject({ runtimeStatus: 'sleeping', assistantSettingsPending: false, snapshotControlsFresh: true, assistantSettings: { instant: 'local-quality' } })
    expect(container.textContent).not.toContain('Model loading:')
    expect(getComposer(container).disabled).toBe(false)
    await act(async () => { setTextAreaValue(getComposer(container), 'Synthetic ready question') })
    expect(getSendButton(container).disabled).toBe(false)
  })

  it('surfaces an error if the model warming state later fails', async () => {
    vi.useFakeTimers()
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
            voice_input_available: false,
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
      await vi.advanceTimersByTimeAsync(600)
    })

    expect(container.textContent).toContain('Startup error: failed to initialize local llama.cpp runtime: boom')
  })

  it('keeps polling ready state until a warming capability becomes available', async () => {
    vi.useFakeTimers()
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
      await vi.advanceTimersByTimeAsync(600)
    })

    expect(startupCalls).toBe(2)
  })

  it('surfaces a capability that fails after ready state was first published', async () => {
    vi.useFakeTimers()
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
      await vi.advanceTimersByTimeAsync(600)
    })

    expect(container.textContent).toContain('completion startup failed')
  })

  it('continues monitoring a capability after it becomes available', async () => {
    vi.useFakeTimers()
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
      await vi.advanceTimersByTimeAsync(1_700)
    })

    expect(startupCalls).toBe(3)
    expect(container.textContent).toContain('completion worker stopped')
  })

  it('keeps an active prompt tracked across settled capability polling', async () => {
    vi.useFakeTimers()
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
        if (command === 'get_app_snapshot') return appSnapshot()
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
      await vi.advanceTimersByTimeAsync(1_100)
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
      finishPrompt?.(savedOutcome(requestId, 'Long-running prompt', 'Original response'))
      await Promise.resolve()
    })

    expect(container.textContent).toContain('Original response')
  })

  it.each(['listening', 'processing'] as const)(
    'does not regress live %s state during capability polling',
    async (runtimePhase) => {
      vi.useFakeTimers()
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
        await vi.advanceTimersByTimeAsync(600)
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
      vi.useFakeTimers()
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
        await vi.advanceTimersByTimeAsync(1_600)
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

  it('does not auto-start microphone capture when wake-word capability is unavailable', async () => {
    nativeObservationEnabled = true
    nativeProjection.voice.auto_start_available = true
    const wakeReason = 'wake word model is unavailable for this test'
    const capabilities = completeCapabilities({ wake_word: 'unavailable' }).map((capability) =>
      capability['id'] === 'wake_word' ? { ...capability, reason: wakeReason } : capability)
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') return appSnapshot([], {}, { capabilities })
        if (command === 'get_startup_state') return readyStartupState({ voice_input_available: true, capabilities })
        throw new Error(`Unexpected capability fixture command: ${command}`)
      },
    }

    const { container } = await renderApp()
    await openSettings(container); await act(async () => { navigateSettings(container, 'voice') })
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
        if (command === 'get_app_snapshot') return appSnapshot([], { prefetch: true, completion: true })
        if (command === 'get_assistant_settings') {
          return {
            instant: 'local-fast', deep: 'opencode-sol-high', review: 'opencode-sol-high',
            deep_enabled: false, review_enabled: false, prefetch: true, completion: true,
          }
        }
        if (command === 'request_completion' || command === 'clear_completion') return null
        if (command === 'submit_prompt') {
          return savedOutcome((args as { requestId: string }).requestId, (args as { prompt: string }).prompt)
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
        if (command === 'get_app_snapshot') return appSnapshot([], { completion: true })
        if (command === 'get_assistant_settings') {
          return {
            instant: 'local-fast', deep: 'opencode-sol-high', review: 'opencode-sol-high',
            deep_enabled: false, review_enabled: false, prefetch: false, completion: true,
          }
        }
        if (command === 'request_completion' || command === 'clear_completion') return null
        if (command === 'submit_prompt') {
          return savedOutcome((args as { requestId: string }).requestId, (args as { prompt: string }).prompt)
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

  it('marks a Deep-only correction as Deep corrected without requiring Review', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let finishPrompt: ((value: unknown) => void) | undefined
    let requestId = ''
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
    expect(container.querySelector('[aria-label="Deep status: corrected"]')).not.toBeNull()
    expect(container.querySelector('[aria-label="Review status: corrected"]')).toBeNull()
    expect(container.querySelector('a[href="https://example.com/deep"]')).not.toBeNull()
    await act(async () => {
      finishPrompt?.({ kind: 'cancelled' })
      await Promise.resolve()
    })
  })
})

describe('Settings-only desktop speech diagnostics', () => {
  it('shows reported providers and static speech failures in Settings, not chat', async () => {
    const capabilities = completeCapabilities().map((item) => item['id'] === 'parakeet'
      ? { ...item, state: 'unavailable', reason: 'Synthetic STT model missing', actual_provider: null }
      : item['id'] === 'tts' ? { ...item, actual_provider: 'cuda' } : item)
    window.__TAURI_INTERNALS__ = { invoke: async (command) => {
      if (command === 'get_app_snapshot') return appSnapshot([], {}, { capabilities })
      if (command === 'get_startup_state') return readyStartupState({ capabilities, voice_input_available: false, voice_input_error: 'Synthetic STT model missing',
        speech_services: {
          stt: { configured_engine: 'Parakeet', runtime_engine: null, configured_model: null, configured_voice: null, configured_locale: null },
          tts: { configured_engine: 'Piper ONNX', runtime_engine: 'Piper ONNX', configured_model: null, configured_voice: null, configured_locale: null },
        },
      })
      return null
    } }
    const { container } = await renderApp()
    expect(container.textContent).not.toContain('Synthetic STT model missing')
    expect(container.textContent).not.toContain('CUDA')
    await act(async () => { flushSync(() => container.querySelector<HTMLButtonElement>('[aria-label="Settings"]')!.click()); navigateSettings(container, 'voice') })
    const dialog = container.querySelector('[role="dialog"]')!
    expect(dialog.textContent).toContain('Synthetic STT model missing')
    expect(dialog.textContent).toContain('Parakeet')
    expect(dialog.textContent).toContain('Local processing')
    expect(dialog.textContent).toContain('CUDA')
    expect(dialog.textContent).toContain('Piper ONNX')
    expect(dialog.textContent).toContain('Model identity not reported')
    expect(dialog.textContent).toContain('Voice identity not reported')
  })
})

async function renderApp(strict = false, session?: FrontendSession): Promise<{ container: HTMLElement }> {
  installDesktopObservationFixture()
  installPromptChannelFixture()
  const container = document.createElement('div')
  document.body.append(container)
  mountedContainers.push(container)
  const root = createRoot(container)
  mountedRoots.push(root)

  await act(async () => {
    root.render(strict ? <StrictMode><App session={session} /></StrictMode> : <App session={session} />)
  })

  return { container }
}

function installDesktopObservationFixture(): void {
  const fixture = window.__TAURI_INTERNALS__
  if (fixture === undefined) return
  const invoke = fixture.invoke
  window.__TAURI_INTERNALS__ = { ...fixture, invoke: async (command, args) => {
    if (obsoleteRendererAudioCommands.has(command)) {
      rendererAudioCalls.push(command)
      throw new Error(`Renderer must not own native audio: ${command}`)
    }
    if (command === 'read_desktop_runtime') {
      if (!nativeObservationEnabled) throw new Error('Synthetic observer unavailable for typed-only fixture')
      return { revision: nativeProjection.revision, snapshot: nativeProjection, events: [] }
    }
    const result = await invoke(command, args)
    if (command === 'get_startup_state' && isRecord(result) && typeof result['runtime_phase'] === 'string') {
      nativeProjection.voice.runtime_phase = result['runtime_phase']
    }
    return result
  }, listen: (event, handler) => event === 'desktop-runtime-changed' ? Promise.resolve(() => undefined) : fixture.listen?.(event, handler) ?? Promise.resolve(() => undefined) }
}

function installPromptChannelFixture(): void {
  const fixture = window.__TAURI_INTERNALS__
  if (fixture === undefined || fixture.createChannel !== undefined) return
  const invoke = fixture.invoke
  const listen = fixture.listen
  window.__TAURI_INTERNALS__ = {
    ...fixture,
    createChannel: <T,>(handler: (message: T) => void) => ({ send: handler }),
    invoke: async (command, args) => {
      if (command === 'submit_prompt') {
        const request = args as { requestId: string; updates: { send: (payload: unknown) => void } }
        expect(request.updates.send).toBeTypeOf('function')
        // Retained event-shaped fixtures feed a command-scoped Channel, not a global subscription.
        await listen?.('prompt-execution-event', ({ payload }) => {
          if (typeof payload !== 'object' || payload === null) { request.updates.send(payload); return }
          const { request_id, ...update } = payload as Record<string, unknown>
          if (request_id === request.requestId) request.updates.send(update)
        })
      }
      return invoke(command, args)
    },
    listen: async (event, handler) => {
      expect(event).not.toBe('prompt-execution-event')
      return listen === undefined ? () => undefined : listen(event, handler)
    },
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
  const button = container.querySelector<HTMLButtonElement>('.composer__utility-row .composer__button')

  if (button === null) {
    throw new Error('Missing send button')
  }

  return button
}

function getThemeRadio(container: HTMLElement, theme: 'dark' | 'light'): HTMLInputElement {
  const radio = container.querySelector<HTMLInputElement>(`input[name="uiTheme"][value="${theme}"]`)
  if (!radio) throw new Error(`Missing ${theme} theme radio`)
  return radio
}

function changeTheme(container: HTMLElement, theme: 'dark' | 'light'): void {
  const wasClosed = container.querySelector('[role="dialog"]') === null
  if (wasClosed) flushSync(() => { getButtonByLabel(container, 'Settings').click() })
  getThemeRadio(container, theme).click()
  if (wasClosed) flushSync(() => { getButtonByLabel(container, 'Close settings').click() })
}

function navigateSettings(container: HTMLElement, page: string): void {
  const row = container.querySelector<HTMLButtonElement>(`[data-settings-page="${page}"]`)
  if (row === null) throw new Error(`Missing Settings row: ${page}`)
  flushSync(() => row.click())
}

function getButtonByLabel(container: HTMLElement, label: string): HTMLButtonElement {
  const button = container.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`)

  if (button === null) {
    throw new Error(`Missing ${label} button`)
  }

  return button
}

function getButtonByText(container: HTMLElement, text: string): HTMLButtonElement {
  const button = Array.from(container.querySelectorAll<HTMLButtonElement>('button'))
    .find((candidate) => candidate.textContent?.trim() === text || candidate.getAttribute('aria-label') === text)
  if (button === undefined) {
    throw new Error(`Missing ${text} button`)
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
  const button = buttons.find((candidate) => candidate.getAttribute('aria-label') === label)

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
  const labels = Array.from(container.querySelectorAll<HTMLLabelElement>('.settings-panel label.ui-switch-field'))
  const autoStopLabel = labels.find((label) => label.textContent?.includes('Stop listening after silence'))
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
  if (container.querySelector('[aria-label="Back to settings"]') !== null) {
    await act(async () => { getButtonByLabel(container, 'Back to settings').click(); await Promise.resolve() })
  }
  await act(async () => { getButtonByLabel(container, 'Close settings').click(); await Promise.resolve() })
  expect(container.querySelector('[role="dialog"]')).toBeNull()
}

async function getResponseProfileSelect(container: HTMLElement): Promise<HTMLSelectElement> {
  await act(async () => {
    getButtonByLabel(container, 'Settings').click()
    await Promise.resolve()
  })
  await act(async () => { navigateSettings(container, 'models') })
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
      command !== 'record_frontend_runtime_diagnostic' &&
      command !== 'get_ui_text_size' &&
      command !== 'set_ui_text_size' &&
      command !== 'get_ui_theme' &&
      command !== 'set_ui_theme' &&
      command !== 'get_app_snapshot' &&
      command !== 'read_desktop_runtime' &&
      command !== 'get_assistant_settings' &&
      command !== 'set_assistant_settings' &&
      command !== 'get_wake_clip_settings' &&
      command !== 'list_wake_clips' &&
      command !== 'set_auto_update_download' &&
      command !== 'get_update_snapshot' &&
      command !== 'check_for_update',
  )
}

function updateSnapshot(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    revision: 2,
    phase: 'up_to_date',
    operation: null,
    current_version: '2026.7.27-1',
    version: '2026.7.28-1',
    notes: null,
    progress_phase: null,
    downloaded_bytes: 0,
    total_bytes: null,
    error: null,
    reason: null,
    auto_download_enabled: true,
    auto_install_enabled: true,
    ...overrides,
  }
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

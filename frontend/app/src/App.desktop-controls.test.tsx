import { act } from 'react'
import { createRoot } from 'react-dom/client'
import type { Root } from 'react-dom/client'
import { afterEach, describe, expect, it, vi } from 'vitest'
import App from './App'
import { FrontendSession } from './FrontendSession'
import { sessionAuthority } from './SessionAuthority'
import { appSnapshot, canonicalTurn, completeCapabilities, readyStartupState } from './test/sessionFixtures'

type Availability = 'warming' | 'available' | 'failed'
let root: Root | null = null
let container: HTMLElement | null = null
afterEach(() => {
  act(() => root?.unmount())
  root = null
  container?.remove()
  container = null
  delete window.__TAURI_INTERNALS__
  window.history.replaceState(null, '')
  vi.useRealTimers()
  vi.restoreAllMocks()
})

function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((complete) => { resolve = complete })
  return { promise, resolve }
}

async function mount(initial: Availability = 'available', session = new FrontendSession()) {
  vi.useFakeTimers()
  let availability = initial
  let sessionId = '1'
  let transcript: Record<string, unknown>[] = []
  let snapshotResponse: (() => Promise<unknown>) | null = null
  let startupResponse: (() => Promise<unknown>) | null = null
  const unexpected: string[] = []
  const audioTransitions: boolean[] = []
  const snapshot = (state = availability) => appSnapshot(transcript, {}, {
    session_id: sessionId, runtime_phase: state === 'warming' ? 'initializing' : 'sleeping',
    capabilities: completeCapabilities({ local_fast: state }),
    model_options: [{ id: 'local-fast', label: 'Synthetic native Fast', available: state === 'available', unavailable_reason: state === 'available' ? null : 'Synthetic native model unavailable' }],
  })
  const startup = (state = availability) => readyStartupState({
    kind: state === 'warming' ? 'warming_model' : 'ready',
    message: 'Synthetic local model warming', voice_input_available: false,
    runtime_phase: state === 'warming' ? 'initializing' : 'sleeping',
    capabilities: completeCapabilities({ local_fast: state }),
  })
  const invoke = vi.fn(async (command: string, args?: unknown): Promise<unknown> => {
    if (command === 'set_native_audio_active') {
      expect(args).toEqual({ active: expect.any(Boolean) })
      audioTransitions.push((args as { active: boolean }).active)
      return null
    }
    if (command === 'get_app_snapshot') return snapshotResponse === null ? snapshot() : snapshotResponse()
    if (command === 'get_startup_state') return startupResponse === null ? startup() : startupResponse()
    if (command === 'submit_prompt') {
      const request = args as { requestId: string; prompt: string }
      const turn = canonicalTurn('1', '2', request.prompt, 'Synthetic admitted answer', request.requestId, 'text', 'instant', sessionId)
      transcript = [turn]
      return { request_id: request.requestId, runtime_phase: 'sleeping', outcome: 'completed', turn }
    }
    if (command === 'reset_session') { sessionId = '3'; transcript = []; return snapshot() }
    if (command === 'get_update_snapshot') return { revision: 1, phase: 'up_to_date', operation: null, current_version: 'synthetic-version', version: null, notes: null, progress_phase: null, downloaded_bytes: 0, total_bytes: null, error: null, reason: null, auto_download_enabled: false, auto_install_enabled: false }
    if (['record_frontend_runtime_diagnostic', 'write_runtime_log', 'check_for_update', 'clear_completion'].includes(command)) return null
    unexpected.push(command)
    throw new Error(`Unexpected desktop controls fixture command: ${command}`)
  })
  window.__TAURI_INTERNALS__ = { invoke, createChannel: <T,>(send: (value: T) => void) => ({ send }) }
  container = document.createElement('div'); document.body.append(container); root = createRoot(container)
  await act(async () => { root!.render(<App session={session} />) })
  return {
    session, invoke, snapshot, startup, unexpected, audioTransitions,
    setAvailability: (next: Availability) => { availability = next },
    setSnapshotResponse: (response: (() => Promise<unknown>) | null) => { snapshotResponse = response },
    setStartupResponse: (response: (() => Promise<unknown>) | null) => { startupResponse = response },
    snapshots: () => invoke.mock.calls.filter(([command]) => command === 'get_app_snapshot').length,
  }
}

async function typeQuestion() {
  await act(async () => {
    const composer = container!.querySelector<HTMLTextAreaElement>('#promptComposer')!
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')!.set!.call(composer, 'Synthetic controls question')
    composer.dispatchEvent(new Event('input', { bubbles: true }))
  })
}
const sendButton = () => container!.querySelector<HTMLButtonElement>('button[type="submit"]')!
const poll = async (milliseconds = 1_000) => { await act(async () => { await vi.advanceTimersByTimeAsync(milliseconds) }) }

describe('desktop change-triggered canonical model controls', () => {
  it('owns native audio only while mounted and foreground, without admitting cues for typed-only controls', async () => {
    const state = await mount()
    expect(state.audioTransitions).toEqual([true])
    try {
      Object.defineProperty(document, 'hidden', { configurable: true, value: true })
      await act(async () => { document.dispatchEvent(new Event('visibilitychange')) })
      Object.defineProperty(document, 'hidden', { configurable: true, value: false })
      await act(async () => { document.dispatchEvent(new Event('visibilitychange')) })
      expect(state.audioTransitions).toEqual([true, false, true])
      await act(async () => { root!.unmount(); root = null })
      expect(state.audioTransitions).toEqual([true, false, true, false])
      expect(state.invoke.mock.calls.filter(([command]) => command === 'reserve_cue_playback_id' || command === 'activate_voice_cue')).toEqual([])
      expect(state.unexpected).toEqual([])
    } finally {
      Reflect.deleteProperty(document, 'hidden')
    }
  })
  it('refreshes a successfully hydrated warming snapshot when startup becomes ready and admits Send', async () => {
    const state = await mount('warming')
    expect(state.snapshots()).toBe(1)
    expect(state.session.getControlsSnapshot().snapshotHydrationState).toBe('succeeded')
    expect(state.session.getModelsSnapshot().instantAssistantOptions[0]?.available).toBe(false)
    state.setAvailability('available')
    await poll(500)
    await typeQuestion()
    expect(sendButton().disabled).toBe(false)
    expect(state.snapshots()).toBe(2)
    expect(container!.querySelector('.shell')?.getAttribute('data-runtime-status')).toBe('sleeping')
    await act(async () => { sendButton().click() })
    expect(state.invoke.mock.calls.filter(([command]) => command === 'submit_prompt')).toHaveLength(1)
    expect(container!.querySelector('.answer-stage__answer')?.textContent).toBe('Synthetic admitted answer')
    expect(state.unexpected).toEqual([])
  })

  it('blocks a previously available native option while failure controls are pending, then recovers without extra steady-state reads', async () => {
    const state = await mount()
    await typeQuestion()
    expect(sendButton().disabled).toBe(false)
    const failed = deferred<unknown>()
    state.setSnapshotResponse(() => failed.promise)
    state.setAvailability('failed')
    await poll()
    expect(sendButton().disabled).toBe(true)
    expect(state.session.getControlsSnapshot().snapshotControlsFresh).toBe(false)
    expect(state.snapshots()).toBe(2)
    await act(async () => { sendButton().click() })
    expect(state.invoke.mock.calls.filter(([command]) => command === 'submit_prompt')).toHaveLength(0)
    await act(async () => { failed.resolve(state.snapshot('failed')) })
    expect(state.session.getModelsSnapshot().instantAssistantOptions[0]?.available).toBe(false)
    await poll(3_000)
    expect(state.snapshots()).toBe(2)
    state.setSnapshotResponse(null)
    state.setAvailability('available')
    await poll()
    expect(state.snapshots()).toBe(3)
    expect(sendButton().disabled).toBe(false)
    expect(state.unexpected).toEqual([])
  })

  it('coalesces changing availability and rejects an older available snapshot before the latest failed generation settles', async () => {
    const state = await mount('warming')
    const available = deferred<unknown>()
    const failed = deferred<unknown>()
    let refreshes = 0
    state.setSnapshotResponse(() => ++refreshes === 1 ? available.promise : failed.promise)
    const olderAvailable = state.snapshot('available')
    state.setAvailability('available')
    await poll(500)
    await typeQuestion()
    state.setAvailability('failed')
    await poll()
    expect(state.snapshots()).toBe(2)
    expect(sendButton().disabled).toBe(true)
    await act(async () => { available.resolve(olderAvailable) })
    expect(state.snapshots()).toBe(3)
    expect(state.session.getControlsSnapshot().snapshotControlsFresh).toBe(false)
    expect(sendButton().disabled).toBe(true)
    await act(async () => { failed.resolve(state.snapshot('failed')) })
    expect(state.session.getModelsSnapshot().instantAssistantOptions[0]?.available).toBe(false)
    state.setSnapshotResponse(null)
    state.setAvailability('available')
    await poll()
    expect(state.snapshots()).toBe(4)
    expect(sendButton().disabled).toBe(false)
    expect(state.unexpected).toEqual([])
  })

  it('keeps controls stale after a failed refresh and recovers only from a later meaningful native change', async () => {
    const state = await mount()
    await typeQuestion()
    state.setAvailability('failed')
    state.setSnapshotResponse(async () => { throw new Error('Synthetic native controls unavailable') })
    await poll()
    expect(state.session.getControlsSnapshot().snapshotControlsFresh).toBe(false)
    expect(sendButton().disabled).toBe(true)
    await poll(3_000)
    expect(state.snapshots()).toBe(2)
    state.setAvailability('available')
    state.setSnapshotResponse(null)
    await poll()
    expect(state.snapshots()).toBe(3)
    expect(sendButton().disabled).toBe(false)
    expect(state.unexpected).toEqual([])
  })

  it('retries stale controls on foreground without polling snapshots for unchanged fresh availability', async () => {
    const state = await mount('warming')
    state.setAvailability('available')
    state.setSnapshotResponse(async () => { throw new Error('Synthetic transient snapshot transport failure') })
    await poll(500)
    await typeQuestion()
    expect(sendButton().disabled).toBe(true)
    expect(state.snapshots()).toBe(2)
    state.setSnapshotResponse(null)
    await act(async () => { window.dispatchEvent(new Event('focus')) })
    expect(state.snapshots()).toBe(3)
    expect(sendButton().disabled).toBe(false)
    await act(async () => {
      window.dispatchEvent(new Event('focus'))
      window.dispatchEvent(new Event('pageshow'))
      document.dispatchEvent(new Event('visibilitychange'))
    })
    await poll(3_000)
    expect(state.snapshots()).toBe(3)
    expect(state.unexpected).toEqual([])
  })

  it('cannot publish an old lifetime snapshot over a remounted authority', async () => {
    const state = await mount('warming')
    const available = deferred<unknown>()
    const olderAvailable = state.snapshot('available')
    state.setAvailability('available')
    state.setSnapshotResponse(() => available.promise)
    await poll(500)
    expect(state.snapshots()).toBe(2)
    await act(async () => { root!.unmount(); root = null })
    state.setAvailability('failed')
    state.setSnapshotResponse(null)
    root = createRoot(container!)
    await act(async () => { root!.render(<App session={state.session} />) })
    expect(state.snapshots()).toBe(3)
    expect(state.session.getControlsSnapshot().snapshotControlsFresh).toBe(true)
    expect(state.session.getModelsSnapshot().instantAssistantOptions[0]?.available).toBe(false)
    await act(async () => { available.resolve(olderAvailable) })
    expect(state.snapshots()).toBe(3)
    expect(state.session.getModelsSnapshot().instantAssistantOptions[0]?.available).toBe(false)
    expect(state.unexpected).toEqual([])
  })

  it('cannot publish a pending availability snapshot over New Chat canonical controls', async () => {
    const state = await mount('warming')
    const available = deferred<unknown>()
    const olderAvailable = state.snapshot('available')
    state.setAvailability('available')
    state.setSnapshotResponse(() => available.promise)
    await poll(500)
    state.setAvailability('failed')
    state.setSnapshotResponse(null)
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="New Chat"]')!.click() })
    expect(state.invoke.mock.calls.filter(([command]) => command === 'reset_session')).toHaveLength(1)
    expect(state.snapshots()).toBe(3)
    expect(state.session.getControlsSnapshot().snapshotControlsFresh).toBe(true)
    expect(state.session.getModelsSnapshot().instantAssistantOptions[0]?.available).toBe(false)
    await act(async () => { available.resolve(olderAvailable) })
    expect(state.snapshots()).toBe(3)
    expect(state.session.getModelsSnapshot().instantAssistantOptions[0]?.available).toBe(false)
    await typeQuestion()
    expect(sendButton().disabled).toBe(true)
    expect(state.unexpected).toEqual([])
  })

  it('does not treat a startup response captured before a newer control publication as a new availability generation', async () => {
    const state = await mount()
    const delayedStartup = deferred<unknown>()
    const oldAvailable = state.startup('available')
    state.setStartupResponse(() => delayedStartup.promise)
    await poll()
    state.setAvailability('failed')
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="New Chat"]')!.click() })
    expect(state.snapshots()).toBe(2)
    await act(async () => { delayedStartup.resolve(oldAvailable) })
    expect(state.snapshots()).toBe(2)
    expect(state.session.getControlsSnapshot().snapshotControlsFresh).toBe(true)
    expect(state.session.getModelsSnapshot().instantAssistantOptions[0]?.available).toBe(false)
    expect(state.unexpected).toEqual([])
  })

  it('does not let initialization settlement overwrite newer independent runtime activity', async () => {
    const state = await mount('warming')
    const available = deferred<unknown>()
    state.setAvailability('available')
    state.setSnapshotResponse(() => available.promise)
    await poll(500)
    // An independent runtime owner publishes while the real App/parser controls read is pending.
    const authority = sessionAuthority(state.session)
    await act(async () => {
      authority.operationRef('runtimeRevision', () => 0).current += 1
      authority.fieldState('runtimeStatus')[1]('listening')
    })
    await act(async () => { available.resolve(state.snapshot('available')) })
    expect(state.session.getModelsSnapshot().instantAssistantOptions[0]?.available).toBe(true)
    expect(container!.querySelector('.shell')?.getAttribute('data-runtime-status')).toBe('listening')
    await typeQuestion()
    expect(sendButton().disabled).toBe(true)
    expect(state.unexpected).toEqual([])
  })

  it('renders unified desktop header icon controls and places wake confidence in composer utility actions', async () => {
    const state = await mount()
    const headerActions = container!.querySelector('.shell__header-actions')!
    expect(headerActions.querySelector('[aria-label="Speak replies"] [data-icon-kind="speaker"]')).not.toBeNull()
    expect(headerActions.querySelector('[aria-label="Copy conversation"] [data-icon-kind="copy"]')).not.toBeNull()
    expect(headerActions.querySelector('[aria-label="New Chat"] [data-icon-kind="plus"]')).not.toBeNull()
    expect(headerActions.querySelector('[aria-label="Settings"] [data-icon-kind="settings"]')).not.toBeNull()

    const authority = sessionAuthority(state.session)
    await act(async () => {
      authority.fieldState('runtimeStatus')[1]('sleeping')
      authority.fieldState('wakeConfidence')[1](0.55)
    })
    const wakeChip = container!.querySelector('.composer__utility-actions .composer__wake--medium')
    expect(wakeChip?.textContent).toBe('wake: 55.00%')
    expect(state.unexpected).toEqual([])
  })
})

import { act } from 'react'
import { createRoot } from 'react-dom/client'
import { afterEach, describe, expect, it, vi } from 'vitest'
import App from './App'
import { appSnapshot, canonicalTurn, completeCapabilities, defaultAssistantSettings, readyStartupState, durableId } from './test/sessionFixtures'
import { FrontendSession } from './FrontendSession'
import { sessionAuthority } from './SessionAuthority'
import type { StartLiveAudioSourceOptions } from './lib/liveAudioSource'
import * as captureModule from './lib/liveAudioSource'
import { loadAudioInputDeviceId } from './lib/audioInputPreference'

afterEach(() => { delete window.__TAURI_INTERNALS__; window.history.replaceState(null, ''); window.localStorage.removeItem('voxgolem.audioInputDeviceId'); vi.restoreAllMocks() })
const nativePrompt = () => ({ request_id: 'synthetic-native-prompt', execution_id: { session_id: '1', generation: '2' }, prompt: 'Synthetic native question',
  updates: [{ kind: 'text', text: 'Synthetic accumulated answer' }] as Record<string, unknown>[], result: null as unknown })
const stoppedVoice = { auto_stop_on_silence: true, auto_start_available: false, capture_id: null, state: 'stopped', runtime_phase: 'sleeping', wake_confidence: null, partial_transcript: null, error: null }

function fixture(options: { native?: boolean; active?: boolean; unavailable?: boolean; holdRead?: Promise<unknown>; voiceAvailable?: boolean; autoStart?: boolean; autoStop?: boolean; captureId?: number; modelAvailable?: boolean; settingsFail?: boolean; snapshotFail?: boolean; resetSnapshotFail?: boolean; resetHold?: Promise<unknown>; stopHold?: Promise<unknown>; startFail?: boolean; fallback?: boolean; updateReady?: boolean; autoStopSave?: (enabled: boolean) => Promise<boolean>; withholdPrompt?: boolean; owner?: 'native_voice' | 'renderer' | null; resetReject?: boolean; cancelReject?: boolean; profileFail?: boolean } = {}) {
  let prompt: ReturnType<typeof nativePrompt> | null = options.native === false ? null : nativePrompt()
  let transcript: Record<string, unknown>[] = []
  let unsaved: Record<string, unknown> | null = null
  let revision = 1
  let active = options.active ?? true
  let speech = { playback_id: null as number | null, playing: false, pending: false }
  let sessionId = '1'
  let durable = false
  const executionId = (sequence: number): string => durable ? durableId(sequence) : String(sequence)
  let reset = false
  let withheld = options.withholdPrompt ?? false
  let profile = 'fast'
  let settings = defaultAssistantSettings()
  let pendingEvents: Record<string, unknown>[] | null = null
  let voice: Record<string, unknown> = { ...stoppedVoice, auto_start_available: options.autoStart ?? false, auto_stop_on_silence: options.autoStop ?? true,
    ...(options.captureId === undefined ? {} : { capture_id: options.captureId, state: 'active' }) }
  let captureSequence = options.captureId ?? 0
  let readHold = options.holdRead
  let typedFinish: ((value: unknown) => void) | undefined
  let typedReject: ((error: Error) => void) | undefined
  let typedUpdate: ((value: unknown) => void) | undefined
  let typedId = ''
  let rendererGeneration = '2'
  let canonicalPhase = 'sleeping'
  const listeners = new Map<string, (event: { payload: unknown }) => void>()
  const invalidate = (): void => { revision += 1; listeners.get('desktop-runtime-changed')?.({ payload: { revision } }) }
  const invoke = vi.fn(async (command: string, rawArgs?: unknown): Promise<unknown> => {
    const args = rawArgs as Record<string, unknown> | undefined
    if (command === 'read_desktop_runtime') {
      if (readHold !== undefined) { const hold = readHold; readHold = undefined; return hold }
      if (pendingEvents !== null) { const events = pendingEvents; pendingEvents = null; return { revision, snapshot: null, events } }
      return { revision, snapshot: { revision, voice, prompt: withheld ? null : prompt, speech }, events: [] }
    }
    if (command === 'get_desktop_request_owner') {
      const expected = prompt?.execution_id ?? { session_id: sessionId, generation: rendererGeneration }
      if (!active || args!['requestId'] !== (prompt?.request_id ?? (typedId || 'synthetic-typed-orphan')) || JSON.stringify(args!['executionId']) !== JSON.stringify(expected)) return null
      return options.owner === undefined ? options.native === false ? 'renderer' : 'native_voice' : options.owner
    }
    if (command === 'get_startup_state') return readyStartupState({ selected_response_profile: profile, voice_input_available: options.voiceAvailable ?? false, prompt_cancellation_available: true })
    if (command === 'get_app_snapshot') { if (options.snapshotFail || (reset && options.resetSnapshotFail)) throw new Error('Synthetic snapshot unavailable'); return appSnapshot(transcript, { ...settings, response_profile: profile }, { session_id: sessionId, runtime_phase: canonicalPhase, unsaved_result: unsaved,
      ...(options.modelAvailable === false ? { capabilities: completeCapabilities({ local_fast: 'unavailable' }), model_options: [{ id: 'local-fast', label: 'Synthetic unavailable model', available: false, unavailable_reason: 'Synthetic unavailable model' }] } : {}),
      ...(active ? { active_request: { request_id: prompt?.request_id ?? (typedId || 'synthetic-typed-orphan'), execution_id: prompt?.execution_id ?? { session_id: sessionId, generation: rendererGeneration }, prompt: prompt?.prompt ?? 'Synthetic native question' } } : {}) }) }
    if (command === 'get_assistant_settings') { if (options.settingsFail) throw new Error('Synthetic settings unavailable'); return settings }
    if (command === 'set_assistant_settings') { settings = args!['settings'] as Record<string, unknown>; return settings }
    if (command === 'switch_response_profile') { if (options.profileFail) throw new Error('Synthetic secret=FAKE_TOKEN https://synthetic.invalid/private prompt=FAKE_PROMPT'); profile = args!['profile'] as string; return { selected_response_profile: profile, supported_response_profiles: ['fast', 'quality'] } }
    if (command === 'reserve_native_microphone_capture_id') { voice = { ...voice, auto_start_available: false }; return ++captureSequence }
    if (command === 'start_native_microphone') {
      if (options.startFail) { voice = { ...voice, state: 'error', capture_id: null, error: 'Synthetic capture failed', auto_start_available: false }; invalidate(); throw new Error('Synthetic capture failed') }
      voice = { ...voice, state: 'active', capture_id: args!['captureId'], auto_start_available: false }; invalidate()
      return { fell_back_to_default: options.fallback ?? false }
    }
    if (command === 'stop_native_microphone') { if (options.stopHold !== undefined) await options.stopHold; voice = { ...voice, state: 'stopped', capture_id: null, auto_start_available: false, partial_transcript: null }; invalidate(); return null }
    if (command === 'set_voice_auto_stop') {
      const enabled = options.autoStopSave === undefined ? args!['enabled'] as boolean : await options.autoStopSave(args!['enabled'] as boolean)
      voice = { ...voice, auto_stop_on_silence: enabled }; invalidate(); return enabled
    }
    if (command === 'reset_session') {
      if (unsaved !== null) throw new Error('Synthetic unresolved output blocks generic reset')
      if (options.resetHold !== undefined) await options.resetHold
      if (options.resetReject) throw new Error('Synthetic reset rejected')
      reset = true
      sessionId = '2'; transcript = []; prompt = null; active = false; unsaved = null
      voice = { ...voice, runtime_phase: 'sleeping', partial_transcript: null, auto_start_available: false }; invalidate()
      return null
    }
    if (command === 'discard_unsaved_and_new_chat') {
      if (unsaved === null || JSON.stringify(args!['executionId']) !== JSON.stringify(unsaved['execution_id'])) throw new Error('Synthetic stale discard target')
      sessionId = executionId(3); transcript = []; prompt = null; active = false; unsaved = null
      invalidate()
      return null
    }
    if (command === 'submit_prompt') {
      typedId = args!['requestId'] as string; active = true
      typedUpdate = (args!['updates'] as { send: (value: unknown) => void }).send
      return new Promise((resolve, reject) => { typedFinish = resolve; typedReject = reject })
    }
    if (command === 'cancel_prompt') {
      if (options.cancelReject) throw new Error('Synthetic cancellation rejected')
      const expected = args!['expectedExecutionId']
      if (expected !== undefined && (args!['requestId'] !== (prompt?.request_id ?? (typedId || 'synthetic-typed-orphan')) || JSON.stringify(expected) !== JSON.stringify(prompt?.execution_id ?? { session_id: sessionId, generation: rendererGeneration }))) return false
      if (options.native === false && prompt === null) active = false
      return true
    }
    if (command === 'get_update_snapshot') return { revision: 1, phase: options.updateReady ? 'ready' : 'up_to_date', operation: null, current_version: 'synthetic', version: options.updateReady ? 'synthetic-next' : null, notes: null, progress_phase: null, downloaded_bytes: 0, total_bytes: null, error: null, reason: null, auto_download_enabled: false, auto_install_enabled: false }
    if (command === 'get_ui_theme') return { theme: 'dark' }
    if (command === 'get_ui_text_size') return { size: 'medium' }
    if (command === 'list_audio_input_devices') return [{ device_id: 'synthetic-input', label: 'Synthetic microphone' }]
    if (command === 'list_wake_clips') return []
    if (command === 'get_wake_clip_settings') return { enabled: false, directory: '/tmp/synthetic-clips' }
    if (command === 'chatgpt_auth') return { phase: 'signed_out' }
    if (command === 'set_tts_enabled') return { enabled: args!['enabled'] }
    if (command === 'record_frontend_runtime_diagnostic' || command === 'check_for_update' || command === 'clear_completion') return null
    throw new Error(`Unexpected synthetic command: ${command}`)
  })
  if (options.unavailable) invoke.mockImplementation(async (command) => {
    if (command === 'read_desktop_runtime') throw new Error('Synthetic observation unavailable')
    if (command === 'get_app_snapshot') return appSnapshot([], {}, { active_request: { request_id: 'synthetic-unknown', execution_id: { session_id: '1', generation: '2' }, prompt: 'Synthetic unknown question' } })
    if (command === 'get_startup_state') return readyStartupState({ voice_input_available: false })
    if (command === 'get_assistant_settings') return defaultAssistantSettings()
    return null
  })
  window.__TAURI_INTERNALS__ = {
    invoke,
    listen: async (name, callback) => { listeners.set(name, callback); return () => { listeners.delete(name) } },
    createChannel: <T,>(send: (value: T) => void) => ({ send }),
  }
  return { invoke, invalidate,
    finishSavedAndNext: () => {
      if (prompt === null) throw new Error('Synthetic prompt missing')
      const turn = { ...canonicalTurn('1', '2', prompt.prompt, 'Synthetic saved A answer', prompt.request_id, 'text', 'instant', sessionId), execution_id: prompt.execution_id }
      transcript = [turn]
      const finished = { revision: ++revision, kind: 'prompt_finished', request_id: prompt.request_id, execution_id: prompt.execution_id, result: { kind: 'completed', save_disposition: 'saved', turn } }
      prompt = { request_id: 'synthetic-native-B', execution_id: { session_id: sessionId, generation: executionId(4) }, prompt: 'Synthetic B question', updates: [{ kind: 'text', text: 'Synthetic accumulated B answer' }], result: null }
      active = true; canonicalPhase = 'executing'
      pendingEvents = [finished, { revision: ++revision, kind: 'prompt_started', prompt }, { revision: ++revision, kind: 'prompt_update', request_id: prompt.request_id, execution_id: prompt.execution_id, update: { kind: 'text', text: 'Synthetic B delta' } }]
      prompt = { ...prompt, updates: [{ kind: 'text', text: 'Synthetic accumulated B answerSynthetic B delta' }] }
      listeners.get('desktop-runtime-changed')?.({ payload: { revision } })
    },
    forgetProcessOutput: () => { unsaved = null; active = false; prompt = null; invalidate() },
    typedUnsaved: () => {
      unsaved = { request_id: typedId, execution_id: { session_id: sessionId, generation: executionId(2) }, prompt: 'Synthetic typed question', answer: { kind: 'text', text: 'Synthetic typed unsaved answer' }, sources: [], final_stage: 'instant', reason: 'storage_error' }
      active = false
      typedFinish?.({ kind: 'unsaved', save_disposition: 'unsaved', unsaved_result: unsaved })
    },
    useDurableContext: () => { durable = true; sessionId = durableId(1); if (prompt !== null) prompt = { ...prompt, execution_id: { session_id: sessionId, generation: durableId(2) } } },
    finishPrepared: (outcome: 'saved' | 'unsaved' | 'cancelled' | 'error') => {
      if (prompt === null) throw new Error('Synthetic prompt missing')
      const execution = prompt.execution_id
      const requestId = prompt.request_id
      let result: Record<string, unknown>
      if (outcome === 'unsaved') {
        unsaved = { request_id: requestId, execution_id: execution, prompt: prompt.prompt, answer: { kind: 'text', text: 'Synthetic prepared unsaved answer' }, sources: [], final_stage: 'instant', reason: 'storage_error' }
        result = { kind: 'unsaved', save_disposition: 'unsaved', unsaved_result: unsaved }
      } else if (outcome === 'saved') {
        const turn = { ...canonicalTurn('3', '4', prompt.prompt, 'Synthetic prepared saved answer', requestId), execution_id: execution }
        transcript = [turn]; result = { kind: 'completed', save_disposition: 'saved', turn }
      } else result = outcome === 'error' ? { kind: 'error', request_id: requestId, runtime_phase: 'sleeping', error_message: 'Synthetic prepared provider failure' } : { kind: 'cancelled' }
      active = false
      prompt = { ...prompt, result }
      pendingEvents = [
        { revision: ++revision, kind: 'prompt_update', request_id: requestId, execution_id: execution, update: { kind: 'text', text: 'Synthetic pending delta' } },
        { revision: ++revision, kind: 'prompt_finished', request_id: requestId, execution_id: execution, result },
      ]
      listeners.get('desktop-runtime-changed')?.({ payload: { revision } })
    },
    emptyContext: () => { sessionId = '0'; prompt = null; active = false },
    startPreparedNative: (mode: 'snapshot' | 'event') => {
      sessionId = executionId(1); prompt = { ...nativePrompt(), execution_id: { session_id: sessionId, generation: executionId(2) } }; active = true
      if (mode === 'snapshot') invalidate()
      else {
        pendingEvents = [{ revision: ++revision, kind: 'prompt_started', prompt }]
        listeners.get('desktop-runtime-changed')?.({ payload: { revision } })
      }
    },
    reuseNativePrompt: () => { active = true; prompt = { ...nativePrompt(), execution_id: { session_id: sessionId, generation: executionId(4) }, prompt: 'Synthetic reused native question', updates: [{ kind: 'text', text: 'Synthetic current native answer' }] }; invalidate() },
    delayedEvents: (events: Record<string, unknown>[]) => {
      pendingEvents = events.map((event) => ({ ...event, revision: ++revision }))
      listeners.get('desktop-runtime-changed')?.({ payload: { revision } })
    },
    finishUnsaved: (mode: 'snapshot' | 'event' | 'retired') => {
      if (prompt === null) throw new Error('Synthetic native prompt is missing')
      unsaved = { request_id: prompt.request_id, execution_id: prompt.execution_id, prompt: prompt.prompt, answer: { kind: 'text', text: 'Synthetic unsaved native answer' }, sources: [{ url: 'https://example.test/native-source', title: 'Synthetic native source' }], final_stage: 'deep', reason: 'storage_error' }
      const result = { kind: 'unsaved', save_disposition: 'unsaved', unsaved_result: unsaved }
      active = false; canonicalPhase = 'sleeping'
      if (mode === 'snapshot') prompt = { ...prompt, result }
      else {
        if (mode === 'event') pendingEvents = [{ revision: ++revision, kind: 'prompt_finished', request_id: prompt.request_id, execution_id: prompt.execution_id, result }]
        prompt = null
      }
      if (mode === 'event') listeners.get('desktop-runtime-changed')?.({ payload: { revision } })
      else invalidate()
    },
    reuseOrphanRequestId: () => { typedId = 'synthetic-typed-orphan' },
    finishSuccessor: () => {
      if (prompt === null) throw new Error('Synthetic native successor is missing')
      const turn = canonicalTurn('3', '4', prompt.prompt, 'Synthetic successor committed answer', prompt.request_id)
      transcript = [...transcript, turn]; active = false; canonicalPhase = 'sleeping'
      prompt = { ...prompt, result: { kind: 'completed', save_disposition: 'saved', turn } }
      voice = { ...voice, runtime_phase: 'sleeping' }; invalidate()
    },
    retireOrphan: (outcome: 'completed' | 'cancelled', successor: 'none' | 'renderer' | 'native' = 'none') => {
      transcript = outcome === 'completed' ? [canonicalTurn('1', '2', 'Synthetic native question', 'Synthetic orphan committed answer', 'synthetic-typed-orphan')] : []
      active = successor !== 'none'
      typedId = successor === 'renderer' ? 'synthetic-independent-renderer' : ''
      rendererGeneration = successor === 'renderer' ? '4' : '2'
      prompt = successor === 'native' ? { request_id: 'synthetic-native-next', execution_id: { session_id: '1', generation: '4' }, prompt: 'Synthetic next native question', updates: [{ kind: 'text', text: 'Synthetic next native answer' }], result: null } : null
      canonicalPhase = active ? 'executing' : 'sleeping'
      voice = { ...voice, runtime_phase: canonicalPhase }
    },
    seedHistory: () => { transcript = [{ ...canonicalTurn('19', '20', 'Synthetic retained question', 'Synthetic retained answer', 'synthetic-retained', 'text', 'instant', sessionId), execution_id: { session_id: sessionId, generation: executionId(20) } }] },
    reveal: () => { withheld = false; invalidate() },
    holdRead: (hold: Promise<unknown>) => { readHold = hold },
    root: () => structuredClone({ revision, snapshot: { revision, voice, prompt, speech }, events: [] }),
    voice: (patch: Record<string, unknown>) => { voice = { ...voice, ...patch } },
    event: (payload: unknown) => listeners.get('completion-event')?.({ payload }),
    replace: (text: string) => { prompt = { ...(prompt ?? nativePrompt()), updates: [{ kind: 'text', text }] } },
    updates: (updates: Record<string, unknown>[]) => { prompt = { ...(prompt ?? nativePrompt()), updates } },
    partial: () => { voice = { ...stoppedVoice, capture_id: 12, state: 'active', runtime_phase: 'listening', partial_transcript: { session_id: 73, revision: 9, text: 'Synthetic partial text' } } },
    speech: (playing: boolean, pending: boolean) => { speech = { playback_id: playing ? 21 : null, playing, pending } },
    finish: (refused = false, sources: { url: string; title: string }[] = [], stage: 'instant' | 'deep' | 'review' = 'instant') => {
      const turn = { ...canonicalTurn('1', '2', 'Synthetic native question', 'Synthetic canonical answer', 'synthetic-native-prompt', refused ? 'refusal' : 'text', stage, sessionId), execution_id: { session_id: sessionId, generation: executionId(2) }, sources }
      transcript = [turn]; active = false
      prompt = { ...(prompt ?? nativePrompt()), result: { kind: refused ? 'refused' : 'completed', save_disposition: 'saved', turn } }
    },
    retire: () => { prompt = null },
    next: () => { active = true; prompt = { request_id: 'synthetic-native-next', execution_id: { session_id: '1', generation: '4' }, prompt: 'Synthetic next native question', updates: [{ kind: 'text', text: 'Synthetic next native answer' }], result: null } },
    terminalAndNext: (outcome: 'cancelled' | 'error') => {
      const result = outcome === 'cancelled' ? { kind: 'cancelled' } : { kind: 'error', request_id: 'synthetic-native-prompt', runtime_phase: 'sleeping', error_message: 'Synthetic native failure' }
      pendingEvents = [{ revision: ++revision, kind: 'prompt_finished', request_id: 'synthetic-native-prompt', execution_id: { session_id: '1', generation: '2' }, result }]
      active = true
      prompt = { request_id: 'synthetic-native-next', execution_id: { session_id: '1', generation: '4' }, prompt: 'Synthetic next native question', updates: [{ kind: 'text', text: 'Synthetic next native answer' }], result: null }
      pendingEvents.push({ revision: ++revision, kind: 'prompt_started', prompt })
      listeners.get('desktop-runtime-changed')?.({ payload: { revision } })
    },
    typedUpdate: (text: string) => typedUpdate?.({ kind: 'text', text }),
    typedFinish: (outcome: 'completed' | 'cancelled' | 'error' | 'rejected' = 'completed') => {
      const turn = canonicalTurn('1', '2', 'Synthetic typed question', 'Synthetic typed answer', typedId)
      transcript = outcome === 'completed' ? [turn] : []; active = false
      if (outcome === 'rejected') typedReject?.(new Error('Synthetic transport rejection'))
      else if (outcome === 'error') typedReject?.(new Error('Synthetic execution error'))
      else typedFinish?.(outcome === 'completed' ? { kind: 'completed', save_disposition: 'saved', turn } : { kind: 'cancelled' })
    },
  }
}
async function mount(session = new FrontendSession({ audioInputDeviceId: loadAudioInputDeviceId() })) {
  const container = document.createElement('div'); document.body.append(container)
  const root = createRoot(container)
  await act(async () => { root.render(<App session={session} />) })
  return { container, session, unmount: async () => { await act(async () => { root.unmount() }); container.remove() } }
}
async function sendTyped(container: HTMLElement): Promise<void> {
  await act(async () => {
    const composer = container.querySelector<HTMLTextAreaElement>('#promptComposer')!
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')!.set!.call(composer, 'Synthetic typed question')
    composer.dispatchEvent(new Event('input', { bubbles: true }))
  })
  await act(async () => { container.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
}
function button(container: HTMLElement, label: string): HTMLButtonElement {
  const found = container.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`) ?? Array.from(container.querySelectorAll('button')).find((item) => item.textContent === label)
  if (found === undefined || found === null) throw new Error(`Missing synthetic control ${label}`)
  return found
}
async function click(container: HTMLElement, label: string): Promise<void> { await act(async () => { button(container, label).click() }) }
function autoStopToggle(container: HTMLElement): HTMLInputElement {
  const label = Array.from(container.querySelectorAll('label')).find((item) => item.textContent?.includes('Auto Stop'))
  if (label?.querySelector('input') == null) throw new Error('Missing Auto Stop control')
  return label.querySelector('input')!
}
function deferred<T>() { let resolve!: (value: T) => void; let reject!: (error: Error) => void; const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no }); return { promise, resolve, reject } }

describe('autonomous desktop runtime presentation', () => {
  it.each(['active', 'terminal'] as const)('retires first-admission read ownership on same-session remount with a matching %s execution', async (mode) => {
    const native = fixture({ native: false, active: false })
    native.useDurableContext(); native.emptyContext()
    const session = new FrontendSession()
    let app = await mount(session)
    const original = native.invoke.getMockImplementation()!
    const gates: ReturnType<typeof deferred<void>>[] = []
    native.invoke.mockImplementation(async (command, args) => {
      const value = await original(command, args)
      if (command === 'get_app_snapshot' && gates.length < 2) { const gate = deferred<void>(); gates.push(gate); await gate.promise }
      return value
    })
    try {
      await act(async () => { native.startPreparedNative('event') })
      await vi.waitFor(() => expect(gates).toHaveLength(1))
      await app.unmount()
      if (mode === 'terminal') native.finishUnsaved('snapshot')
      app = await mount(session)
      await vi.waitFor(() => expect(gates).toHaveLength(2))
      await act(async () => { gates[0]!.resolve() })
      expect(sessionAuthority(session).selection.canonicalSession.current).toBe('0')
      const reads = native.invoke.mock.calls.filter(([command]) => command === 'get_app_snapshot').length
      await act(async () => { native.delayedEvents([{ kind: 'prompt_started', prompt: native.root().snapshot.prompt }]) })
      expect(native.invoke.mock.calls.filter(([command]) => command === 'get_app_snapshot')).toHaveLength(reads)
      await act(async () => { gates[1]!.resolve() })
      if (mode === 'active') {
        await vi.waitFor(() => expect(app.container.textContent).toContain('Synthetic accumulated answer'))
        await act(async () => { native.finishPrepared('error') })
        await vi.waitFor(() => expect(app.container.textContent).toContain('Synthetic prepared provider failure'))
      } else await vi.waitFor(() => expect(app.container.querySelector('[aria-label="Unsaved answer"]')).not.toBeNull())
      expect(session.getControlsSnapshot().promptState).toBe('idle')
      expect(sessionAuthority(session).selection.canonicalSession.current).toBe(durableId(1))
    } finally { gates.forEach((gate) => gate.resolve()); await app.unmount() }
  })

  it.each(['unsaved', 'error'] as const)('reattaches B from the existing observer after a synchronous saved-A/start-B/update-B batch, then consumes B %s incrementally', async (outcome) => {
    const native = fixture()
    native.useDurableContext()
    const app = await mount()
    const original = native.invoke.getMockImplementation()!
    const release = deferred<void>()
    let captured = false
    native.invoke.mockImplementation(async (command, args) => {
      const value = await original(command, args)
      if (command === 'get_app_snapshot' && !captured) { captured = true; await release.promise }
      return value
    })
    try {
      await act(async () => { native.finishSavedAndNext() })
      await vi.waitFor(() => expect(captured).toBe(true))
      await act(async () => { release.resolve() })
      await vi.waitFor(() => expect(app.container.textContent).toContain('Synthetic accumulated B answer'))
      expect(app.container.textContent?.match(/Synthetic B delta/g)).toHaveLength(1)
      expect(app.session.getControlsSnapshot().promptState).toBe('executing')
      expect(app.container.textContent).toContain('Synthetic saved A answer')
      await act(async () => { native.finishPrepared(outcome) })
      await vi.waitFor(() => expect(app.session.getControlsSnapshot().promptState).toBe('idle'))
      if (outcome === 'unsaved') expect(app.container.querySelector('[aria-label="Unsaved answer"]')?.textContent).toContain('Synthetic prepared unsaved answer')
      else expect(app.container.textContent).toContain('Synthetic prepared provider failure')
      expect(app.container.textContent).toContain('Synthetic saved A answer')
    } finally { release.resolve(); await app.unmount() }
  })
  it.each(['cancelled', 'error'] as const)('validates the successor context afresh when its %s receipt arrives before the coalesced admission read can start', async (outcome) => {
    const native = fixture({ native: false, active: false })
    native.useDurableContext(); native.emptyContext()
    const app = await mount()
    const original = native.invoke.getMockImplementation()!
    const release = deferred<void>()
    let reads = 0
    native.invoke.mockImplementation(async (command, args) => {
      const value = await original(command, args)
      if (command === 'get_app_snapshot' && ++reads === 1) await release.promise
      return value
    })
    try {
      await act(async () => { native.startPreparedNative('event') })
      await vi.waitFor(() => expect(reads).toBe(1))
      await act(async () => { native.reuseNativePrompt() })
      await act(async () => { native.finishPrepared(outcome) })
      await act(async () => { release.resolve() })
      await vi.waitFor(() => expect(app.container.textContent).toContain(outcome === 'error' ? 'Synthetic prepared provider failure' : 'Response cancelled'))
      expect(app.session.getControlsSnapshot().promptState).toBe('idle')
      expect(sessionAuthority(app.session).selection.canonicalSession.current).toBe(durableId(1))
    } finally { await app.unmount() }
  })
  it('accepts a genuinely fresh canonical null after native process-owned output is no longer available', async () => {
    const native = fixture({ native: false, active: false })
    native.useDurableContext()
    const app = await mount()
    try {
      await sendTyped(app.container)
      await act(async () => { native.typedUnsaved() })
      expect(app.container.querySelector('[aria-label="Unsaved answer"]')).not.toBeNull()
      await act(async () => { native.forgetProcessOutput() })
      await vi.waitFor(() => expect(app.container.querySelector('[aria-label="Unsaved answer"]')).toBeNull())
      expect(native.invoke.mock.calls.some(([command]) => command === 'discard_unsaved_and_new_chat')).toBe(false)
    } finally { await app.unmount() }
  })
  it('fences delayed canonical null and non-null recovery reads across unsaved settlement and explicit discard', async () => {
    const native = fixture({ native: false, active: false })
    native.useDurableContext()
    const app = await mount()
    const original = native.invoke.getMockImplementation()!
    let gate: ReturnType<typeof deferred<void>> | null = null
    let captured = false
    native.invoke.mockImplementation(async (command, args) => {
      const value = await original(command, args)
      if (command === 'get_app_snapshot' && gate !== null && !captured) { captured = true; await gate.promise }
      return value
    })
    try {
      gate = deferred<void>()
      await act(async () => { native.invalidate() })
      await vi.waitFor(() => expect(captured).toBe(true))
      await sendTyped(app.container)
      await act(async () => { native.typedUnsaved() })
      expect(app.container.querySelector('[aria-label="Unsaved answer"]')).not.toBeNull()
      await act(async () => { gate!.resolve() })
      expect(app.container.querySelector('[aria-label="Unsaved answer"]')).not.toBeNull()
      gate = deferred<void>(); captured = false
      await act(async () => { native.invalidate() })
      await vi.waitFor(() => expect(captured).toBe(true))
      await click(app.container, 'New Chat')
      expect(app.container.querySelector('[aria-label="Unsaved answer"]')).toBeNull()
      await act(async () => { gate!.resolve() })
      expect(app.container.querySelector('[aria-label="Unsaved answer"]')).toBeNull()
      expect(sessionAuthority(app.session).selection.canonicalSession.current).toBe(durableId(3))
    } finally { await app.unmount() }
  })
  it('requires a separate admission validation for a successor received while the first validation is pending', async () => {
    const native = fixture({ native: false, active: false })
    native.useDurableContext(); native.emptyContext()
    const app = await mount()
    const original = native.invoke.getMockImplementation()!
    const reads: ReturnType<typeof deferred<void>>[] = []
    native.invoke.mockImplementation(async (command, args) => {
      const value = await original(command, args)
      if (command === 'get_app_snapshot' && reads.length < 2) { const gate = deferred<void>(); reads.push(gate); await gate.promise }
      return value
    })
    try {
      await act(async () => { native.startPreparedNative('event') })
      await vi.waitFor(() => expect(reads).toHaveLength(1))
      await act(async () => { native.reuseNativePrompt() })
      await act(async () => {
        native.replace('Synthetic newest successor answer')
        native.delayedEvents([{ kind: 'prompt_update', request_id: 'synthetic-native-prompt', execution_id: { session_id: durableId(1), generation: durableId(4) }, update: { kind: 'text', text: 'Synthetic pending successor delta' } }])
      })
      expect(reads).toHaveLength(1)
      await act(async () => { reads[0]!.resolve() })
      await vi.waitFor(() => expect(reads).toHaveLength(2))
      expect(app.container.textContent).not.toContain('Synthetic current native answer')
      expect(sessionAuthority(app.session).selection.canonicalSession.current).toBe('0')
      await act(async () => { reads[1]!.resolve() })
      await vi.waitFor(() => expect(app.container.textContent).toContain('Synthetic newest successor answer'))
      await act(async () => { native.finishUnsaved('event') })
      await vi.waitFor(() => expect(app.session.getControlsSnapshot().promptState).toBe('idle'))
    } finally { await app.unmount() }
  })

  it('uses a fresh bounded native root instead of accumulating deltas during delayed first-admission validation', async () => {
    const native = fixture({ native: false, active: false })
    native.useDurableContext(); native.emptyContext()
    const app = await mount()
    const original = native.invoke.getMockImplementation()!
    const release = deferred<void>()
    let captured = false
    native.invoke.mockImplementation(async (command, args) => {
      const value = await original(command, args)
      if (command === 'get_app_snapshot' && !captured) { captured = true; await release.promise }
      return value
    })
    try {
      await act(async () => { native.startPreparedNative('event') })
      await vi.waitFor(() => expect(captured).toBe(true))
      await act(async () => {
        native.replace('Synthetic latest accumulated answer')
        native.delayedEvents([{ kind: 'prompt_update', request_id: 'synthetic-native-prompt', execution_id: { session_id: durableId(1), generation: durableId(2) }, update: { kind: 'text', text: 'Synthetic intervening delta' } }])
      })
      await act(async () => { release.resolve() })
      await vi.waitFor(() => expect(app.container.textContent).toContain('Synthetic latest accumulated answer'))
      expect(app.container.textContent).not.toContain('Synthetic intervening delta')
      expect(app.session.getControlsSnapshot().promptState).toBe('executing')
    } finally { await app.unmount() }
  })
  it.each(['saved', 'unsaved', 'cancelled', 'error'] as const)('retains matching %s terminal events while first-admission validation is already captured and delayed', async (outcome) => {
    const native = fixture({ native: false, active: false })
    native.useDurableContext()
    native.emptyContext()
    const app = await mount()
    const original = native.invoke.getMockImplementation()!
    const release = deferred<void>()
    let captured = false
    native.invoke.mockImplementation(async (command, args) => {
      const value = await original(command, args)
      if (command === 'get_app_snapshot' && !captured) { captured = true; await release.promise }
      return value
    })
    try {
      await act(async () => { native.startPreparedNative('event') })
      await vi.waitFor(() => expect(captured).toBe(true))
      await act(async () => { native.finishPrepared(outcome) })
      await act(async () => { release.resolve() })
      await vi.waitFor(() => expect(app.session.getControlsSnapshot().promptState).toBe('idle'))
      if (outcome === 'saved') expect(app.container.textContent).toContain('Synthetic prepared saved answer')
      if (outcome === 'unsaved') expect(app.container.querySelector('[aria-label="Unsaved answer"]')?.textContent).toContain('Synthetic prepared unsaved answer')
      if (outcome === 'error') expect(app.container.textContent).toContain('Synthetic prepared provider failure')
      if (outcome === 'cancelled') expect(app.container.textContent).toContain('Response cancelled')
      expect(app.container.textContent).not.toContain('Synthetic pending delta')
    } finally { await app.unmount() }
  })
  it.each(['snapshot', 'event'] as const)('attaches a first native durable preparation from an empty selection via %s', async (mode) => {
    const native = fixture({ native: false, active: false })
    native.useDurableContext()
    native.emptyContext()
    const app = await mount()
    try {
      await act(async () => { native.startPreparedNative(mode) })
      await vi.waitFor(() => expect(app.container.textContent).toContain('Synthetic accumulated answer'))
      expect(sessionAuthority(app.session).selection.canonicalSession.current).toBe(durableId(1))
      expect(native.invoke.mock.calls.some(([command]) => command === 'submit_prompt')).toBe(false)
      await act(async () => { native.finishUnsaved('event') })
      await vi.waitFor(() => expect(app.container.querySelector('[aria-label="Unsaved answer"]')).not.toBeNull())
      expect(sessionAuthority(app.session).selection.transcript.current).toHaveLength(0)
    } finally { await app.unmount() }
  })
  it.each(['snapshot', 'event', 'retired'] as const)('presents native voice save failure via %s separately from saved history and blocks follow-up', async (mode) => {
    const native = fixture()
    native.useDurableContext()
    native.seedHistory()
    const app = await mount()
    try {
      await act(async () => { native.finishUnsaved(mode) })
      await vi.waitFor(() => expect(app.container.querySelector('[aria-label="Unsaved answer"]')?.textContent).toContain('Synthetic unsaved native answer'))
      expect(app.container.querySelectorAll('[aria-label="Unsaved answer"]')).toHaveLength(1)
      expect(app.container.textContent).not.toContain('Synthetic accumulated answer')
      expect(app.container.textContent).toContain('Synthetic retained answer')
      expect(app.container.querySelector('[aria-label="Answer not saved"]')).not.toBeNull()
      expect(sessionAuthority(app.session).selection.transcript.current).toHaveLength(1)
      expect(sessionAuthority(app.session).selection.transcript.current[0]?.requestId).toBe('synthetic-retained')
      await sendTyped(app.container)
      expect(native.invoke.mock.calls.some(([command]) => command === 'submit_prompt')).toBe(false)
    } finally { await app.unmount() }
    const reloaded = await mount()
    try {
      expect(reloaded.container.querySelectorAll('[aria-label="Unsaved answer"]')).toHaveLength(1)
      expect(reloaded.container.querySelectorAll('.message--user')).toHaveLength(2)
      expect(sessionAuthority(reloaded.session).selection.transcript.current).toHaveLength(1)
    } finally { await reloaded.unmount() }
  })

  it('ignores delayed native update and finish envelopes for a reused correlation with a different execution', async () => {
    const native = fixture()
    native.useDurableContext()
    const app = await mount()
    try {
      await act(async () => { native.finish(); native.invalidate() })
      await vi.waitFor(() => expect(app.container.textContent).toContain('Synthetic canonical answer'))
      await act(async () => { native.reuseNativePrompt() })
      await vi.waitFor(() => expect(app.container.textContent).toContain('Synthetic current native answer'))
      await act(async () => { native.delayedEvents([
        { kind: 'prompt_update', request_id: 'synthetic-native-prompt', execution_id: { session_id: durableId(1), generation: durableId(2) }, update: { kind: 'text', text: 'Synthetic stale native text' } },
        { kind: 'prompt_finished', request_id: 'synthetic-native-prompt', execution_id: { session_id: durableId(1), generation: durableId(2) }, result: { kind: 'cancelled' } },
      ]) })
      expect(app.container.textContent).not.toContain('Synthetic stale native text')
      expect(app.container.textContent).toContain('Synthetic current native answer')
      expect(button(app.container, 'Stop')).toBeDefined()
      await act(async () => { native.finishUnsaved('event') })
      await vi.waitFor(() => expect(app.container.querySelector('[aria-label="Unsaved answer"]')?.textContent).toContain('Synthetic unsaved native answer'))
      expect(app.container.textContent).toContain('Synthetic canonical answer')
      expect(sessionAuthority(app.session).selection.transcript.current).toHaveLength(1)
    } finally { await app.unmount() }
  })
  it('null-owner retirement race: recognizes a successor with the same request ID but a different execution', async () => {
    const native = fixture({ native: false, active: true })
    const original = native.invoke.getMockImplementation()!
    const ownership = deferred<unknown>()
    native.invoke.mockImplementation((command, args) => command === 'get_desktop_request_owner' ? ownership.promise : original(command, args))
    const app = await mount()
    try {
      native.retireOrphan('completed', 'renderer'); native.reuseOrphanRequestId()
      await act(async () => { ownership.resolve(null) })
      expect(app.session.getControlsSnapshot().promptState).toBe('idle')
      expect(app.session.getControlsSnapshot().runtimeStatus).toBe('executing')
      expect(app.session.getMessagesSnapshot().filter((row) => row.role === 'assistant' && row.content === 'Synthetic orphan committed answer')).toHaveLength(1)
      expect(native.invoke.mock.calls.filter(([command]) => command === 'get_desktop_request_owner')).toHaveLength(1)
      expect(native.invoke.mock.calls.filter(([command]) => command === 'cancel_prompt' || command === 'submit_prompt')).toEqual([])
    } finally { await app.unmount() }
  })
  it.each(['reset', 'unmount'] as const)('null-owner retirement race: rejects a late canonical retirement read after %s', async (boundary) => {
    const native = fixture({ native: false, active: true })
    const original = native.invoke.getMockImplementation()!
    const ownership = deferred<unknown>()
    const canonical = deferred<unknown>()
    let holdCanonical = false
    native.invoke.mockImplementation((command, args) => {
      if (command === 'get_desktop_request_owner') return ownership.promise
      if (command === 'get_app_snapshot' && holdCanonical) { holdCanonical = false; return canonical.promise }
      return original(command, args)
    })
    const app = await mount()
    let mounted = true
    try {
      native.retireOrphan('completed')
      const retired = await original('get_app_snapshot')
      holdCanonical = true
      await act(async () => { ownership.resolve(null) })
      expect(holdCanonical).toBe(false)
      if (boundary === 'reset') await click(app.container, 'New Chat')
      else { await app.unmount(); mounted = false }
      await act(async () => { canonical.resolve(retired) })
      expect(app.session.getMessagesSnapshot()).toEqual([])
      if (boundary === 'reset') expect(app.session.getControlsSnapshot().promptState).toBe('idle')
      expect(native.invoke.mock.calls.filter(([command]) => command === 'cancel_prompt' || command === 'submit_prompt')).toEqual([])
    } finally { if (mounted) await app.unmount() }
  })
  it('null-owner retirement race: an older retirement read cannot replace a newer canonical successor settlement', async () => {
    const native = fixture({ native: false, active: true })
    const original = native.invoke.getMockImplementation()!
    const ownership = deferred<unknown>()
    const canonical = deferred<unknown>()
    let holdCanonical = false
    native.invoke.mockImplementation((command, args) => {
      if (command === 'get_desktop_request_owner') return ownership.promise
      if (command === 'get_app_snapshot' && holdCanonical) { holdCanonical = false; return canonical.promise }
      return original(command, args)
    })
    const app = await mount()
    try {
      native.retireOrphan('completed', 'native')
      const retired = await original('get_app_snapshot')
      holdCanonical = true
      await act(async () => { ownership.resolve(null) })
      await act(async () => { native.invalidate() })
      await act(async () => { native.finishSuccessor() })
      expect(app.container.textContent).toContain('Synthetic successor committed answer')
      await act(async () => { canonical.resolve(retired) })
      const messages = app.session.getMessagesSnapshot()
      expect(messages.filter((row) => row.role === 'assistant' && row.content === 'Synthetic orphan committed answer')).toHaveLength(1)
      expect(messages.filter((row) => row.role === 'assistant' && row.content === 'Synthetic successor committed answer')).toHaveLength(1)
      expect(app.session.getControlsSnapshot().promptState).toBe('idle')
      expect(app.session.getControlsSnapshot().runtimeStatus).toBe('sleeping')
      expect(native.invoke.mock.calls.filter(([command]) => command === 'cancel_prompt' || command === 'submit_prompt')).toEqual([])
    } finally { await app.unmount() }
  })
  it('preserves a private profile error in the UI but sends only fixed diagnostic categories', async () => {
    const native = fixture({ native: false, active: false, profileFail: true })
    const app = await mount()
    try {
      await act(async () => { await app.session.intents.changeInstant('local-quality') })
      expect(app.container.textContent).toContain('Synthetic secret=FAKE_TOKEN https://synthetic.invalid/private prompt=FAKE_PROMPT')
      const diagnostics = native.invoke.mock.calls.filter(([command]) => command === 'record_frontend_runtime_diagnostic')
      expect(diagnostics).toContainEqual(['record_frontend_runtime_diagnostic', { event: { kind: 'profile_switch_failed' } }])
      expect(diagnostics).toContainEqual(['record_frontend_runtime_diagnostic', { event: { kind: 'frontend_notice' } }])
      expect(JSON.stringify(diagnostics)).not.toMatch(/FAKE_TOKEN|synthetic\.invalid|FAKE_PROMPT|detail/)
    } finally { await app.unmount() }
  })
  it.each([
    ['completed', 'none'], ['cancelled', 'none'],
    ['completed', 'renderer'], ['cancelled', 'renderer'],
    ['completed', 'native'], ['cancelled', 'native'],
  ] as const)('null-owner retirement race: reconciles typed A %s without cancelling or retargeting successor %s', async (outcome, successor) => {
    vi.useFakeTimers()
    const native = fixture({ native: false, active: true })
    const original = native.invoke.getMockImplementation()!
    const ownership = deferred<unknown>()
    native.invoke.mockImplementation((command, args) => command === 'get_desktop_request_owner' ? ownership.promise : original(command, args))
    const app = await mount()
    try {
      expect(native.invoke).toHaveBeenCalledWith('get_desktop_request_owner', { requestId: 'synthetic-typed-orphan', executionId: { session_id: '1', generation: '2' } })
      expect(app.session.getControlsSnapshot().promptState).toBe('stopping')
      native.retireOrphan(outcome, successor)
      await act(async () => { ownership.resolve(null) })
      await act(async () => { await vi.advanceTimersByTimeAsync(2_000) })
      expect(app.session.getControlsSnapshot().promptState).toBe(successor === 'native' ? 'executing' : 'idle')
      const a = app.session.getMessagesSnapshot().filter((row) => row.role === 'assistant' && row.content === 'Synthetic orphan committed answer')
      expect(a).toHaveLength(outcome === 'completed' ? 1 : 0)
      expect(app.session.getMessagesSnapshot().filter((row) => row.observation?.requestId === 'synthetic-typed-orphan' && row.observation.outcome === 'pending')).toEqual([])
      if (successor !== 'none') expect(app.session.getControlsSnapshot().runtimeStatus).toBe('executing')
      if (successor === 'native') expect(app.container.textContent).toContain('Synthetic next native answer')
      expect(native.invoke.mock.calls.filter(([command]) => command === 'get_desktop_request_owner')).toHaveLength(1)
      expect(native.invoke.mock.calls.filter(([command]) => command === 'cancel_prompt' || command === 'submit_prompt')).toEqual([])
    } finally { await app.unmount(); vi.useRealTimers() }
  })
  it('null-owner retirement race: adds committed A once without disturbing an already attached native B', async () => {
    const native = fixture({ native: false, active: true })
    const original = native.invoke.getMockImplementation()!
    const ownership = deferred<unknown>()
    native.invoke.mockImplementation((command, args) => command === 'get_desktop_request_owner' ? ownership.promise : original(command, args))
    const app = await mount()
    try {
      native.retireOrphan('completed', 'native')
      await act(async () => { native.invalidate() })
      expect(app.container.textContent).toContain('Synthetic next native answer')
      await act(async () => { ownership.resolve(null) })
      const messages = app.session.getMessagesSnapshot()
      expect(messages.filter((row) => row.role === 'assistant' && row.content === 'Synthetic orphan committed answer')).toHaveLength(1)
      expect(messages.filter((row) => row.role === 'assistant' && row.observation?.requestId === 'synthetic-native-next')).toHaveLength(1)
      expect(app.session.getControlsSnapshot().promptState).toBe('executing')
      expect(native.invoke.mock.calls.filter(([command]) => command === 'cancel_prompt' || command === 'submit_prompt')).toEqual([])
    } finally { await app.unmount() }
  })
  it('review regression: an older intermediate partial read cannot consume a hinted successor session', async () => {
    const native = fixture({ native: false, active: false })
    const app = await mount()
    const read = deferred<unknown>()
    try {
      await act(async () => { native.voice({ state: 'active', capture_id: 12, runtime_phase: 'listening', partial_transcript: { session_id: 72, revision: 30, text: 'Synthetic preceding partial' } }); native.invalidate() })
      native.holdRead(read.promise)
      await act(async () => { native.invalidate() })
      const older = native.root()
      await act(async () => { native.voice({ partial_transcript: { session_id: 73, revision: 1, text: 'Synthetic partial text' } }); native.invalidate(); native.event({ source: 'voice', revision: 1, voice_session_id: 73, suffix: ' successor hint' }) })
      await act(async () => { read.resolve(older) })
      expect(app.session.getControlsSnapshot().partialTranscript).toBe('Synthetic partial text')
      expect(app.session.getControlsSnapshot().voiceCompletionSuffix).toBe(' successor hint')
    } finally { await app.unmount() }
  })
  it('review regression: an ownership-query/cancellation race cannot cancel the successor execution', async () => {
    const native = fixture({ native: false, active: true })
    const original = native.invoke.getMockImplementation()!
    let switched = false
    native.invoke.mockImplementation(async (command, args) => {
      if (command === 'get_desktop_request_owner' && !switched) { switched = true; native.next(); return 'renderer' }
      const result = await original(command, args)
      if (command === 'cancel_prompt') native.invalidate()
      return result
    })
    const app = await mount()
    try {
      expect(native.invoke.mock.calls.filter(([command]) => command === 'cancel_prompt')).toEqual([
        ['cancel_prompt', { requestId: 'synthetic-typed-orphan', expectedExecutionId: { session_id: '1', generation: '2' } }],
      ])
      expect(app.container.textContent).toContain('Synthetic next native answer')
      expect(app.session.getControlsSnapshot().promptState).toBe('executing')
      expect(native.invoke.mock.calls.filter(([command]) => command === 'submit_prompt')).toEqual([])
    } finally { await app.unmount() }
  })
  it.each(['reset', 'source-error'] as const)('review regression: pending device continuation cannot renew capture after %s', async (interruption) => {
    const stop = deferred<unknown>()
    const native = fixture({ native: false, active: false, voiceAvailable: true, captureId: 84, stopHold: stop.promise })
    const app = await mount()
    try {
      await act(async () => { void app.session.intents.changeAudioInputDevice('synthetic-input') })
      if (interruption === 'reset') await click(app.container, 'New Chat')
      else await act(async () => { native.voice({ state: 'error', capture_id: null, error: 'Synthetic capture source failure' }); native.invalidate() })
      await act(async () => { stop.resolve(null) })
      expect(native.invoke.mock.calls.filter(([command]) => command === 'start_native_microphone')).toEqual([])
    } finally { await app.unmount() }
  })
  it('review regression: profile command and capture continuation both wait for actual capture retirement', async () => {
    const stop = deferred<unknown>()
    const native = fixture({ native: false, active: false, voiceAvailable: true, captureId: 86, stopHold: stop.promise })
    const app = await mount()
    try {
      await act(async () => { void app.session.intents.changeInstant('local-quality') })
      expect(native.invoke.mock.calls.filter(([command]) => command === 'switch_response_profile' || command === 'start_native_microphone')).toEqual([])
      await act(async () => { stop.resolve(null) })
      expect(native.invoke.mock.calls.filter(([command]) => command === 'switch_response_profile')).toHaveLength(1)
      expect(native.invoke.mock.calls.filter(([command]) => command === 'start_native_microphone')).toHaveLength(1)
    } finally { await app.unmount() }
  })
  it('review regression: changing the selected device while capture is off persists selection without granting capture consent', async () => {
    const native = fixture({ native: false, active: false, voiceAvailable: true })
    const app = await mount()
    try {
      await act(async () => { app.session.intents.changeAudioInputDevice('synthetic-input') })
      expect(window.localStorage.getItem('voxgolem.audioInputDeviceId')).toBe('synthetic-input')
      expect(native.invoke.mock.calls.filter(([command]) => command === 'start_native_microphone' || command === 'reserve_native_microphone_capture_id')).toEqual([])
    } finally { await app.unmount() }
  })
  it.each(['reset', 'endpoint'] as const)('review regression: early completion candidates and late repeated hints cannot survive %s', async (boundary) => {
    const native = fixture({ native: false, active: false })
    const app = await mount()
    const read = deferred<unknown>()
    try {
      await act(async () => {
        native.voice({ state: 'active', capture_id: 12, runtime_phase: 'listening', partial_transcript: { session_id: 72, revision: 3, text: 'Synthetic preceding partial' } }); native.invalidate()
      })
      native.holdRead(read.promise)
      await act(async () => { native.partial(); native.invalidate() })
      const old = native.root()
      await act(async () => { native.event({ source: 'voice', revision: 9, voice_session_id: 73, suffix: ' obsolete early hint' }) })
      if (boundary === 'reset') {
        await click(app.container, 'New Chat')
        await act(async () => { read.resolve(old) })
      } else {
        native.voice({ partial_transcript: null, runtime_phase: 'processing' })
        await act(async () => { read.resolve(native.root()) })
      }
      await act(async () => { native.event({ source: 'voice', revision: 9, voice_session_id: 73, suffix: ' obsolete repeated hint' }) })
      await act(async () => {
        native.voice({ state: 'active', capture_id: 13, runtime_phase: 'listening', partial_transcript: { session_id: 74, revision: 1, text: 'Synthetic next partial' } }); native.invalidate()
      })
      expect(app.session.getControlsSnapshot().voiceCompletionSuffix).toBe('')
      await act(async () => { native.event({ source: 'voice', revision: 1, voice_session_id: 74, suffix: ' valid next hint' }) })
      expect(app.session.getControlsSnapshot().voiceCompletionSuffix).toBe(' valid next hint')
    } finally { await app.unmount() }
  })
  it('review regression: a newer bounded same-session hint waits through an intermediate partial but stale revisions cannot overwrite it', async () => {
    const native = fixture({ native: false, active: false })
    const app = await mount()
    try {
      await act(async () => { native.partial(); native.invalidate() })
      await act(async () => { native.event({ source: 'voice', revision: 11, voice_session_id: 73, suffix: ' newest hint' }) })
      await act(async () => { native.voice({ partial_transcript: { session_id: 73, revision: 10, text: 'Synthetic intermediate partial' } }); native.invalidate() })
      expect(app.session.getControlsSnapshot().voiceCompletionSuffix).toBe('')
      await act(async () => { native.voice({ partial_transcript: { session_id: 73, revision: 11, text: 'Synthetic current partial' } }); native.invalidate() })
      await act(async () => { native.event({ source: 'voice', revision: 10, voice_session_id: 73, suffix: ' obsolete hint' }) })
      expect(app.session.getControlsSnapshot().voiceCompletionSuffix).toBe(' newest hint')
    } finally { await app.unmount() }
  })
  it.each(['native_voice', null] as const)('review regression: absent projection never authorizes orphan cancellation when owner=%s', async (owner) => {
    vi.useFakeTimers()
    const native = fixture({ withholdPrompt: true, owner })
    const app = await mount()
    try {
      expect(native.invoke.mock.calls.filter(([command]) => command === 'cancel_prompt' || command === 'submit_prompt')).toEqual([])
      expect(native.invoke).toHaveBeenCalledWith('get_desktop_request_owner', {
        requestId: 'synthetic-native-prompt', executionId: { session_id: '1', generation: '2' },
      })
      await act(async () => { native.reveal() })
      expect(app.container.textContent).toContain('Synthetic accumulated answer')
      expect(app.session.getMessagesSnapshot().filter((row) => row.role === 'user')).toHaveLength(1)
      expect(native.invoke.mock.calls.filter(([command]) => command === 'cancel_prompt' || command === 'submit_prompt')).toEqual([])
    } finally { await app.unmount(); vi.useRealTimers() }
  })
  it.each(['cancelled', 'error'] as const)('review regression: retires terminal native A (%s) while B remains active beyond the recovery budget', async (outcome) => {
    vi.useFakeTimers()
    const native = fixture()
    native.seedHistory()
    const app = await mount()
    try {
      await act(async () => { native.terminalAndNext(outcome) })
      await act(async () => { await vi.advanceTimersByTimeAsync(2_000) })
      expect(app.container.textContent).toContain('Synthetic next native answer')
      expect(app.session.getControlsSnapshot().promptState).toBe('executing')
      expect(app.session.getMessagesSnapshot().filter((row) => row.role === 'user')).toHaveLength(2)
      expect(app.container.textContent).toContain('Synthetic retained answer')
      expect(native.invoke.mock.calls.filter(([command]) => command === 'submit_prompt' || command === 'cancel_prompt')).toEqual([])
    } finally { await app.unmount(); vi.useRealTimers() }
  })
  it.each([true, false])('review regression: device continuation waits for retirement and restarts exactly once (local handle=%s)', async (local) => {
    const stop = deferred<unknown>()
    const native = fixture({ native: false, active: false, voiceAvailable: true, autoStart: local, ...(local ? {} : { captureId: 80 }), stopHold: stop.promise })
    const app = await mount()
    const starts = native.invoke.mock.calls.filter(([command]) => command === 'start_native_microphone').length
    try {
      await act(async () => { void app.session.intents.changeAudioInputDevice('synthetic-input') })
      expect(native.invoke.mock.calls.filter(([command]) => command === 'stop_native_microphone')).toHaveLength(1)
      expect(native.invoke.mock.calls.filter(([command]) => command === 'start_native_microphone')).toHaveLength(starts)
      await act(async () => { stop.resolve(null) })
      const requests = native.invoke.mock.calls.filter(([command]) => command === 'start_native_microphone')
      expect(requests).toHaveLength(starts + 1)
      expect(requests.at(-1)?.[1]).toMatchObject({ deviceId: 'synthetic-input' })
    } finally { await app.unmount() }
  })
  it.each([
    ['local', false], ['reattached', false], ['off', false],
    ['local', true], ['reattached', true], ['off', true],
  ] as const)('review regression: profile continuation preserves prior capture intent (%s, rollback=%s)', async (capture, rollback) => {
    const native = fixture({ native: false, active: false, voiceAvailable: true, autoStart: capture === 'local', ...(capture === 'reattached' ? { captureId: 90 } : {}), profileFail: rollback })
    const app = await mount()
    const before = native.invoke.mock.calls.filter(([command]) => command === 'start_native_microphone').length
    try {
      await act(async () => { await app.session.intents.changeInstant('local-quality') })
      expect(native.invoke.mock.calls.filter(([command]) => command === 'switch_response_profile')).toHaveLength(1)
      expect(native.invoke.mock.calls.filter(([command]) => command === 'start_native_microphone')).toHaveLength(before + (capture === 'off' ? 0 : 1))
      expect(app.session.getControlsSnapshot().micActive).toBe(capture !== 'off')
    } finally { await app.unmount() }
  })
  it('review regression: rejected reset/cancel retains one native user projection and one assistant through resumed streaming', async () => {
    const native = fixture({ resetReject: true, cancelReject: true })
    const app = await mount()
    try {
      await click(app.container, 'New Chat')
      expect(app.session.getMessagesSnapshot().filter((row) => row.role === 'user')).toHaveLength(1)
      expect(app.session.getMessagesSnapshot().filter((row) => row.role === 'assistant')).toHaveLength(1)
      await act(async () => { native.replace('Synthetic continued native answer'); native.invalidate() })
      expect(app.container.textContent).toContain('Synthetic continued native answer')
      expect(app.session.getMessagesSnapshot().filter((row) => row.role === 'user')).toHaveLength(1)
      expect(native.invoke.mock.calls.filter(([command]) => command === 'submit_prompt')).toEqual([])
    } finally { await app.unmount() }
  })
  it.each([73, 999])('review regression: buffers only a matching completion delivered before its partial read (session=%s)', async (hintSession) => {
    const native = fixture({ native: false, active: false })
    const app = await mount()
    const read = deferred<unknown>()
    try {
      await act(async () => { native.voice({ state: 'active', capture_id: 12, runtime_phase: 'listening' }); native.invalidate() })
      native.holdRead(read.promise)
      await act(async () => { native.partial(); native.invalidate() })
      await act(async () => { native.event({ source: 'voice', revision: 9, voice_session_id: hintSession, suffix: ' synthetic early suffix' }) })
      expect(app.session.getControlsSnapshot().voiceCompletionSuffix).toBe('')
      await act(async () => { read.resolve(native.root()) })
      expect(app.session.getControlsSnapshot().voiceCompletionSuffix).toBe(hintSession === 73 ? ' synthetic early suffix' : '')
    } finally { await app.unmount() }
  })
  it('does not construct or resume any renderer audio owner when attaching to autonomous native work', async () => {
    const native = fixture()
    const audio = vi.fn()
    vi.stubGlobal('Audio', audio)
    const app = await mount()
    try {
      expect(native.invoke.mock.calls.filter(([command]) => [
        'set_native_audio_active', 'reserve_local_tts_playback_id', 'speak_local_tts', 'finish_tts_playback',
        'reserve_cue_playback_id', 'activate_voice_cue', 'finish_cue_playback', 'prepare_voice_cues',
      ].includes(command))).toEqual([])
      expect(audio).not.toHaveBeenCalled()
    } finally { await app.unmount(); vi.unstubAllGlobals() }
  })
  it('does not mistake a reattached capture Stop request or failed acknowledgement for projected physical retirement', async () => {
    const stopped = deferred<unknown>()
    const native = fixture({ native: false, active: false, voiceAvailable: true, captureId: 79, stopHold: stopped.promise })
    const app = await mount()
    try {
      await click(app.container, 'Stop mic')
      expect(app.session.getControlsSnapshot().micActive).toBe(true)
      await act(async () => { stopped.reject(new Error('Synthetic stop transport failure')) })
      expect(app.session.getControlsSnapshot().micActive).toBe(true)
      expect(button(app.container, 'Stop mic').disabled).toBe(false)
      expect(native.invoke.mock.calls.filter(([command]) => command === 'start_native_microphone')).toEqual([])
    } finally { await app.unmount() }
  })
  it('a late old-capture Stop failure cannot stop a newer user-started capture or publish its obsolete error', async () => {
    const oldStop = deferred<unknown>()
    const native = fixture({ native: false, active: false, voiceAvailable: true, autoStart: true, stopHold: oldStop.promise })
    const app = await mount()
    try {
      await click(app.container, 'Stop mic')
      await act(async () => { native.voice({ state: 'stopped', capture_id: null }); native.invalidate() })
      await click(app.container, 'Start mic')
      await act(async () => { oldStop.reject(new Error('Synthetic obsolete Stop failure')) })
      expect(app.session.getControlsSnapshot().micActive).toBe(true)
      expect(native.invoke.mock.calls.filter(([command]) => command === 'stop_native_microphone')).toHaveLength(1)
      expect(native.invoke.mock.calls.filter(([command]) => command === 'start_native_microphone')).toHaveLength(2)
      expect(app.container.textContent).not.toContain('Synthetic obsolete Stop failure')
    } finally { await app.unmount() }
  })
  it('recovers a missed native terminal before attaching the next canonical execution without waiting for global idleness', async () => {
    const native = fixture()
    const app = await mount()
    try {
      await act(async () => { native.finish(); native.next(); native.invalidate() })
      expect(app.container.textContent).toContain('Synthetic canonical answer')
      expect(app.container.textContent).toContain('Synthetic next native answer')
      expect(app.session.getControlsSnapshot().promptState).toBe('executing')
      expect(app.session.getMessagesSnapshot().filter((message) => message.role === 'user')).toHaveLength(2)
      expect(native.invoke.mock.calls.filter(([command]) => command === 'submit_prompt' || command === 'cancel_prompt')).toEqual([])
    } finally { await app.unmount() }
  })
  it('reattaches accumulated native text after observation loss without cancellation, duplicate admission or speech replay', async () => {
    const native = fixture()
    const app = await mount()
    try {
      const lost = deferred<unknown>()
      native.holdRead(lost.promise)
      await act(async () => { native.invalidate(); lost.reject(new Error('Synthetic observer transport loss')) })
      expect(app.session.getControlsSnapshot().desktopRuntimeReady).toBe(false)
      expect(app.session.getControlsSnapshot().promptState).toBe('executing')
      await act(async () => { native.replace('Synthetic resumed accumulated text'); window.dispatchEvent(new Event('focus')) })
      expect(app.container.textContent).toContain('Synthetic resumed accumulated text')
      expect(app.container.textContent).not.toContain('Synthetic accumulated answer')
      expect(native.invoke.mock.calls.filter(([command]) => command === 'cancel_prompt' || command === 'submit_prompt' || command === 'speak_local_tts')).toEqual([])
    } finally { await app.unmount() }
  })
  it('bounds failed reset snapshot recovery and leaves real New Chat available without restoring old projections', async () => {
    vi.useFakeTimers()
    const native = fixture({ native: false, active: false, resetSnapshotFail: true })
    const app = await mount()
    const before = native.invoke.mock.calls.filter(([command]) => command === 'get_app_snapshot').length
    try {
      await click(app.container, 'New Chat')
      // Exercise reset's 250 + 500 ms recovery budget independently of the
      // existing one-second startup/capability refresh owner.
      await act(async () => { await vi.advanceTimersByTimeAsync(750) })
      expect(native.invoke.mock.calls.filter(([command]) => command === 'get_app_snapshot')).toHaveLength(before + 3)
      expect(app.session.getControlsSnapshot().promptState).toBe('reconciling')
      expect(app.session.getMessagesSnapshot()).toEqual([])
      expect(button(app.container, 'New Chat').disabled).toBe(false)
      expect(app.container.textContent).toContain('Session recovery required')
    } finally { await app.unmount(); vi.useRealTimers() }
  })
  it.each([false, true])('reconciles native terminal sources/refusal=%s and actual stage metadata through canonical parsers', async (refused) => {
    const native = fixture()
    const app = await mount()
    const observed = { url: 'https://example.test/observed', title: 'Synthetic provisional source' }
    const canonical = { url: 'https://example.test/canonical', title: 'Synthetic canonical source' }
    try {
      await act(async () => {
        native.updates([
          { kind: 'text', text: 'Synthetic current provisional answer' },
          { kind: 'sources', sources: [observed] },
          { kind: 'stage', stage: 'deep', status: 'corrected', detail: 'Synthetic correction' },
          { kind: 'stage', stage: 'review', status: 'failed', detail: 'Synthetic review failure' },
        ])
        native.invalidate()
      })
      expect(app.container.textContent).toContain('Synthetic current provisional answer')
      await act(async () => { native.finish(refused, refused ? [] : [canonical], 'deep'); native.invalidate() })
      const message = app.session.getMessagesSnapshot().find((row) => row.role === 'assistant')!
      expect(message.contentKind).toBe(refused ? 'refusal' : 'text')
      expect(message.answerStage?.sources).toEqual(refused ? [] : [canonical])
      expect(message.answerStage?.stages).toContainEqual({ stage: 'review', status: 'failed', detail: 'Synthetic review failure' })
      expect(app.session.getAnnouncementSnapshot()).toBe(`${refused ? 'Assistant refusal' : 'Assistant response'}: Synthetic canonical answer`)
      expect(native.invoke.mock.calls.filter(([command]) => command === 'submit_prompt' || command === 'speak_local_tts')).toEqual([])
    } finally { await app.unmount() }
  })
  it('serializes native Auto Stop writes and ignores obsolete acknowledgements without losing the latest effective setting', async () => {
    const first = deferred<boolean>()
    let calls = 0
    const native = fixture({ native: false, active: false, voiceAvailable: true, autoStop: true, autoStopSave: async (enabled) => ++calls === 1 ? first.promise : enabled })
    const app = await mount()
    try {
      await click(app.container, 'Settings')
      await act(async () => { autoStopToggle(app.container).click() })
      await act(async () => { app.session.intents.setAutoStopOnSilence(true) })
      expect(native.invoke.mock.calls.filter(([command]) => command === 'set_voice_auto_stop')).toHaveLength(1)
      await act(async () => { first.resolve(false) })
      expect(native.invoke.mock.calls.filter(([command]) => command === 'set_voice_auto_stop')).toHaveLength(2)
      expect(autoStopToggle(app.container).checked).toBe(true)
    } finally { await app.unmount() }
  })
  it('consumes initial native autostart consent once and never renews it on renderer reload or New Chat', async () => {
    const native = fixture({ native: false, active: false, voiceAvailable: true, autoStart: true })
    const first = await mount()
    expect(native.invoke.mock.calls.filter(([command]) => command === 'start_native_microphone')).toHaveLength(1)
    await click(first.container, 'Stop mic')
    await first.unmount()
    const second = await mount()
    try {
      expect(native.invoke.mock.calls.filter(([command]) => command === 'start_native_microphone')).toHaveLength(1)
      await click(second.container, 'New Chat')
      expect(native.invoke.mock.calls.filter(([command]) => command === 'start_native_microphone')).toHaveLength(1)
      await click(second.container, 'Start mic')
      expect(native.invoke.mock.calls.filter(([command]) => command === 'start_native_microphone')).toHaveLength(2)
      expect(native.invoke.mock.calls.filter(([command]) => command === 'reserve_native_microphone_capture_id')).toHaveLength(2)
    } finally { await second.unmount() }
  })
  it('waits for the initial observation receipt before autostart, without guessing consent from startup capabilities', async () => {
    const hold = deferred<unknown>()
    const native = fixture({ native: false, active: false, voiceAvailable: true, autoStart: false, holdRead: hold.promise })
    const app = await mount()
    try {
      expect(native.invoke.mock.calls.filter(([command]) => command === 'start_native_microphone')).toEqual([])
      await act(async () => { hold.resolve(native.root()) })
      expect(native.invoke.mock.calls.filter(([command]) => command === 'start_native_microphone')).toEqual([])
      await click(app.container, 'Start mic')
      expect(native.invoke.mock.calls.filter(([command]) => command === 'start_native_microphone')).toHaveLength(1)
    } finally { await app.unmount() }
  })
  it('stops an already-active native capture by its real projected ID after renderer reattachment', async () => {
    const native = fixture({ native: false, active: false, voiceAvailable: true, captureId: 47, autoStart: false })
    const app = await mount()
    try {
      expect(native.invoke.mock.calls.filter(([command]) => command === 'start_native_microphone')).toEqual([])
      await click(app.container, 'Stop mic')
      expect(native.invoke.mock.calls.filter(([command]) => command === 'stop_native_microphone')).toEqual([['stop_native_microphone', { captureId: 47 }]])
      await act(async () => { native.invalidate() })
      expect(native.invoke.mock.calls.filter(([command]) => command === 'start_native_microphone')).toEqual([])
    } finally { await app.unmount() }
  })
  it('projects a source failure without treating it as silence, cancelling its prompt, or renewing capture consent', async () => {
    const native = fixture({ voiceAvailable: true, captureId: 51 })
    const app = await mount()
    try {
      await act(async () => { native.voice({ capture_id: null, state: 'error', error: 'Synthetic input source failed', auto_start_available: false }); native.invalidate() })
      expect(app.container.textContent).toContain('Microphone unavailable')
      expect(app.container.querySelector('.answer-stage__answer')?.textContent).toBe('Synthetic accumulated answer')
      expect(native.invoke.mock.calls.filter(([command]) => ['cancel_prompt', 'start_native_microphone', 'mark_silence', 'ingest_audio_frame'].includes(command))).toEqual([])
    } finally { await app.unmount() }
    const reattached = await mount()
    try { expect(native.invoke.mock.calls.filter(([command]) => command === 'start_native_microphone')).toEqual([]) }
    finally { await reattached.unmount() }
  })
  it.each([true, false])('preserves selected-device fallback semantics when native capture succeeds=%s', async (success) => {
    window.localStorage.setItem('voxgolem.audioInputDeviceId', 'synthetic-input')
    const native = fixture({ native: false, active: false, voiceAvailable: true, autoStart: true, fallback: true, startFail: !success })
    const app = await mount()
    try {
      expect(native.invoke.mock.calls.find(([command]) => command === 'start_native_microphone')?.[1]).toMatchObject({ deviceId: 'synthetic-input' })
      expect(window.localStorage.getItem('voxgolem.audioInputDeviceId')).toBe(success ? null : 'synthetic-input')
      expect(native.invoke.mock.calls.filter(([command]) => command === 'start_native_microphone')).toHaveLength(1)
      if (!success) expect(app.container.textContent).toContain('Microphone unavailable')
    } finally { await app.unmount() }
  })
  it.each(['resolve', 'reject'] as const)('fences delayed coarse microphone startup and fallback metadata across reset: %s', async (outcome) => {
    const hold = deferred<captureModule.LiveAudioSource>()
    let options: StartLiveAudioSourceOptions | undefined
    const stop = vi.fn(async () => undefined)
    vi.spyOn(captureModule, 'startLiveAudioSource').mockImplementation((next) => { options = next; return hold.promise })
    window.localStorage.setItem('voxgolem.audioInputDeviceId', 'synthetic-input')
    const native = fixture({ native: false, active: false, voiceAvailable: true, autoStart: true })
    const app = await mount()
    try {
      expect(options).toBeDefined()
      await click(app.container, 'New Chat')
      await act(async () => {
        options?.onSelectedDeviceFallback?.('synthetic-input')
        if (outcome === 'resolve') hold.resolve({ captureId: 58, stop, detach: vi.fn() })
        else hold.reject(new Error('Synthetic obsolete start failure'))
      })
      expect(window.localStorage.getItem('voxgolem.audioInputDeviceId')).toBe('synthetic-input')
      expect(app.session.getControlsSnapshot().micActive).toBe(false)
      expect(app.container.textContent).not.toContain('Synthetic obsolete start failure')
      expect(stop).toHaveBeenCalledTimes(outcome === 'resolve' ? 1 : 0)
      expect(native.invoke.mock.calls.filter(([command]) => command === 'submit_prompt')).toEqual([])
    } finally { await app.unmount() }
  })
  it('keeps both microphone and model controls fenced while reset is pending, without renewing autostart afterwards', async () => {
    const hold = deferred<unknown>()
    const native = fixture({ native: false, active: false, voiceAvailable: true, captureId: 61, resetHold: hold.promise })
    const app = await mount()
    try {
      await click(app.container, 'New Chat')
      expect(button(app.container, 'Stop mic').disabled).toBe(true)
      expect(app.container.querySelector<HTMLSelectElement>('#composerInstantSelect')?.disabled).toBe(true)
      await act(async () => { hold.resolve(null) })
      expect(app.session.getControlsSnapshot().resetPending).toBe(false)
      expect(native.invoke.mock.calls.filter(([command]) => command === 'start_native_microphone')).toEqual([])
    } finally { await app.unmount() }
  })
  it('does not let a pre-reset observation response restore native prompt rows or partial text', async () => {
    const native = fixture()
    const app = await mount()
    const old = native.root()
    const hold = deferred<unknown>()
    try {
      native.holdRead(hold.promise)
      await act(async () => { native.invalidate() })
      await click(app.container, 'New Chat')
      await act(async () => { hold.resolve(old) })
      expect(app.container.textContent).not.toContain('Synthetic native question')
      expect(app.container.textContent).not.toContain('Synthetic accumulated answer')
      expect(app.session.getMessagesSnapshot()).toEqual([])
      expect(native.invoke.mock.calls.filter(([command]) => command === 'submit_prompt')).toEqual([])
    } finally { await app.unmount() }
  })
  it('reattaches a valid native request despite stale unavailable model controls without readmission', async () => {
    const native = fixture({ modelAvailable: false })
    const app = await mount()
    try {
      expect(app.container.textContent).toContain('Synthetic accumulated answer')
      expect(native.invoke.mock.calls.filter(([command]) => command === 'submit_prompt' || command === 'cancel_prompt')).toEqual([])
    } finally { await app.unmount() }
  })
  it.each(['model', 'settings'] as const)('retains typed admission safety when native %s controls are unavailable', async (kind) => {
    const native = fixture({ native: false, active: false, modelAvailable: kind !== 'model', settingsFail: kind === 'settings', snapshotFail: kind === 'settings' })
    const app = await mount()
    try {
      await sendTyped(app.container)
      expect(native.invoke.mock.calls.filter(([command]) => command === 'submit_prompt')).toEqual([])
      expect(app.session.getControlsSnapshot().composerValue).toBe('Synthetic typed question')
    } finally { await app.unmount() }
  })
  it.each([
    ['completed', 'listening'], ['cancelled', 'listening'], ['error', 'listening'], ['rejected', 'listening'],
    ['completed', 'error'], ['cancelled', 'error'], ['error', 'error'], ['rejected', 'error'],
  ] as const)('preserves newer native state through typed %s reconciliation: %s', async (outcome, phase) => {
    const native = fixture({ native: false, active: false })
    const app = await mount()
    try {
      await sendTyped(app.container)
      await act(async () => {
        if (phase === 'listening') native.partial()
        else native.voice({ runtime_phase: 'error', state: 'error', capture_id: null, error: 'Synthetic native capture failure' })
        native.invalidate()
      })
      await act(async () => { native.typedFinish(outcome) })
      expect(app.container.querySelector('.shell')?.getAttribute('data-runtime-status')).toBe(phase)
      expect(app.session.getControlsSnapshot().promptState).toBe('idle')
      expect(app.container.textContent.includes('Synthetic typed answer')).toBe(outcome === 'completed')
      expect(native.invoke.mock.calls.filter(([command]) => command === 'speak_local_tts')).toEqual([])
    } finally { await app.unmount() }
  })
  it('uses native partial identity/revision for completion and ignores old-session or superseded completions', async () => {
    const native = fixture({ native: false, active: false })
    const app = await mount()
    try {
      await act(async () => { native.partial(); native.invalidate() })
      await act(async () => {
        native.event({ source: 'voice', revision: 9, voice_session_id: 72, suffix: ' obsolete' })
        native.event({ source: 'voice', revision: 8, voice_session_id: 73, suffix: ' stale' })
      })
      expect(app.session.getControlsSnapshot().voiceCompletionSuffix).toBe('')
      await act(async () => { native.event({ source: 'voice', revision: 9, voice_session_id: 73, suffix: ' current' }) })
      expect(app.session.getControlsSnapshot().voiceCompletionSuffix).toBe(' current')
      await act(async () => { native.speech(false, true); native.invalidate() })
      expect(app.session.getControlsSnapshot().voiceCompletionSuffix).toBe(' current')
      await act(async () => { native.voice({ partial_transcript: null, runtime_phase: 'processing' }); native.invalidate() })
      await act(async () => { native.event({ source: 'voice', revision: 9, voice_session_id: 73, suffix: ' obsolete' }) })
      expect(app.session.getControlsSnapshot().voiceCompletionSuffix).toBe('')
    } finally { await app.unmount() }
  })
  it('projects native wake confidence and phase without renderer cue or endpointing decisions, even while hidden', async () => {
    const native = fixture({ native: false, active: false })
    const app = await mount()
    try {
      for (const [phase, confidence] of [['listening', 0.95], ['processing', 0.4], ['sleeping', 0.2]] as const) {
        await act(async () => { native.voice({ runtime_phase: phase, wake_confidence: confidence }); native.invalidate(); document.dispatchEvent(new Event('visibilitychange')) })
        expect(app.session.getControlsSnapshot().runtimeStatus).toBe(phase)
        expect(app.session.getControlsSnapshot().wakeConfidence).toBe(confidence)
      }
      expect(native.invoke.mock.calls.filter(([command]) => ['activate_voice_cue', 'reserve_cue_playback_id', 'prepare_voice_cues', 'ingest_audio_frame', 'mark_silence', 'save_wake_clip'].includes(command))).toEqual([])
    } finally { await app.unmount() }
  })
  it('uses native pending/playing speech to block update installation without any renderer speech request', async () => {
    const native = fixture({ native: false, active: false, updateReady: true })
    const app = await mount()
    try {
      await click(app.container, 'Settings')
      expect(button(app.container, 'Restart and update').disabled).toBe(false)
      for (const [playing, pending] of [[false, true], [true, false], [false, false]]) {
        await act(async () => { native.speech(playing!, pending!); native.invalidate() })
        expect(button(app.container, 'Restart and update').disabled).toBe(playing || pending)
      }
      expect(native.invoke.mock.calls.filter(([command]) => command === 'speak_local_tts' || command === 'finish_tts_playback')).toEqual([])
    } finally { await app.unmount() }
  })
  it('reattaches native-effective Auto Stop and changes it through the native command, preserving failed-write rollback', async () => {
    let fail = false
    const native = fixture({ native: false, active: false, voiceAvailable: true, autoStop: false, autoStopSave: async (enabled) => { if (fail) throw new Error('Synthetic preference failure'); return enabled } })
    const app = await mount()
    try {
      await click(app.container, 'Settings')
      expect(autoStopToggle(app.container).checked).toBe(false)
      await act(async () => { autoStopToggle(app.container).click() })
      expect(autoStopToggle(app.container).checked).toBe(true)
      fail = true
      await act(async () => { autoStopToggle(app.container).click() })
      expect(autoStopToggle(app.container).checked).toBe(true)
      expect(native.invoke.mock.calls.filter(([command]) => command === 'set_voice_auto_stop')).toEqual([['set_voice_auto_stop', { enabled: true }], ['set_voice_auto_stop', { enabled: false }]])
    } finally { await app.unmount() }
    const reattached = await mount()
    try { expect(reattached.session.getControlsSnapshot().autoStopOnSilence).toBe(true) }
    finally { await reattached.unmount() }
  })
  it('fences a stale Auto Stop root read across native preference publication', async () => {
    const save = deferred<boolean>()
    const native = fixture({ native: false, active: false, voiceAvailable: true, autoStop: true, autoStopSave: () => save.promise })
    const app = await mount()
    const stale = native.root()
    const read = deferred<unknown>()
    try {
      await click(app.container, 'Settings')
      native.holdRead(read.promise)
      await act(async () => { native.invalidate() })
      await act(async () => { autoStopToggle(app.container).click() })
      await act(async () => { save.resolve(false) })
      expect(app.session.getControlsSnapshot().autoStopOnSilence).toBe(false)
      await act(async () => { read.resolve(stale) })
      expect(autoStopToggle(app.container).checked).toBe(false)
    } finally { await app.unmount() }
  })
  it('waits for listener/read ownership before deciding whether hydration found a typed orphan', async () => {
    let release!: (value: unknown) => void
    const holdRead = new Promise((resolve) => { release = resolve })
    const native = fixture({ holdRead })
    const app = await mount()
    try {
      expect(native.invoke.mock.calls.filter(([command]) => command === 'cancel_prompt')).toEqual([])
      await act(async () => { release({ revision: 1, snapshot: { revision: 1, voice: stoppedVoice, speech: { playback_id: null, playing: false, pending: false }, prompt: nativePrompt() }, events: [] }) })
      expect(app.container.textContent).toContain('Synthetic accumulated answer')
      expect(native.invoke.mock.calls.filter(([command]) => command === 'cancel_prompt' || command === 'submit_prompt')).toEqual([])
    } finally { await app.unmount() }
  })
  it('retains typed-orphan recovery only after affirmative renderer ownership, with an exact execution cancellation fence', async () => {
    const native = fixture({ native: false, active: true })
    const app = await mount()
    try {
      expect(native.invoke).toHaveBeenCalledWith('get_desktop_request_owner', { requestId: 'synthetic-typed-orphan', executionId: { session_id: '1', generation: '2' } })
      expect(native.invoke.mock.calls.filter(([command]) => command === 'cancel_prompt')).toEqual([['cancel_prompt', { requestId: 'synthetic-typed-orphan', expectedExecutionId: { session_id: '1', generation: '2' } }]])
    } finally { await app.unmount() }
  })
  it('attaches to a native admission without submitting or cancelling it, and replaces accumulated text on reconnect', async () => {
    const native = fixture()
    const app = await mount()
    try {
      expect(app.container.textContent).toContain('Synthetic accumulated answer')
      expect(native.invoke.mock.calls.filter(([command]) => command === 'submit_prompt' || command === 'cancel_prompt')).toEqual([])
      native.replace('Synthetic replacement answer')
      await act(async () => { native.invalidate() })
      expect(app.container.textContent).toContain('Synthetic replacement answer')
      expect(app.container.textContent).not.toContain('Synthetic accumulated answer')
      expect(app.container.querySelectorAll('[data-request-id="synthetic-native-prompt"]')).toHaveLength(2)
      await act(async () => { native.finish(); native.invalidate() })
      expect(app.container.querySelector('.answer-stage__answer')?.textContent).toBe('Synthetic canonical answer')
      expect(native.invoke.mock.calls.filter(([command]) => command === 'submit_prompt' || command === 'speak_local_tts')).toEqual([])
    } finally { await app.unmount() }
    expect(native.invoke.mock.calls.filter(([command]) => command === 'cancel_prompt' || command === 'stop_native_microphone')).toEqual([])
    expect(native.invoke.mock.calls.filter(([command]) => command === 'set_native_audio_active')).toEqual([])
  })
  it('uses real native partial session identity for completion projection', async () => {
    const native = fixture({ native: false, active: false })
    const app = await mount()
    try {
      await act(async () => { native.partial(); native.invalidate() })
      expect(app.container.textContent).toContain('Synthetic partial text')
      expect(native.invoke.mock.calls.filter(([command]) => command === 'ingest_audio_frame' || command === 'mark_silence')).toEqual([])
    } finally { await app.unmount() }
  })
  it('Stop targets the canonical native request, while losing the renderer never does', async () => {
    const native = fixture()
    const app = await mount()
    try {
      await act(async () => { app.container.querySelector<HTMLButtonElement>('button[aria-label="Stop"]')!.click() })
      expect(native.invoke.mock.calls.filter(([command]) => command === 'cancel_prompt')).toEqual([['cancel_prompt', { requestId: 'synthetic-native-prompt' }]])
    } finally { await app.unmount() }
    expect(native.invoke.mock.calls.filter(([command]) => command === 'cancel_prompt')).toHaveLength(1)
  })
  it('reattaches after detached completion through canonical history, never replaying speech or optimistic rows', async () => {
    const native = fixture()
    const first = await mount()
    await first.unmount()
    native.finish(); native.retire()
    const second = await mount()
    try {
      expect(second.container.querySelectorAll('.answer-stage__answer')).toHaveLength(1)
      expect(second.container.textContent).toContain('Synthetic canonical answer')
      expect(native.invoke.mock.calls.filter(([command]) => command === 'submit_prompt' || command === 'cancel_prompt' || command === 'speak_local_tts')).toEqual([])
    } finally { await second.unmount() }
  })
  it('does not cancel an unknown active request when native observation cannot be established', async () => {
    const native = fixture({ unavailable: true })
    const app = await mount()
    try {
      expect(native.invoke.mock.calls.filter(([command]) => command === 'cancel_prompt')).toEqual([])
      expect(app.container.textContent).toContain('Native session observation unavailable')
    } finally { await app.unmount() }
  })
  it('retains the typed command Channel but delegates reply speech, including hidden output, entirely to native', async () => {
    const native = fixture({ native: false, active: false })
    const app = await mount()
    try {
      const composer = app.container.querySelector<HTMLTextAreaElement>('#promptComposer')!
      await act(async () => {
        Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')!.set!.call(composer, 'Synthetic typed question')
        composer.dispatchEvent(new Event('input', { bubbles: true }))
      })
      await act(async () => { app.container.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
      await act(async () => { native.typedUpdate('Synthetic first paragraph.\n\nSynthetic detail.') })
      expect(app.container.textContent).toContain('Synthetic first paragraph.')
      await act(async () => { document.dispatchEvent(new Event('visibilitychange')); native.speech(true, false); native.invalidate() })
      expect(native.invoke.mock.calls.filter(([command]) => command === 'set_native_audio_active')).toEqual([])
      await act(async () => { native.typedFinish() })
      expect(app.container.textContent).toContain('Synthetic typed answer')
      expect(native.invoke.mock.calls.filter(([command]) => command === 'speak_local_tts' || command === 'reserve_local_tts_playback_id')).toEqual([])
    } finally { await app.unmount() }
  })
})

import { StrictMode, act, useState } from 'react'
import { createRoot } from 'react-dom/client'
import type { Root } from 'react-dom/client'
import type { ReactNode } from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { AndroidVoiceControls as AndroidVoiceView } from './AndroidVoiceControls'
import { useAndroidVoiceAdapter } from '../lib/useAndroidVoiceAdapter'
import type { AndroidVoiceAdapterInputs } from '../lib/useAndroidVoiceAdapter'
import { ControlIcon } from './ControlIcon'

// Integration harness owns the adapter; the rendered controls only consume its view.
function AndroidVoiceControls(props: AndroidVoiceAdapterInputs & { compact?: boolean; modelControl?: ReactNode }) {
  const voice = useAndroidVoiceAdapter(props)
  return <AndroidVoiceView voice={voice} disabled={props.disabled} generationAvailable={props.generationAvailable}
    compact={props.compact ?? false} modelControl={props.modelControl} />
}

let root: Root | null = null
let container: HTMLElement | null = null
const wire = { kind: 'voice_status', available: true, reason: 'Synthetic local recognizer', mode: 'dictation', dictation: 'idle', loop: 'idle', ttsEnabled: false, ttsAvailable: false, ttsPhase: 'idle', error: null }
afterEach(() => { act(() => root?.unmount()); container?.remove(); root = null; container = null; delete window.__TAURI_INTERNALS__; vi.useRealTimers(); vi.restoreAllMocks() })

async function setup(compact = false, initialStatus: Record<string, unknown> = {}, { disabled = false, generationAvailable = true } = {}) {
  let channel: (value: unknown) => void = () => undefined
  let sessionId = ''
  const onDraft = vi.fn()
  const onSubmit = vi.fn()
  const onInputCancelled = vi.fn()
  const stopRef = { current: null as (() => void) | null }
  const native = vi.fn(async (name: string, args?: unknown): Promise<unknown> => {
    if (name === 'assistant_voice_status') return { ...wire, ...initialStatus }
    if (name === 'start_assistant_voice') { sessionId = (args as { sessionId: string }).sessionId; const mode = (args as { loopMode: boolean }).loopMode ? 'loop' : (args as { completionPolicy: string }).completionPolicy === 'single_turn' ? 'ask' : 'dictation'; return { ...wire, mode, dictation: 'listening', loop: mode === 'dictation' ? 'idle' : 'listening' } }
    return null
  })
  window.__TAURI_INTERNALS__ = {
    createChannel: <T,>(handler: (value: T) => void) => { channel = handler as (value: unknown) => void; return {} },
    invoke: native,
  }
  container = document.createElement('div'); document.body.append(container); root = createRoot(container)
  await act(async () => { root?.render(<AndroidVoiceControls draft="" disabled={disabled} generationAvailable={generationAvailable} stopRef={stopRef} onDraft={onDraft} onSubmit={onSubmit} onActivity={vi.fn()} onInputCancelled={onInputCancelled} onSettled={vi.fn()} compact={compact} modelControl={compact ? <button aria-label="Model"><ControlIcon kind="model" /></button> : undefined} />) })
  return { native, onDraft, onSubmit, onInputCancelled, stopRef, event: (value: Record<string, unknown>) => channel({ sessionId, ...value }) }
}

function compactButton(label: string): HTMLButtonElement {
  return container!.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`)!
}

function expectIconOnlyRow() {
  expect(container!.querySelectorAll('button')).toHaveLength(4)
  const visible = container!.cloneNode(true) as HTMLElement
  visible.querySelectorAll('.sr-only').forEach((element) => element.remove())
  expect(visible.textContent).toBe('')
  for (const button of container!.querySelectorAll('button')) expect(button.querySelector('svg[aria-hidden="true"]')).not.toBeNull()
}

describe('compact Android voice icons', () => {
  it('starts Ask in the owned current draft and uses square Stop cancellation, never Finish', async () => {
    const state = await setup(true)
    await act(async () => { compactButton('Ask').click() })
    expect(state.native).toHaveBeenCalledWith('start_assistant_voice', expect.objectContaining({ loopMode: false, completionPolicy: 'single_turn', draft: '' }))
    expect(compactButton('Stop voice').querySelector('svg')?.getAttribute('data-icon-kind')).toBe('stop')
    expect(compactButton('Finish dictation')).toBeNull()
    await act(async () => { compactButton('Stop voice').click(); state.event({ kind: 'submit', requestId: 'synthetic-cancelled-final', text: 'Synthetic stale final' }) })
    expect(state.native).toHaveBeenCalledWith('stop_assistant_voice', undefined)
    expect(state.native.mock.calls.some(([name]) => name === 'finish_assistant_dictation')).toBe(false)
    expect(state.onSubmit).not.toHaveBeenCalled()
  })
  it.each([
    [{}, 'Ask', 'ready', 'Ready to dictate'],
    [{ available: false, reason: 'Synthetic microphone permission denied' }, 'Ask', 'unavailable', 'Synthetic microphone permission denied'],
    [{ dictation: 'checking' }, 'Stop voice', 'busy', 'Starting voice…'],
    [{ dictation: 'listening', readyForSpeech: true }, 'Stop voice', 'listening', 'Listening…'],
    [{ dictation: 'finishing' }, 'Stop voice', 'finishing', 'Finishing dictation…'],
    [{ mode: 'loop', dictation: 'listening', loop: 'listening', readyForSpeech: true }, 'Stop voice', 'listening', 'Listening…'],
    [{ mode: 'loop', loop: 'generating' }, 'Stop voice', 'generating', 'Responding…'],
    [{ mode: 'loop', loop: 'speaking' }, 'Stop voice', 'speaking', 'Speaking…'],
    [{ mode: 'loop', loop: 'between_turns' }, 'Stop voice', 'between-turns', 'Ready for next turn…'],
  ])('keeps four icons with a shape mark for %j', async (status, label, state, description) => {
    await setup(true, status as Record<string, unknown>)
    expectIconOnlyRow()
    const button = compactButton(label as string)
    const icon = button.querySelector('svg')!
    expect(icon.getAttribute('data-icon-state')).toBe(state)
    expect(icon.querySelector('[data-state-mark] path')?.getAttribute('d')).toBeTruthy()
    expect(button.title).toContain(description)
    const describedBy = button.getAttribute('aria-describedby')!
    expect(document.getElementById(describedBy)?.textContent).toContain(description)
    if ((status as Record<string, unknown>)['mode'] === 'loop') expect(button.getAttribute('aria-pressed')).toBe('true')
  })
  it('cancels a live Ask while retaining partial input without sending', async () => {
    const state = await setup(true)
    await act(async () => { compactButton('Ask').click() })
    expect(compactButton('Ask').getAttribute('aria-pressed')).toBe('true')
    expect(compactButton('Ask').disabled).toBe(true)
    expect(compactButton('Stop voice').querySelector('svg')?.getAttribute('data-icon-kind')).toBe('stop')
    compactButton('Stop voice').focus()
    expect(document.activeElement).toBe(compactButton('Stop voice'))
    await act(async () => { state.event({ kind: 'dictation', phase: 'partial', draft: 'Synthetic partial draft' }) })
    expect(state.onDraft).toHaveBeenCalledWith('Synthetic partial draft')
    expect(state.onSubmit).not.toHaveBeenCalled()
    expectIconOnlyRow()
    await act(async () => { compactButton('Stop voice').click(); state.event({ kind: 'dictation', phase: 'final', draft: 'Synthetic stale draft' }) })
    expect(state.native).toHaveBeenCalledWith('stop_assistant_voice', undefined)
    expect(state.onInputCancelled).toHaveBeenCalledTimes(1)
    expect(state.onDraft).toHaveBeenCalledTimes(1)
  })
  it('routes a Talk authoritative submit once without presenting Finish as Stop', async () => {
    const state = await setup(true)
    await act(async () => { compactButton('Talk').click() })
    expect(compactButton('Finish conversation turn')).toBeNull()
    expect(compactButton('Stop voice').title).toContain('Cancel recognition, request and playback; do not submit')
    expect(compactButton('Stop voice').disabled).toBe(false)
    expect(state.onSubmit).not.toHaveBeenCalled()
    await act(async () => { state.event({ kind: 'submit', requestId: 'synthetic-loop-turn', text: 'Synthetic finalized loop input' }) })
    expect(state.onSubmit).toHaveBeenCalledWith('synthetic-loop-turn', 'Synthetic finalized loop input')
    expectIconOnlyRow()
  })
  it.each(['listening', 'finishing'])('cancels legacy shortcut without mislabelling Finish as Stop in %s', async (shortcutPhase) => {
    const state = await setup(true, { mode: 'shortcut', dictation: shortcutPhase, shortcutPhase })
    expectIconOnlyRow()
    expect(compactButton('Stop & send')).toBeNull()
    expect(compactButton('Stop voice').disabled).toBe(false)
    await act(async () => { compactButton('Stop voice').click() })
    expect(state.native.mock.calls.some(([name]) => name === 'stop_send_new_voice_shortcut')).toBe(false)
    expect(state.onSubmit).not.toHaveBeenCalled()
  })
  it.each(['no_speech', 'recognition_error_7', 'Microphone permission denied; enable access in system Settings'])('retains actionable error accessibility and quiet no-match: %s', async (error) => {
    const state = await setup(true)
    await act(async () => { compactButton('Ask').click(); state.event({ ...wire, dictation: 'error', error }) })
    expectIconOnlyRow()
    const actionable = error.startsWith('Microphone')
    expect(container!.querySelector('[role="alert"]')?.textContent ?? null).toBe(actionable ? error : null)
    expect(compactButton('Ask').querySelector('svg')?.getAttribute('data-icon-state')).toBe(actionable ? 'error' : 'ready')
    if (actionable) {
      expect(compactButton('Ask').title).toContain(error)
      expect(container!.querySelector('[role="alert"]')?.classList.contains('sr-only')).toBe(true)
    }
    expect(state.onSubmit).not.toHaveBeenCalled()
  })
  it('shows distinct non-color speaker marks for disabled, enabled, and speaking preferences', async () => {
    const state = await setup(true, { ttsAvailable: true })
    const marks: string[] = []
    for (const next of [{ ttsEnabled: false, ttsPhase: 'idle' }, { ttsEnabled: true, ttsPhase: 'idle' }, { ttsEnabled: true, ttsPhase: 'speaking' }]) {
      if (next.ttsEnabled === false) await act(async () => { compactButton('Ask').click() })
      await act(async () => { state.event({ ...wire, ttsAvailable: true, ...next }) })
      const speaker = compactButton('Speak replies')
      expect(speaker.getAttribute('aria-pressed')).toBe(String(next.ttsEnabled))
      marks.push(speaker.querySelector('[data-state-mark] path')!.getAttribute('d')!)
      expectIconOnlyRow()
    }
    expect(new Set(marks).size).toBe(3)
  })
  it('uses different SVG mark shapes for all voice phases, not color alone', async () => {
    const state = await setup(true)
    await act(async () => { compactButton('Ask').click() })
    const marks: string[] = []
    for (const phase of [
      { dictation: 'checking' }, { dictation: 'listening' }, { dictation: 'finishing' },
      { dictation: 'idle', loop: 'generating' }, { loop: 'speaking' }, { loop: 'between_turns' },
      { loop: 'idle' }, { available: false }, { dictation: 'error', error: 'Synthetic actionable failure' },
    ]) {
      await act(async () => { state.event({ ...wire, ...phase }) })
      marks.push(container!.querySelector('[data-state-mark] path')!.getAttribute('d')!)
      expectIconOnlyRow()
    }
    expect(new Set(marks).size).toBe(marks.length)
  })
  it.each([
    ['Speak replies', { ttsAvailable: true }, 'set_tts_enabled', 'Synthetic speech preference failure'],
  ])('exposes failure of %s through icon, description, title and hidden alert', async (label, status, command, error) => {
    const state = await setup(true, status as Record<string, unknown>)
    state.native.mockImplementation(async (name) => {
      if (name === command) throw new Error('Synthetic speech preference failure')
      if (name === 'assistant_voice_status') return { ...wire, ...status as Record<string, unknown> }
      return null
    })
    await act(async () => { compactButton(label as string).click() })
    expectIconOnlyRow()
    expect(container!.querySelector('[role="alert"]')?.textContent).toBe(error)
    const button = compactButton(label as string)
    expect(button.title).toContain(error)
    expect(document.getElementById(button.getAttribute('aria-describedby')!)?.textContent).toContain(error)
    expect(button.querySelector('svg')?.getAttribute('data-icon-state')).toBe('error')
    expect(state.onSubmit).not.toHaveBeenCalled()
  })
  it('keeps generation and speech availability gates without blocking enabled speech from being turned off', async () => {
    const state = await setup(true, { ttsEnabled: true, ttsAvailable: false }, { generationAvailable: false, disabled: true })
    expect(compactButton('Ask').disabled).toBe(true)
    expect(compactButton('Talk').disabled).toBe(true)
    expect(compactButton('Talk').querySelector('svg')?.getAttribute('data-icon-state')).toBe('unavailable')
    expect(compactButton('Talk').title).toContain('requires an available model')
    expect(compactButton('Speak replies').disabled).toBe(false)
    await act(async () => { compactButton('Speak replies').click() })
    expect(state.native).toHaveBeenCalledWith('set_tts_enabled', { enabled: false })
    expectIconOnlyRow()
  })
  it('keeps conversation start and Stop semantics in the second icon slot', async () => {
    const state = await setup(true)
    await act(async () => { compactButton('Talk').click() })
    expect(state.native).toHaveBeenCalledWith('start_assistant_voice', expect.objectContaining({ loopMode: true }))
    expect(compactButton('Stop voice').getAttribute('aria-pressed')).toBe('true')
    await act(async () => { compactButton('Stop voice').click() })
    expect(state.native).toHaveBeenCalledWith('stop_assistant_voice', undefined)
    expect(compactButton('Talk').getAttribute('aria-pressed')).toBe('false')
    expectIconOnlyRow()
  })
  it.each(['pending', 'unavailable'] as const)('keeps initial readiness %s accessible without a visible status row', async (outcome) => {
    let resolve: (value: unknown) => void = () => undefined
    let reject: (reason: Error) => void = () => undefined
    window.__TAURI_INTERNALS__ = { invoke: async (name) => name === 'assistant_voice_status' ? new Promise((yes, no) => { resolve = yes; reject = no }) : null }
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<AndroidVoiceControls draft="" disabled={false} generationAvailable compact stopRef={{ current: null }} onDraft={vi.fn()} onSubmit={vi.fn()} onActivity={vi.fn()} onSettled={vi.fn()} modelControl={<button aria-label="Model"><ControlIcon kind="model" /></button>} />) })
    expectIconOnlyRow()
    expect(compactButton('Ask').disabled).toBe(true)
    expect(compactButton('Ask').querySelector('svg')?.getAttribute('data-icon-state')).toBe('busy')
    expect(compactButton('Ask').title).toContain('Checking voice availability')
    await act(async () => { if (outcome === 'pending') resolve(wire); else reject(new Error('Synthetic unavailable speech status')) })
    expectIconOnlyRow()
    expect(compactButton('Ask').querySelector('svg')?.getAttribute('data-icon-state')).toBe(outcome === 'pending' ? 'ready' : 'error')
    if (outcome === 'unavailable') expect(container!.querySelector('[role="alert"]')?.textContent).toBe('Native speech status is unavailable')
  })
  it('marks a pending start as busy and lets the Stop icon retire it without late activation', async () => {
    const state = await setup(true)
    let resolveStart: (value: unknown) => void = () => undefined
    state.native.mockImplementation(async (name) => name === 'start_assistant_voice' ? new Promise((resolve) => { resolveStart = resolve }) : null)
    await act(async () => { compactButton('Ask').click() })
    expectIconOnlyRow()
    expect(compactButton('Ask').disabled).toBe(true)
    expect(compactButton('Ask').querySelector('svg')?.getAttribute('data-icon-state')).toBe('busy')
    expect(container!.querySelector('[role="status"]')?.textContent).toBe('Starting voice…')
    expect(container!.querySelector('[role="status"]')?.classList.contains('sr-only')).toBe(true)
    expect(compactButton('Stop voice').disabled).toBe(false)
    await act(async () => { compactButton('Stop voice').click(); resolveStart({ ...wire, dictation: 'listening' }) })
    expect(compactButton('Talk')).not.toBeNull()
    expect(compactButton('Ask').getAttribute('aria-pressed')).toBe('false')
    expect(state.onSubmit).not.toHaveBeenCalled()
  })
})

describe('Android native voice controls', () => {
  it('opens unverified shortcut navigation visibly but requires an app Ask tap before microphone admission', async () => {
    vi.useFakeTimers()
    let confirmation = true
    const native = vi.fn(async (name: string) => {
      if (name === 'assistant_voice_status') return wire
      if (name === 'consume_new_voice_shortcut') { const required = confirmation; confirmation = false; return { token: null, error: null, confirmationRequired: required } }
      if (name === 'start_assistant_voice') return { ...wire, mode: 'ask', loop: 'listening', dictation: 'listening' }
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke: native, createChannel: () => ({}) }
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<AndroidVoiceControls compact draft="Synthetic current draft" disabled={false} generationAvailable shortcutReady stopRef={{ current: null }} onDraft={vi.fn()} onSubmit={vi.fn()} onActivity={vi.fn()} onSettled={vi.fn()} onBeforeStart={async () => 'Synthetic current draft'} />) })
    await act(async () => { await vi.advanceTimersByTimeAsync(0) })
    expect(native.mock.calls.some(([name]) => name === 'start_assistant_voice')).toBe(false)
    const message = container.querySelector('[data-voice-confirmation]')!
    expect(message.textContent).toContain('Tap Ask')
    expect(message.classList.contains('sr-only')).toBe(false)
    await act(async () => { window.dispatchEvent(new Event('focus')); await vi.advanceTimersByTimeAsync(1000) })
    expect(native.mock.calls.some(([name]) => name === 'start_assistant_voice')).toBe(false)
    await act(async () => { compactButton('Ask').click() })
    expect(native.mock.calls.filter(([name]) => name === 'start_assistant_voice')).toHaveLength(1)
    expect(native).toHaveBeenCalledWith('start_assistant_voice', expect.objectContaining({ completionPolicy: 'single_turn', draft: 'Synthetic current draft' }))
    expect(container.querySelector('[data-voice-confirmation]')).toBeNull()
  })
  it('passes a native-minted selected-head owner through submission without rebinding it to a UI snapshot', async () => {
    const state = await setup(true)
    await act(async () => { compactButton('Ask').click() })
    const context = { sessionId: '11111111-1111-4111-8111-111111111111:1', selectedHead: '11111111-1111-4111-8111-111111111111:2' }
    await act(async () => { state.event({ kind: 'submit', requestId: 'synthetic-owned-submit', text: 'Synthetic final question', context }) })
    expect(state.onSubmit).toHaveBeenCalledWith('synthetic-owned-submit', 'Synthetic final question', context)
  })
  it.each(['ready', 'stop', 'background'] as const)('binds current draft after admission without preparing a new chat: %s', async (outcome) => {
    let admit: (draft: string) => void = () => undefined
    const before = vi.fn(() => new Promise<string>((resolve) => { admit = resolve }))
    const legacyBefore = vi.fn(async () => 'Synthetic legacy draft')
    const legacyPrepare = vi.fn(async () => undefined)
    const onActivity = vi.fn()
    const native = vi.fn(async (name: string) => name === 'assistant_voice_status' ? wire : name === 'start_assistant_voice' ? { ...wire, mode: 'ask', loop: 'listening', dictation: 'listening' } : null)
    const stopRef = { current: null as (() => void) | null }
    window.__TAURI_INTERNALS__ = { invoke: native, createChannel: () => ({}) }
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<AndroidVoiceControls compact draft="Synthetic stale prop draft" disabled={false} generationAvailable stopRef={stopRef} onDraft={vi.fn()} onSubmit={vi.fn()} onActivity={onActivity} onSettled={vi.fn()} onBeforeStart={before} onShortcutBeforeStart={legacyBefore} onShortcutPrepare={legacyPrepare} />) })
    await act(async () => { compactButton('Ask').click() })
    expect(native.mock.calls.some(([name]) => name === 'start_assistant_voice')).toBe(false)
    await act(async () => {
      if (outcome === 'stop') stopRef.current?.()
      if (outcome === 'background') { vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('hidden'); document.dispatchEvent(new Event('visibilitychange')) }
      admit('Synthetic flushed current draft')
    })
    expect(native.mock.calls.filter(([name]) => name === 'start_assistant_voice')).toHaveLength(outcome === 'ready' ? 1 : 0)
    if (outcome === 'ready') expect(native).toHaveBeenCalledWith('start_assistant_voice', expect.objectContaining({ draft: 'Synthetic flushed current draft', completionPolicy: 'single_turn' }))
    expect(legacyBefore).not.toHaveBeenCalled(); expect(legacyPrepare).not.toHaveBeenCalled()
    expect(native.mock.calls.some(([name]) => name === 'reset_session' || name === 'prepare_new_voice_shortcut')).toBe(false)
  })
  it.each(['no_speech', 'recognition_retirement_failed'])('preserves partial drafts without auto-send and only warns for actionable failure %s', async (error) => {
    const { event, onDraft, onSubmit } = await setup()
    await act(async () => { Array.from(container!.querySelectorAll('button')).find((button) => button.textContent === 'Dictate')!.click() })
    await act(async () => { event({ kind: 'dictation', phase: 'partial', draft: 'Synthetic unconfirmed draft' }); event({ ...wire, dictation: 'error', error }) })
    expect(onDraft).toHaveBeenCalledWith('Synthetic unconfirmed draft')
    expect(onSubmit).not.toHaveBeenCalled()
    expect(container!.querySelector('[role="alert"]')?.textContent ?? null).toBe(error === 'no_speech' ? null : error)
  })
  it.each([
    { wording: 'legacy', recognitionRoute: 'Google · en-AU requested · provider-controlled · may use network · fallback',
      ttsRoute: 'Samsung preference · en-GB · installed local voice' },
    { wording: 'neutral', recognitionRoute: 'Google · requested en-AU · selected en-US · reported en-GB · local preferred · provider-controlled · may use network · fallback',
      ttsRoute: 'Samsung preference · requested en-AU · selected en-GB · installed local voice · fallback' },
  ])('publishes $wording speech diagnostics without displaying them in chat', async ({ recognitionRoute, ttsRoute }) => {
    const next = { ...wire, dictation: 'listening', readyForSpeech: true, ttsEnabled: true, ttsAvailable: true,
      recognitionRoute, ttsRoute }
    window.__TAURI_INTERNALS__ = { invoke: async () => next }
    const onStatus = vi.fn()
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<AndroidVoiceControls draft="" disabled={false} generationAvailable stopRef={{ current: null }}
      onDraft={vi.fn()} onSubmit={vi.fn()} onActivity={vi.fn()} onSettled={vi.fn()} onStatus={onStatus} />) })
    expect(container.textContent).not.toContain(next.recognitionRoute)
    expect(container.textContent).not.toContain(next.ttsRoute)
    expect(container.textContent).not.toContain(next.reason)
    expect(container.textContent).toContain('Listening…')
    expect(onStatus).toHaveBeenLastCalledWith(expect.objectContaining({ recognitionRoute, ttsRoute }), null)
  })
  it('keeps idle status failures out of chat and publishes them for Settings', async () => {
    const onStatus = vi.fn()
    window.__TAURI_INTERNALS__ = { invoke: async () => { throw new Error('Synthetic unavailable status') } }
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<AndroidVoiceControls draft="" disabled={false} generationAvailable stopRef={{ current: null }} onDraft={vi.fn()} onSubmit={vi.fn()} onActivity={vi.fn()} onSettled={vi.fn()} onStatus={onStatus} />) })
    expect(container.textContent).not.toContain('Native speech status is unavailable')
    expect(onStatus).toHaveBeenLastCalledWith(null, 'Native speech status is unavailable')
  })
  it.each([['dictation', false], ['loop', false], ['shortcut', false], ['dictation', true], ['loop', true], ['shortcut', true]] as const)('presents %s as starting until native speech readiness is known (compact=%s)', async (mode, compact) => {
    const state = await setup(compact)
    await act(async () => { container!.querySelector<HTMLButtonElement>(`button[aria-label="${compact ? 'Ask' : 'Dictate'}"]`)!.click() })
    const listening = { ...wire, mode, dictation: 'listening', loop: mode === 'loop' ? 'listening' : 'idle', shortcutPhase: mode === 'shortcut' ? 'listening' : null }
    await act(async () => { state.event(listening) })
    expect(container!.querySelector('[role="status"]')?.textContent).toBe('Starting voice…')
    await act(async () => { state.event({ ...listening, readyForSpeech: false }) })
    expect(container!.querySelector('[role="status"]')?.textContent).toBe('Starting voice…')
    await act(async () => { state.event({ ...listening, readyForSpeech: true }) })
    expect(container!.querySelector('[role="status"]')?.textContent).toBe('Listening…')
    if (compact) expectIconOnlyRow()
    await act(async () => { state.event({ ...listening, dictation: 'finishing', shortcutPhase: mode === 'shortcut' ? 'finishing' : null, readyForSpeech: false }) })
    expect(container!.querySelector('[role="status"]')?.textContent).toBe('Finishing dictation…')
    await act(async () => { state.stopRef.current?.(); state.event({ ...listening, readyForSpeech: true }) })
    expect(container!.querySelector('[role="status"]')).toBeNull()
    expect(state.onSubmit).not.toHaveBeenCalled()
  })
  it.each(['available', 'unavailable', 'background', 'unmount', 'status-error'] as const)('waits for known shortcut availability before consuming a launch: %s', async (outcome) => {
    vi.useFakeTimers()
    const token = '77777777-7777-4777-8777-777777777777'
    let resolveStatus: (value: unknown) => void = () => undefined
    let rejectStatus: (reason: unknown) => void = () => undefined
    let polls = 0
    let consumed = false
    const before = vi.fn(async () => 'Synthetic preserved draft')
    const submit = vi.fn()
    const native = vi.fn(async (name: string) => {
      if (name === 'assistant_voice_status') {
        polls += 1
        return polls === 1 ? new Promise((resolve, reject) => { resolveStatus = resolve; rejectStatus = reject }) : wire
      }
      if (name === 'consume_new_voice_shortcut') { const next = consumed ? null : token; consumed = true; return { token: next, error: null } }
      if (name === 'start_assistant_voice') return { ...wire, mode: 'shortcut', dictation: 'listening', shortcutPhase: 'listening' }
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke: native, createChannel: () => ({}) }
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<AndroidVoiceControls draft="Synthetic preserved draft" disabled={false} generationAvailable shortcutReady stopRef={{ current: null }} onDraft={vi.fn()} onSubmit={submit} onActivity={vi.fn()} onSettled={vi.fn()} onShortcutBeforeStart={before} />) })
    await act(async () => { window.dispatchEvent(new Event('voxgolem-new-voice-chat')); await vi.advanceTimersByTimeAsync(0) })
    expect(native.mock.calls.some(([name]) => name === 'consume_new_voice_shortcut')).toBe(false)
    await act(async () => {
      if (outcome === 'background') { vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('hidden'); document.dispatchEvent(new Event('visibilitychange')) }
      if (outcome === 'unmount') { root?.unmount(); root = null }
      if (outcome === 'status-error') rejectStatus(new Error('Synthetic unavailable status'))
      else resolveStatus({ ...wire, available: outcome !== 'unavailable' })
    })
    await act(async () => { await vi.advanceTimersByTimeAsync(0) })
    if (outcome === 'status-error') {
      expect(native.mock.calls.some(([name]) => name === 'consume_new_voice_shortcut')).toBe(false)
      await act(async () => { await vi.advanceTimersByTimeAsync(1000) })
      await act(async () => { await vi.advanceTimersByTimeAsync(0) })
    }
    if (outcome === 'available' || outcome === 'status-error') {
      expect(native.mock.calls.filter(([name]) => name === 'consume_new_voice_shortcut')).toHaveLength(1)
      expect(before).toHaveBeenCalledTimes(1)
      expect(native).toHaveBeenCalledWith('start_assistant_voice', expect.objectContaining({ draft: '', loopMode: false, completionPolicy: 'stop_send_new', launchToken: token }))
      await act(async () => { window.dispatchEvent(new Event('voxgolem-new-voice-chat')); await vi.advanceTimersByTimeAsync(0) })
      expect(native.mock.calls.filter(([name]) => name === 'start_assistant_voice')).toHaveLength(1)
    } else {
      expect(before).not.toHaveBeenCalled()
      expect(native.mock.calls.some(([name]) => name === 'start_assistant_voice')).toBe(false)
      if (outcome === 'unavailable') {
        expect(native).toHaveBeenCalledWith('release_new_voice_shortcut', { token })
        expect(container!.querySelector('[role="alert"]')?.textContent).toContain('native dictation is unavailable')
        await act(async () => { await vi.advanceTimersByTimeAsync(1000) })
        await act(async () => { await vi.advanceTimersByTimeAsync(0) })
        expect(native.mock.calls.some(([name]) => name === 'start_assistant_voice')).toBe(false)
      } else expect(native.mock.calls.some(([name]) => name === 'consume_new_voice_shortcut')).toBe(false)
    }
    expect(submit).not.toHaveBeenCalled()
  })
  it.each([['dictation', true], ['loop', true], ['dictation', false], ['loop', false]] as const)('quietly ends an owned %s no-match without sending partial words (%s)', async (mode, hasPartial) => {
    const message = 'recognition_error_7'
    const partial = hasPartial ? 'Synthetic partially recognized words' : ''
    const failed = { ...wire, mode, dictation: 'error', loop: mode === 'loop' ? 'error' : 'idle', error: message }
    let sessionId = ''
    let channel: (value: unknown) => void = () => undefined
    const onStatus = vi.fn()
    const onActivity = vi.fn()
    const onSubmit = vi.fn()
    const onSettled = vi.fn()
    const stopRef = { current: null as (() => void) | null }
    window.__TAURI_INTERNALS__ = {
      invoke: async (name, args) => {
        if (name === 'assistant_voice_status') return wire
        if (name === 'start_assistant_voice') { sessionId = (args as { sessionId: string }).sessionId; return { ...wire, mode, dictation: 'listening', loop: mode === 'loop' ? 'listening' : 'idle' } }
        return null
      },
      createChannel: <T,>(handler: (value: T) => void) => { channel = handler as (value: unknown) => void; return {} },
    }
    function Harness() {
      const [draft, setDraft] = useState('')
      return <><textarea aria-label="Retained voice draft" value={draft} readOnly /><AndroidVoiceControls draft={draft} disabled={false} generationAvailable stopRef={stopRef} onDraft={setDraft} onSubmit={onSubmit} onActivity={onActivity} onSettled={onSettled} onStatus={onStatus} /></>
    }
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<Harness />) })
    await act(async () => { Array.from(container!.querySelectorAll('button')).find((button) => button.textContent === (mode === 'loop' ? 'Talk' : 'Dictate'))!.click() })
    await act(async () => {
      if (hasPartial) channel({ sessionId, kind: 'dictation', phase: 'partial', draft: partial })
      if (mode === 'loop') channel({ sessionId, kind: 'loop', phase: 'error', error: message })
      channel({ ...failed, sessionId })
    })
    expect(onActivity).toHaveBeenLastCalledWith(false)
    expect(container.querySelector('[role="alert"]')).toBeNull()
    expect(container.textContent).not.toContain('No speech match')
    expect(container.querySelector<HTMLTextAreaElement>('textarea')?.value).toBe(partial)
    expect(onSubmit).not.toHaveBeenCalled()
    expect(onStatus).toHaveBeenLastCalledWith(expect.objectContaining({ error: message }), null)
    await act(async () => { stopRef.current?.() })
    expect(container.querySelector('[role="alert"]')).toBeNull()
    expect(container.querySelector<HTMLTextAreaElement>('textarea')?.value).toBe(partial)
    expect(onStatus).toHaveBeenLastCalledWith(expect.objectContaining({ error: message }), null)
  })
  it('publishes an unowned cached no-match error for Settings without an actionable chat alert', async () => {
    const message = 'recognition_error_7'
    const failed = { ...wire, mode: 'loop', dictation: 'error', loop: 'error', error: message }
    const onStatus = vi.fn()
    window.__TAURI_INTERNALS__ = { invoke: async () => failed }
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<AndroidVoiceControls draft="Synthetic retained old draft" disabled={false} generationAvailable stopRef={{ current: null }} onDraft={vi.fn()} onSubmit={vi.fn()} onActivity={vi.fn()} onSettled={vi.fn()} onStatus={onStatus} />) })
    expect(container.querySelector('[role="alert"]')).toBeNull()
    expect(container.textContent).not.toContain(message)
    expect(onStatus).toHaveBeenLastCalledWith(expect.objectContaining({ error: message }), null)
  })
  it('rejects a retired StrictMode status request without restarting polling for new callbacks', async () => {
    vi.useFakeTimers()
    const resolves: Array<(value: unknown) => void> = []
    const native = vi.fn(async (name: string) => name === 'assistant_voice_status' ? new Promise((resolve) => resolves.push(resolve)) : null)
    window.__TAURI_INTERNALS__ = { invoke: native }
    const onActivity = vi.fn()
    const stopRef = { current: null }
    const onStatus = vi.fn()
    const render = (callback: typeof onStatus) => <StrictMode><AndroidVoiceControls draft="" disabled={false} generationAvailable stopRef={stopRef} onDraft={vi.fn()} onSubmit={vi.fn()} onActivity={onActivity} onSettled={vi.fn()} onStatus={callback} /></StrictMode>
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(render(onStatus)) })
    expect(resolves).toHaveLength(1)
    await act(async () => { window.dispatchEvent(new Event('focus')); await vi.advanceTimersByTimeAsync(2000) })
    expect(resolves).toHaveLength(1)
    await act(async () => { resolves[0]!({ ...wire, reason: 'Synthetic retired status' }) })
    expect(onStatus.mock.calls.some(([status]) => status?.reason === 'Synthetic retired status')).toBe(false)
    expect(resolves).toHaveLength(2)
    await act(async () => { resolves[1]!({ ...wire, reason: 'Synthetic current status' }) })
    expect(onStatus).toHaveBeenLastCalledWith(expect.objectContaining({ reason: 'Synthetic current status' }), null)
    const replacement = vi.fn()
    const stops = native.mock.calls.filter(([name]) => name === 'stop_assistant_voice').length
    await act(async () => { root?.render(render(replacement)); await vi.advanceTimersByTimeAsync(1000) })
    expect(resolves).toHaveLength(3)
    expect(native.mock.calls.filter(([name]) => name === 'stop_assistant_voice')).toHaveLength(stops)
    await act(async () => { resolves[2]!(wire) })
    expect(replacement).toHaveBeenLastCalledWith(expect.objectContaining({ reason: wire.reason }), null)
  })
  it.each(['channel', 'stop', 'unmount'] as const)('does not publish an in-flight poll after newer %s state', async (transition) => {
    vi.useFakeTimers()
    let resolvePoll: (value: unknown) => void = () => undefined
    let channel: (value: unknown) => void = () => undefined
    let sessionId = ''
    let polls = 0
    const native = vi.fn(async (name: string, args?: unknown) => {
      if (name === 'assistant_voice_status') { polls += 1; return polls === 1 ? wire : new Promise((resolve) => { resolvePoll = resolve }) }
      if (name === 'start_assistant_voice') { sessionId = (args as { sessionId: string }).sessionId; return { ...wire, dictation: 'listening' } }
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke: native, createChannel: <T,>(handler: (value: T) => void) => { channel = handler as (value: unknown) => void; return {} } }
    const onStatus = vi.fn()
    const onActivity = vi.fn()
    const stopRef = { current: null as (() => void) | null }
    const render = (disabled: boolean) => <AndroidVoiceControls draft="" disabled={disabled} generationAvailable stopRef={stopRef} onDraft={vi.fn()} onSubmit={vi.fn()} onActivity={onActivity} onSettled={vi.fn()} onStatus={onStatus} />
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(render(false)) })
    await act(async () => { Array.from(container!.querySelectorAll('button')).find((button) => button.textContent === 'Dictate')!.click() })
    await act(async () => { root?.render(render(true)); await vi.advanceTimersByTimeAsync(1000) })
    expect(native.mock.calls.some(([name]) => name === 'stop_assistant_voice')).toBe(false)
    await act(async () => {
      if (transition === 'channel') channel({ ...wire, sessionId, dictation: 'final', recognitionRoute: 'Synthetic final route' })
      if (transition === 'stop') stopRef.current?.()
      if (transition === 'unmount') { root?.unmount(); root = null }
    })
    const calls = onStatus.mock.calls.length
    await act(async () => { resolvePoll({ ...wire, dictation: 'listening', recognitionRoute: 'Synthetic stale route' }) })
    expect(onStatus).toHaveBeenCalledTimes(calls)
    expect(onStatus.mock.calls.at(-1)?.[0]?.recognitionRoute).not.toBe('Synthetic stale route')
    if (transition === 'stop') expect(onStatus).toHaveBeenLastCalledWith(expect.objectContaining({ dictation: 'idle' }), null)
  })
  it('keeps Channel terminal status over a delayed start reply and preserves a genuine session error', async () => {
    let resolveStart: (value: unknown) => void = () => undefined
    let channel: (value: unknown) => void = () => undefined
    let sessionId = ''
    window.__TAURI_INTERNALS__ = {
      invoke: async (name, args) => {
        if (name === 'assistant_voice_status') return wire
        if (name === 'start_assistant_voice') { sessionId = (args as { sessionId: string }).sessionId; return new Promise((resolve) => { resolveStart = resolve }) }
        return null
      },
      createChannel: <T,>(handler: (value: T) => void) => { channel = handler as (value: unknown) => void; return {} },
    }
    const onStatus = vi.fn()
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<AndroidVoiceControls draft="" disabled={false} generationAvailable stopRef={{ current: null }} onDraft={vi.fn()} onSubmit={vi.fn()} onActivity={vi.fn()} onSettled={vi.fn()} onStatus={onStatus} />) })
    await act(async () => { Array.from(container!.querySelectorAll('button')).find((button) => button.textContent === 'Dictate')!.click() })
    await act(async () => { channel({ ...wire, sessionId, dictation: 'error', error: 'Microphone permission denied; enable access in system Settings' }); resolveStart({ ...wire, dictation: 'listening' }) })
    expect(onStatus).toHaveBeenLastCalledWith(expect.objectContaining({ dictation: 'error' }), null)
    expect(container.querySelector('[role="alert"]')?.textContent).toContain('Microphone permission denied')
    expect(container.textContent).not.toContain(wire.reason)
  })
  it.each(['duplicate', 'background', 'unhydrated', 'invalid-action', 'no-channel', 'invalid-start'] as const)('retires shortcut admission safely: %s', async (race) => {
    vi.useFakeTimers()
    const token = '66666666-6666-4666-8666-666666666666'
    let settle: (value: unknown) => void = () => undefined
    let first = true
    const before = vi.fn(async () => 'Synthetic preserved draft')
    const cancelled = vi.fn()
    const onActivity = vi.fn()
    const onSettled = vi.fn()
    const native = vi.fn(async (name: string) => {
      if (name === 'assistant_voice_status') return wire
      if (name === 'consume_new_voice_shortcut') { if (!first) return { token, error: null }; first = false; return new Promise((resolve) => { settle = resolve }) }
      if (name === 'start_assistant_voice') return { invalid: true }
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke: native, ...(race === 'no-channel' ? {} : { createChannel: () => ({}) }) }
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    const render = (ready: boolean) => <AndroidVoiceControls draft="Synthetic preserved draft" disabled={false} generationAvailable shortcutReady={ready} stopRef={{ current: null }} onDraft={vi.fn()} onSubmit={vi.fn()} onActivity={onActivity} onSettled={onSettled} onShortcutBeforeStart={before} onShortcutCancelled={cancelled} />
    await act(async () => { root?.render(render(true)) })
    await act(async () => { await vi.advanceTimersByTimeAsync(0) })
    await act(async () => {
      if (race === 'duplicate') { for (let count = 0; count < 9; count += 1) window.dispatchEvent(new Event('voxgolem-new-voice-chat')); await vi.advanceTimersByTimeAsync(0) }
      if (race === 'background') { vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('hidden'); document.dispatchEvent(new Event('visibilitychange')) }
      if (race === 'unhydrated') root?.render(render(false))
    })
    await act(async () => { settle({ token, error: race === 'invalid-action' ? {} : null }) })
    expect(native.mock.calls.filter(([name]) => name === 'consume_new_voice_shortcut')).toHaveLength(1)
    expect(native).toHaveBeenCalledWith('release_new_voice_shortcut', { token })
    if (race === 'invalid-start' || race === 'duplicate') { expect(before).toHaveBeenCalledTimes(1); expect(cancelled).toHaveBeenCalledWith('Synthetic preserved draft', false) }
    else expect(before).not.toHaveBeenCalled()
  })
  it.each(['busy', 'permission'] as const)('preserves existing work when shortcut admission fails: %s', async (failure) => {
    vi.useFakeTimers()
    const token = '33333333-3333-4333-8333-333333333333'
    let consumed = false
    const before = vi.fn(async () => 'Preserved synthetic draft')
    const prepare = vi.fn(async () => undefined)
    const cancelled = vi.fn()
    const submit = vi.fn()
    const native = vi.fn(async (name: string) => {
      if (name === 'assistant_voice_status') return wire
      if (name === 'consume_new_voice_shortcut') { if (consumed) return { token: null, error: null }; consumed = true; return failure === 'busy' ? { token: null, error: 'Voice chat is busy; existing work was not cancelled' } : { token, error: null } }
      if (name === 'start_assistant_voice') throw { code: 'unavailable', message: 'Microphone permission admission was cancelled' }
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke: native, createChannel: () => ({}) }
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<AndroidVoiceControls draft="Preserved synthetic draft" disabled={false} generationAvailable shortcutReady stopRef={{ current: null }} onDraft={vi.fn()} onSubmit={submit} onActivity={vi.fn()} onSettled={vi.fn()} onShortcutBeforeStart={before} onShortcutPrepare={prepare} onShortcutCancelled={cancelled} />) })
    await act(async () => { await vi.advanceTimersByTimeAsync(0) })
    expect(prepare).not.toHaveBeenCalled(); expect(submit).not.toHaveBeenCalled()
    expect(native.mock.calls.some(([name]) => name === 'reset_session' || name === 'cancel_prompt')).toBe(false)
    if (failure === 'busy') expect(before).not.toHaveBeenCalled()
    else expect(cancelled).toHaveBeenCalledWith('Preserved synthetic draft', false)
  })
  it('consumes a hydrated shortcut once and waits for confirmed new context before Stop & send submission', async () => {
    vi.useFakeTimers()
    const token = '11111111-1111-4111-8111-111111111111'
    let ready = false
    let consumed = false
    let sessionId = ''
    let channel: (value: unknown) => void = () => undefined
    let prepare: () => void = () => undefined
    const onSubmit = vi.fn()
    const onDraft = vi.fn()
    const before = vi.fn(async () => 'Preserved synthetic old draft')
    const native = vi.fn(async (name: string, args?: unknown) => {
      if (name === 'assistant_voice_status') return wire
      if (name === 'consume_new_voice_shortcut') { if (consumed) return { token: null, error: null }; consumed = true; return { token, error: null } }
      if (name === 'start_assistant_voice') { sessionId = (args as { sessionId: string }).sessionId; return { ...wire, mode: 'shortcut', dictation: 'listening', shortcutPhase: 'listening' } }
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke: native, createChannel: <T,>(handler: (value: T) => void) => { channel = handler as (value: unknown) => void; return {} } }
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    const stopRef = { current: null as (() => void) | null }
    const onActivity = vi.fn()
    const onSettled = vi.fn()
    const onPrepare = vi.fn(async (oldDraft: string) => { expect(oldDraft).toBe('Preserved synthetic old draft'); await new Promise<void>((resolve) => { prepare = () => { ready = true; resolve() } }) })
    const render = (shortcutReady: boolean) => <AndroidVoiceControls draft="Preserved synthetic old draft" disabled={false} generationAvailable shortcutReady={shortcutReady} stopRef={stopRef} onDraft={onDraft} onSubmit={onSubmit} onActivity={onActivity} onSettled={onSettled} onShortcutBeforeStart={before} onShortcutPrepare={onPrepare} />
    await act(async () => { root?.render(render(false)); await vi.advanceTimersByTimeAsync(0) })
    expect(native.mock.calls.some(([name]) => name === 'consume_new_voice_shortcut')).toBe(false)
    await act(async () => { root?.render(render(true)) })
    await act(async () => { await vi.advanceTimersByTimeAsync(0) })
    expect(before).toHaveBeenCalledTimes(1)
    expect(native).toHaveBeenCalledWith('start_assistant_voice', expect.objectContaining({ draft: '', loopMode: false, completionPolicy: 'stop_send_new', launchToken: token }))
    await act(async () => { channel({ sessionId, kind: 'shortcut_ready' }); channel({ sessionId, kind: 'dictation', phase: 'final', draft: 'Synthetic accumulated final words', utterance: '1', error: null }) })
    expect(onSubmit).not.toHaveBeenCalled()
    await act(async () => { Array.from(container!.querySelectorAll('button')).find((button) => button.textContent === 'Stop & send')!.click() })
    expect(native.mock.calls.some(([name]) => name === 'stop_send_new_voice_shortcut')).toBe(true)
    await act(async () => { channel({ sessionId, kind: 'submit', requestId: 'synthetic-shortcut-request', text: 'Synthetic accumulated final words' }) })
    expect(ready).toBe(false); expect(onSubmit).not.toHaveBeenCalled()
    await act(async () => { prepare() })
    expect(onSubmit).toHaveBeenCalledWith('synthetic-shortcut-request', 'Synthetic accumulated final words')
    await act(async () => { channel({ sessionId, kind: 'submit', requestId: 'synthetic-shortcut-request', text: 'Duplicate synthetic final' }); window.dispatchEvent(new Event('voxgolem-new-voice-chat')); await vi.advanceTimersByTimeAsync(0) })
    expect(onSubmit).toHaveBeenCalledTimes(1)
    expect(native.mock.calls.filter(([name]) => name === 'start_assistant_voice')).toHaveLength(1)
  })
  it('retains a shortcut timeout draft for manual review without execution', async () => {
    const state = await setup()
    await act(async () => { Array.from(container!.querySelectorAll('button')).find((button) => button.textContent === 'Dictate')!.click() })
    await act(async () => { state.event({ kind: 'shortcut_review', draft: 'Synthetic uncertain partial', reason: 'Final recognition timed out; review before sending' }) })
    expect(state.onDraft).toHaveBeenCalledWith('Synthetic uncertain partial')
    expect(state.onSubmit).not.toHaveBeenCalled()
    expect(container!.textContent).toContain('review before sending')
  })
  it('keeps a nonempty unprepared shortcut review staged rather than restoring the old draft', async () => {
    vi.useFakeTimers()
    let consumed = false
    let sessionId = ''
    let channel: (value: unknown) => void = () => undefined
    const cancelled = vi.fn()
    const onDraft = vi.fn()
    const onSubmit = vi.fn()
    const onActivity = vi.fn()
    const onSettled = vi.fn()
    const native = vi.fn(async (name: string, args?: unknown) => {
      if (name === 'assistant_voice_status') return wire
      if (name === 'consume_new_voice_shortcut') { const token = consumed ? null : 'cccccccc-cccc-4ccc-8ccc-cccccccccccc'; consumed = true; return { token, error: null } }
      if (name === 'start_assistant_voice') { sessionId = (args as { sessionId: string }).sessionId; return { ...wire, mode: 'shortcut', dictation: 'listening', shortcutPhase: 'listening' } }
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke: native, createChannel: <T,>(handler: (value: T) => void) => { channel = handler as (value: unknown) => void; return {} } }
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<AndroidVoiceControls draft="Synthetic old draft" disabled={false} generationAvailable shortcutReady stopRef={{ current: null }} onDraft={onDraft} onSubmit={onSubmit} onActivity={onActivity} onSettled={onSettled} onShortcutBeforeStart={async () => 'Synthetic old draft'} onShortcutCancelled={cancelled} />) })
    await act(async () => { await vi.advanceTimersByTimeAsync(0) })
    await act(async () => {
      channel({ sessionId, kind: 'shortcut_review', draft: 'Synthetic uncertain partial', reason: 'Final recognition timed out; review before sending' })
      channel({ ...wire, sessionId, mode: 'shortcut', dictation: 'idle', shortcutPhase: 'review' })
    })
    expect(onDraft).toHaveBeenCalledWith('Synthetic uncertain partial')
    expect(cancelled).not.toHaveBeenCalled()
    expect(onSubmit).not.toHaveBeenCalled()
    await act(async () => { Array.from(container!.querySelectorAll('button')).find((button) => button.textContent === 'Dictate')!.click() })
    expect(native.mock.calls.filter(([name]) => name === 'start_assistant_voice')).toHaveLength(1)
    expect(container.textContent).toContain('Review and send the new voice draft')
  })
  it('fills the composer on dictation final without sending and drops callbacks after Stop', async () => {
    const state = await setup()
    await act(async () => { Array.from(container!.querySelectorAll('button')).find((button) => button.textContent === 'Dictate')!.click() })
    await act(async () => { state.event({ kind: 'dictation', phase: 'final', draft: 'Synthetic final words', utterance: '1', error: null }) })
    expect(state.onDraft).toHaveBeenCalledWith('Synthetic final words')
    expect(state.onSubmit).not.toHaveBeenCalled()
    await act(async () => { state.stopRef.current?.(); state.event({ kind: 'dictation', phase: 'final', draft: 'Stale synthetic words', utterance: '1', error: null }) })
    expect(state.onDraft).toHaveBeenCalledTimes(1)
  })
  it('routes an explicit native loop request identity exactly once through normal prompt execution', async () => {
    const state = await setup()
    await act(async () => { Array.from(container!.querySelectorAll('button')).find((button) => button.textContent === 'Talk')!.click() })
    await act(async () => { for (let duplicate = 0; duplicate < 2; duplicate += 1) state.event({ kind: 'submit', requestId: 'synthetic-loop-request', text: 'Synthetic final words' }) })
    expect(state.onSubmit).toHaveBeenCalledTimes(1)
    expect(state.onSubmit).toHaveBeenCalledWith('synthetic-loop-request', 'Synthetic final words')
    await act(async () => { state.event(wire) })
    expect(state.onInputCancelled).not.toHaveBeenCalled()
    await act(async () => { state.stopRef.current?.(); state.event({ kind: 'submit', requestId: 'synthetic-retired-loop-request', text: 'Synthetic obsolete words' }) })
    expect(state.onInputCancelled).toHaveBeenCalledTimes(1)
    expect(state.onSubmit).toHaveBeenCalledTimes(1)
  })
  it('shows missing speech prompt when voice is tapped without speech services, and opens Play Store', async () => {
    const missingWire = { ...wire, available: false, missingSpeechServices: true, speechRequirementMessage: 'Speech services required for tests' }
    const state = await setup(true, missingWire)
    const askButton = compactButton('Ask')
    expect(askButton.disabled).toBe(false)
    await act(async () => { askButton.click() })
    expect(container!.querySelector('.speech-prompt-dialog')).not.toBeNull()
    expect(container!.querySelector('.speech-prompt-dialog__content')?.textContent).toContain('Speech services required for tests')
    const openStoreButton = Array.from(container!.querySelectorAll('button')).find((b) => b.textContent === 'Open Play Store')
    expect(openStoreButton).toBeDefined()
    await act(async () => { openStoreButton!.click() })
    expect(state.native).toHaveBeenCalledWith('open_speech_services_store', undefined)
    expect(container!.querySelector('.speech-prompt-dialog')).toBeNull()
  })
})

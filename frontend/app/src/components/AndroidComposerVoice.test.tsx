import { act } from 'react'
import { createRoot } from 'react-dom/client'
import type { Root } from 'react-dom/client'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { AndroidVoiceControlsProvider, AndroidComposerVoiceControl, AndroidVoiceSecondaryControls } from './AndroidVoiceControls'
import { PhoneActionOverflow } from './PhoneActionOverflow'
import type { AndroidVoiceAdapterView } from '../lib/useAndroidVoiceAdapter'
import { parseAndroidVoiceStatus } from '../lib/androidVoice'

let root: Root | undefined
let container: HTMLDivElement
afterEach(() => { act(() => root?.unmount()); container?.remove(); vi.useRealTimers(); vi.restoreAllMocks() })
function voice(status: Record<string, unknown> = {}): AndroidVoiceAdapterView {
  return { status: parseAndroidVoiceStatus({ kind: 'voice_status', available: true, reason: '', mode: 'dictation', dictation: 'idle', loop: 'idle', ttsEnabled: false, ttsAvailable: true, ttsPhase: 'idle', error: null, ...status }),
    pending: false, active: false, error: null, ownedSessionError: null, statusError: null, confirmationRequired: false,
    start: vi.fn(async () => undefined), stop: vi.fn(), finishDictation: vi.fn(), finishShortcut: vi.fn(), toggleSpeech: vi.fn() }
}
function render(value = voice(), generationAvailable = true, disabled = false, interactionBlocked = false) {
  if (!root) { container = document.createElement('div'); document.body.append(container); root = createRoot(container) }
  act(() => root!.render(<AndroidVoiceControlsProvider voice={value} disabled={disabled} interactionBlocked={interactionBlocked} generationAvailable={generationAvailable}>
    <AndroidComposerVoiceControl />
    <PhoneActionOverflow><AndroidVoiceSecondaryControls /></PhoneActionOverflow>
  </AndroidVoiceControlsProvider>))
  return value
}
function button(label: string) { return container.querySelector<HTMLButtonElement>(`[aria-label="${label}"]`)! }

describe('composer voice intent', () => {
  afterEach(() => { root = undefined })
  it('offers one primary voice action, not Talk or Dictate into draft', () => {
    render()
    expect(button('Ask')).not.toBeNull()
    expect(container.querySelector('[aria-label="Dictate into draft"]')).toBeNull()
    expect(container.querySelector('[aria-label="Talk"]')).toBeNull()
  })
  it('starts single turn directly on click of Ask', () => {
    const value = render()
    act(() => { button('Ask').click() })
    expect(value.start).toHaveBeenCalledExactlyOnceWith(false, undefined, 'single_turn')
  })
  it('does not admit speech from right click or the context menu', () => {
    const value = render()
    act(() => {
      const btn = button('Ask')
      const event = new MouseEvent('click', { bubbles: true, button: 2 })
      Object.defineProperty(event, 'button', { value: 2 })
      btn.dispatchEvent(event)
    })
    expect(value.start).not.toHaveBeenCalled()
  })
  it('does not turn unavailable Ask into Dictate, setting status notice when generationAvailable is false', () => {
    const value = render(voice(), false)
    act(() => button('Ask').click())
    expect(value.start).not.toHaveBeenCalled()
    expect(container.textContent).toContain('Ask requires an available model')
    expect(container.querySelector('[role="status"]')?.textContent).toContain('Ask requires an available model')
  })
  it.each(['ask', 'dictation', 'loop'])('keeps pending %s cancellation operable despite idle-start disabling', (mode) => {
    const value = voice({ mode, dictation: 'checking' }); value.active = true; value.pending = true
    render(value, false, true)
    act(() => button('Stop voice').click())
    expect(value.stop).toHaveBeenCalledOnce()
    expect(value.finishDictation).not.toHaveBeenCalled()
  })
  it.each(['dictation', 'ask', 'loop'])('finishes %s listening through the existing final-result bridge', (mode) => {
    const value = voice({ mode, dictation: 'listening', loop: mode === 'dictation' ? 'idle' : 'listening', readyForSpeech: true }); value.active = true
    render(value)
    const finish = button('Finish dictation')
    expect(finish.querySelector('svg')?.getAttribute('data-icon-kind')).toBe('stop')
    act(() => finish.click())
    expect(value.finishDictation).toHaveBeenCalledOnce()
    expect(value.stop).not.toHaveBeenCalled()
    expect(container.querySelector('.shell__overflow-actions [aria-label="Cancel voice"] svg')).toBeNull()
  })
  it('keeps checking and finishing non-square, with cancellation in composer overflow only', () => {
    const value = voice({ dictation: 'checking' }); value.active = true
    render(value)
    expect(button('Stop voice').querySelector('svg')?.getAttribute('data-icon-kind')).toBe('mic')
    expect(button('Stop voice').querySelector('svg')?.getAttribute('data-icon-state')).toBe('busy')
    expect(container.querySelectorAll('[aria-label="Cancel voice"]')).toHaveLength(1)
    const disclosure = container.querySelector<HTMLElement>('summary[aria-label="More actions"]')!
    expect(disclosure.getAttribute('role')).toBe('button')
    const actions = document.getElementById(disclosure.getAttribute('aria-controls')!)!
    expect(actions.querySelector('[aria-label="Cancel voice"]')).not.toBeNull()
    act(() => disclosure.click())
    expect(disclosure.closest('details')?.open).toBe(true)
    act(() => actions.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })))
    expect(disclosure.closest('details')?.open).toBe(false)
    expect(document.activeElement).toBe(disclosure)
  })
  it('waits for native speech readiness before showing the listening square', () => {
    const value = voice({ mode: 'ask', dictation: 'listening', loop: 'listening', readyForSpeech: false }); value.active = true
    render(value)
    expect(button('Stop voice').querySelector('svg')?.getAttribute('data-icon-kind')).toBe('mic')
    expect(button('Stop voice').querySelector('svg')?.getAttribute('data-icon-state')).toBe('busy')
    expect(button('Finish dictation')).toBeNull()
  })
  it('finishes manual listening, disables repeat finish and retains explicit cancellation', () => {
    const value = voice({ mode: 'dictation', dictation: 'listening', readyForSpeech: true }); value.active = true
    render(value)
    act(() => button('Finish dictation').click())
    expect(value.finishDictation).toHaveBeenCalledOnce()
    render({ ...value, status: { ...value.status!, dictation: 'finishing' } })
    expect(button('Finishing dictation').disabled).toBe(true)
    act(() => button('Cancel voice').click())
    expect(value.stop).toHaveBeenCalledOnce()
  })
})

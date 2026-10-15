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
function pointer(type: string, x = 20, y = 20, id = 1, mouseButton = 0, label = 'Ask') {
  const event = new MouseEvent(type, { bubbles: true, cancelable: true, clientX: x, clientY: y, button: mouseButton })
  Object.defineProperties(event, { pointerId: { value: id }, isPrimary: { value: id === 1 }, pointerType: { value: 'touch' } })
  button(label).dispatchEvent(event)
}
describe('composer voice intent', () => {
  afterEach(() => { root = undefined })
  it('offers one primary voice action and an explicit non-timed draft-only action, not Talk', () => {
    render()
    expect(button('Ask')).not.toBeNull()
    expect(button('Dictate into draft')).not.toBeNull()
    expect(button('Talk')).toBeNull()
  })
  it('arms on hold and starts manual only on release, consuming only the trailing click', () => {
    vi.useFakeTimers(); const value = render()
    vi.spyOn(button('Ask'), 'getBoundingClientRect').mockReturnValue({ left: 0, right: 48, top: 0, bottom: 48 } as DOMRect)
    act(() => { pointer('pointerdown'); vi.advanceTimersByTime(380) })
    expect(value.start).not.toHaveBeenCalled()
    expect(container.textContent).toContain('Release to dictate')
    act(() => { pointer('pointerup'); button('Ask').dispatchEvent(new MouseEvent('click', { bubbles: true, detail: 1 })) })
    expect(value.start).toHaveBeenCalledExactlyOnceWith(false, undefined, 'manual')
    act(() => { button('Ask').click() })
    expect(value.start).toHaveBeenLastCalledWith(false, undefined, 'single_turn')
  })
  it.each(['pointercancel', 'lostpointercapture', 'pointerleave', 'move'])('cancels held intent on %s without Ask or Dictate', (event) => {
    vi.useFakeTimers(); const value = render()
    act(() => { pointer('pointerdown'); vi.advanceTimersByTime(380); pointer(event === 'move' ? 'pointermove' : event, 50, 20); pointer('pointerup', 50, 20); button('Ask').dispatchEvent(new MouseEvent('click', { bubbles: true, detail: 1 })) })
    expect(value.start).not.toHaveBeenCalled()
  })
  it.each(['dictation', 'ask'] as const)('accepts a fresh primary tap in active %s when the hold has no trailing click', (mode) => {
    vi.useFakeTimers(); const value = render()
    vi.spyOn(button('Ask'), 'getBoundingClientRect').mockReturnValue({ left: 0, right: 48, top: 0, bottom: 48 } as DOMRect)
    act(() => { pointer('pointerdown'); vi.advanceTimersByTime(400); pointer('pointerup') })
    expect(value.start).toHaveBeenCalledExactlyOnceWith(false, undefined, 'manual')
    render({ ...value, active: true, status: { ...value.status!, mode, dictation: mode === 'dictation' ? 'listening' : 'idle' } })
    const label = mode === 'dictation' ? 'Finish dictation' : 'Stop voice'
    act(() => {
      pointer('pointerdown', 20, 20, 1, 0, label)
      pointer('pointerup', 20, 20, 1, 0, label)
      button(label).dispatchEvent(new MouseEvent('click', { bubbles: true, detail: 1 }))
    })
    expect(mode === 'dictation' ? value.finishDictation : value.stop).toHaveBeenCalledOnce()
    expect(value.start).toHaveBeenCalledTimes(1)
  })
  it.each([
    ['dictation', 'pointerleave'], ['dictation', 'pointercancel'],
    ['ask', 'pointerleave'], ['ask', 'pointercancel'],
  ] as const)('retires an idle %s gesture after %s without pointerup before a fresh active tap', (mode, cancellation) => {
    vi.useFakeTimers(); const value = render()
    act(() => { pointer('pointerdown'); vi.advanceTimersByTime(100); pointer(cancellation) })
    expect(value.start).not.toHaveBeenCalled()
    if (mode === 'dictation') {
      act(() => button('Dictate into draft').click())
      expect(value.start).toHaveBeenCalledExactlyOnceWith(false, undefined, 'manual')
    }
    render({ ...value, active: true, status: { ...value.status!, mode, dictation: mode === 'dictation' ? 'listening' : 'idle' } })
    const label = mode === 'dictation' ? 'Finish dictation' : 'Stop voice'
    act(() => {
      pointer('pointerdown', 20, 20, 1, 0, label)
      vi.advanceTimersByTime(400)
      pointer('pointerup', 20, 20, 1, 0, label)
      button(label).dispatchEvent(new MouseEvent('click', { bubbles: true, detail: 1 }))
    })
    expect(mode === 'dictation' ? value.finishDictation : value.stop).toHaveBeenCalledOnce()
    expect(value.start).toHaveBeenCalledTimes(mode === 'dictation' ? 1 : 0)
    expect(container.textContent).not.toContain('Release to dictate')
  })
  it.each(['secondary pointer', 'right button'])('retains hold click suppression after a %s down', (input) => {
    vi.useFakeTimers(); const value = render()
    vi.spyOn(button('Ask'), 'getBoundingClientRect').mockReturnValue({ left: 0, right: 48, top: 0, bottom: 48 } as DOMRect)
    act(() => {
      pointer('pointerdown'); vi.advanceTimersByTime(400); pointer('pointerup')
      pointer('pointerdown', 20, 20, input === 'secondary pointer' ? 2 : 1, input === 'right button' ? 2 : 0)
      button('Ask').dispatchEvent(new MouseEvent('click', { bubbles: true, detail: 1 }))
    })
    expect(value.start).toHaveBeenCalledExactlyOnceWith(false, undefined, 'manual')
  })
  it('does not turn unavailable Ask into Dictate, but the explicit draft action stays usable', () => {
    const value = render(voice(), false)
    act(() => button('Ask').click())
    expect(value.start).not.toHaveBeenCalled()
    expect(container.textContent).toContain('Ask requires an available model')
    act(() => button('Dictate into draft').click())
    expect(value.start).toHaveBeenCalledExactlyOnceWith(false, undefined, 'manual')
  })
  it('rejects a held release outside the target even without an intervening move', () => {
    vi.useFakeTimers(); const value = render()
    vi.spyOn(button('Ask'), 'getBoundingClientRect').mockReturnValue({ left: 0, right: 48, top: 0, bottom: 48 } as DOMRect)
    act(() => { pointer('pointerdown'); vi.advanceTimersByTime(380); pointer('pointerup', 60, 20); button('Ask').dispatchEvent(new MouseEvent('click', { bubbles: true, detail: 1 })) })
    expect(value.start).not.toHaveBeenCalled()
    expect(container.textContent).not.toContain('Release to dictate')
  })
  it('starts a short primary tap exactly once through its trailing click', () => {
    vi.useFakeTimers(); const value = render()
    vi.spyOn(button('Ask'), 'getBoundingClientRect').mockReturnValue({ left: 0, right: 48, top: 0, bottom: 48 } as DOMRect)
    act(() => { pointer('pointerdown'); vi.advanceTimersByTime(100); pointer('pointerup'); button('Ask').dispatchEvent(new MouseEvent('click', { bubbles: true, detail: 1 })); vi.advanceTimersByTime(500) })
    expect(value.start).toHaveBeenCalledExactlyOnceWith(false, undefined, 'single_turn')
  })
  it('ignores a secondary pointer without replacing or duplicating the primary hold', () => {
    vi.useFakeTimers(); const value = render()
    vi.spyOn(button('Ask'), 'getBoundingClientRect').mockReturnValue({ left: 0, right: 48, top: 0, bottom: 48 } as DOMRect)
    act(() => { pointer('pointerdown', 20, 20, 2); vi.advanceTimersByTime(400); pointer('pointerup', 20, 20, 2) })
    expect(value.start).not.toHaveBeenCalled()
    expect(container.textContent).not.toContain('Release to dictate')
    act(() => { pointer('pointerdown'); pointer('pointerdown', 20, 20, 2); vi.advanceTimersByTime(380); pointer('pointerup', 20, 20, 2) })
    expect(value.start).not.toHaveBeenCalled()
    act(() => { pointer('pointerup'); button('Ask').dispatchEvent(new MouseEvent('click', { bubbles: true, detail: 1 })) })
    expect(value.start).toHaveBeenCalledExactlyOnceWith(false, undefined, 'manual')
  })
  it('does not admit speech from right click or the context menu', () => {
    vi.useFakeTimers(); const value = render()
    act(() => { pointer('pointerdown', 20, 20, 1, 2); vi.advanceTimersByTime(500); button('Ask').dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, button: 2 })); pointer('pointerup', 20, 20, 1, 2) })
    expect(value.start).not.toHaveBeenCalled()
    expect(container.textContent).not.toContain('Release to dictate')
  })
  it.each(['disabled', 'modal'] as const)('retires a held intent when %s blocks input, without suppressing a fresh assistive click', (blocker) => {
    vi.useFakeTimers(); const value = render()
    vi.spyOn(button('Ask'), 'getBoundingClientRect').mockReturnValue({ left: 0, right: 48, top: 0, bottom: 48 } as DOMRect)
    act(() => { pointer('pointerdown'); vi.advanceTimersByTime(380) })
    expect(container.textContent).toContain('Release to dictate')
    render(value, true, blocker === 'disabled', blocker === 'modal')
    expect(container.textContent).not.toContain('Release to dictate')
    render(value)
    act(() => { pointer('pointerup'); button('Ask').dispatchEvent(new MouseEvent('click', { bubbles: true, detail: 1 })); vi.advanceTimersByTime(500) })
    expect(value.start).not.toHaveBeenCalled()
    act(() => button('Ask').click())
    expect(value.start).toHaveBeenCalledExactlyOnceWith(false, undefined, 'single_turn')
  })
  it('does not start after unmount before the hold threshold', () => {
    vi.useFakeTimers(); const value = render()
    act(() => { pointer('pointerdown'); vi.advanceTimersByTime(100); root!.unmount(); root = undefined; vi.advanceTimersByTime(1000) })
    expect(value.start).not.toHaveBeenCalled()
  })
  it.each(['visibility', 'blur'] as const)('cancels held intent on %s loss', (loss) => {
    vi.useFakeTimers(); const value = render()
    vi.spyOn(button('Ask'), 'getBoundingClientRect').mockReturnValue({ left: 0, right: 48, top: 0, bottom: 48 } as DOMRect)
    act(() => { button('Ask').focus(); pointer('pointerdown'); vi.advanceTimersByTime(380) })
    expect(container.textContent).toContain('Release to dictate')
    act(() => {
      if (loss === 'visibility') {
        vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('hidden')
        document.dispatchEvent(new Event('visibilitychange'))
      } else button('Ask').blur()
      pointer('pointerup'); button('Ask').dispatchEvent(new MouseEvent('click', { bubbles: true, detail: 1 }))
    })
    expect(value.start).not.toHaveBeenCalled()
    expect(container.textContent).not.toContain('Release to dictate')
  })
  it.each(['ask', 'dictation', 'loop'])('keeps pending %s cancellation operable despite idle-start disabling', (mode) => {
    const value = voice({ mode, dictation: 'checking' }); value.active = true; value.pending = true
    render(value, false, true)
    act(() => button('Stop voice').click())
    expect(value.stop).toHaveBeenCalledOnce()
    expect(value.finishDictation).not.toHaveBeenCalled()
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

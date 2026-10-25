import { act } from 'react'
import { createRoot } from 'react-dom/client'
import type { Root } from 'react-dom/client'
import { afterEach, describe, expect, it, vi } from 'vitest'
import App from './App'
import { FrontendSession } from './FrontendSession'
import { sessionAuthority } from './SessionAuthority'
import { parseUnsavedResult } from './lib/appSnapshot'
import { appSnapshot, canonicalTurn, defaultAssistantSettings, readyStartupState, savedOutcome, durableId } from './test/sessionFixtures'

let root: Root | null = null
let container: HTMLElement | null = null
afterEach(() => { act(() => root?.unmount()); container?.remove(); delete window.__TAURI_INTERNALS__; vi.restoreAllMocks(); vi.useRealTimers() })

function fixture(recovered = false, completion = false) {
  let unsaved: Record<string, unknown> | null = recovered ? projection('synthetic-recovered') : null
  let resetFails = false
  let selected = durableId(1)
  const saved = canonicalTurn('11', '12', 'Synthetic saved question', 'Synthetic saved answer', 'synthetic-saved', 'text', 'instant', selected)
  const snapshot = () => appSnapshot(selected === durableId(1) ? [saved] : [], { completion }, { session_id: selected, unsaved_result: unsaved })
  const listeners = new Map<string, (event: { payload: unknown }) => void>()
  const invoke = vi.fn(async (command: string, args?: unknown): Promise<unknown> => {
    if (command === 'get_app_snapshot') return snapshot()
    if (command === 'get_startup_state') return readyStartupState()
    if (command === 'get_assistant_settings') return { ...defaultAssistantSettings(), completion }
    if (command === 'submit_prompt') {
      const request = args as { requestId: string }
      unsaved = projection(request.requestId)
      return { kind: 'unsaved', save_disposition: 'unsaved', unsaved_result: unsaved }
    }
    if (command === 'reset_session') {
      if (unsaved !== null) throw new Error('Synthetic unresolved answer blocks generic reset')
      if (resetFails) throw new Error('Synthetic fresh context failure')
      selected = durableId(2)
      return snapshot()
    }
    if (command === 'discard_unsaved_and_new_chat') {
      const target = (args as { executionId?: unknown } | undefined)?.executionId
      if (unsaved === null || JSON.stringify(target) !== JSON.stringify(unsaved['execution_id'])) throw new Error('Synthetic stale discard target')
      if (resetFails) throw new Error('Synthetic fresh context failure')
      selected = durableId(2); unsaved = null
      return snapshot()
    }
    return null
  })
  window.__TAURI_INTERNALS__ = { invoke, createChannel: <T,>(send: (message: T) => void) => ({ send }), listen: async (name, callback) => { listeners.set(name, callback); return () => { listeners.delete(name) } } }
  return { invoke, failReset: (value: boolean) => { resetFails = value }, replaceUnsaved: (generation: string) => { unsaved = { ...projection('synthetic-newer'), execution_id: { session_id: selected, generation: durableId(generation) } }; return unsaved }, completion: (payload: unknown) => listeners.get('completion-event')?.({ payload }) }
}
function projection(requestId: string) {
  return { request_id: requestId, execution_id: { session_id: durableId(1), generation: durableId(20) }, prompt: 'Synthetic unsaved question', answer: { kind: 'text', text: 'Synthetic unsaved answer' }, sources: [], final_stage: 'instant', reason: 'storage_error' }
}
async function mount(session = new FrontendSession()) {
  container = document.createElement('div'); document.body.append(container); root = createRoot(container)
  await act(async () => { root?.render(<App session={session} />) })
  return { element: container, authority: sessionAuthority(session) }
}
async function draft(element: HTMLElement, value: string) {
  const composer = element.querySelector('textarea')!
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')!.set!.call(composer, value)
    composer.dispatchEvent(new Event('input', { bubbles: true }))
  })
}

describe('process-owned unsaved answer presentation', () => {
  it.each(['text', 'refusal'] as const)('keeps a native-valid BOM %s unsaved result copyable until matching explicit discard', async (kind) => {
    vi.useFakeTimers()
    const native = fixture()
    const original = native.invoke.getMockImplementation()!
    native.invoke.mockImplementation(async (command, args) => {
      const result = await original(command, args)
      if (command === 'submit_prompt') {
        const terminal = result as { unsaved_result: { answer: { kind: string; text: string } } }
        terminal.unsaved_result.answer = { kind, text: '\uFEFF' }
      }
      return result
    })
    const writeText = vi.fn().mockResolvedValue(undefined)
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } })
    const { element, authority } = await mount()
    await draft(element, 'Synthetic BOM answer request')
    await act(async () => { element.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    expect(element.querySelector('[aria-label="Answer not saved"]')).not.toBeNull()
    expect(element.querySelector('[aria-label="Unsaved answer"] .answer-stage__answer')?.textContent).toBe('\uFEFF')
    expect(element.textContent).not.toContain('No response was returned.')
    await act(async () => { element.querySelector<HTMLButtonElement>('[aria-label="Copy unsaved response"]')!.click() })
    expect(writeText).toHaveBeenCalledWith('\uFEFF')
    await draft(element, 'Synthetic blocked follow-up')
    expect(element.querySelector<HTMLButtonElement>('button[type="submit"]')!.disabled).toBe(true)
    expect(authority.fieldRef('unsavedResult').current?.answer.text).toBe('\uFEFF')
    await act(async () => { await vi.advanceTimersByTimeAsync(5_000) })
    expect(element.querySelector('[aria-label="Answer not saved"]')).not.toBeNull()
    expect(element.querySelector<HTMLButtonElement>('button[type="submit"]')!.disabled).toBe(true)
    await act(async () => { element.querySelector<HTMLButtonElement>('[aria-label="New Chat"]')!.click() })
    expect(element.querySelector('[aria-label="Unsaved answer"]')).toBeNull()
    expect(native.invoke.mock.calls.filter(([command]) => command === 'discard_unsaved_and_new_chat')).toEqual([['discard_unsaved_and_new_chat', { executionId: { session_id: durableId(1), generation: durableId(20) } }]])
  })
  it('keeps ordinary New Chat on generic reset when no unsaved answer exists', async () => {
    const native = fixture()
    const { element } = await mount()
    await act(async () => { element.querySelector<HTMLButtonElement>('[aria-label="New Chat"]')!.click() })
    expect(native.invoke.mock.calls.filter(([command]) => command === 'reset_session')).toHaveLength(1)
    expect(native.invoke.mock.calls.some(([command]) => command === 'discard_unsaved_and_new_chat')).toBe(false)
  })
  it('captures the user-intent target before deferred dispatch and preserves a newer output on stale-target failure', async () => {
    const native = fixture(true)
    const { element, authority } = await mount()
    await draft(element, 'Synthetic preserved draft')
    let release!: () => void
    authority.selection.draftWrites.current = new Promise<void>((resolve) => { release = resolve })
    await act(async () => { element.querySelector<HTMLButtonElement>('[aria-label="New Chat"]')!.click() })
    expect(native.invoke.mock.calls.some(([command]) => command === 'discard_unsaved_and_new_chat')).toBe(false)
    const newer = parseUnsavedResult(native.replaceUnsaved('21'))
    await act(async () => { authority.fieldState('unsavedResult')[1](newer); release() })
    expect(native.invoke.mock.calls.filter(([command]) => command === 'discard_unsaved_and_new_chat')).toEqual([['discard_unsaved_and_new_chat', { executionId: { session_id: durableId(1), generation: durableId(20) } }]])
    expect(native.invoke.mock.calls.some(([command]) => command === 'reset_session' || command === 'submit_prompt')).toBe(false)
    expect(authority.fieldRef('unsavedResult').current).toBe(newer)
    expect(authority.selection.canonicalSession.current).toBe(durableId(1))
    expect(element.querySelector('textarea')!.value).toBe('Synthetic preserved draft')
    expect(element.querySelector<HTMLButtonElement>('button[type="submit"]')!.disabled).toBe(true)
    expect(element.textContent).toContain('Synthetic stale discard target')
  })

  it('does not clear a newer unsaved execution when an older successful targeted acknowledgement arrives late', async () => {
    const native = fixture(true)
    const original = native.invoke.getMockImplementation()!
    let acknowledge!: () => void
    const ack = new Promise<void>((resolve) => { acknowledge = resolve })
    native.invoke.mockImplementation(async (command, args) => {
      const result = await original(command, args)
      if (command === 'discard_unsaved_and_new_chat') await ack
      return result
    })
    const { element, authority } = await mount()
    await draft(element, 'Synthetic preserved draft')
    await act(async () => { element.querySelector<HTMLButtonElement>('[aria-label="New Chat"]')!.click() })
    const newer = parseUnsavedResult(native.replaceUnsaved('21'))
    await act(async () => { authority.fieldState('unsavedResult')[1](newer); acknowledge() })
    expect(authority.fieldRef('unsavedResult').current).toBe(newer)
    expect(element.querySelector('[aria-label="Unsaved answer"]')).not.toBeNull()
    expect(element.querySelector('textarea')!.value).toBe('Synthetic preserved draft')
    expect(element.querySelector<HTMLButtonElement>('button[type="submit"]')!.disabled).toBe(true)
    expect(native.invoke.mock.calls.filter(([command]) => command === 'discard_unsaved_and_new_chat')).toHaveLength(1)
  })
  it.each(['banner', 'toolbar'] as const)('uses only the exact targeted command for explicit %s New Chat with unsaved output', async (entry) => {
    const native = fixture(true)
    const { element, authority } = await mount()
    await draft(element, 'Synthetic preserved draft')
    const button = entry === 'toolbar' ? element.querySelector<HTMLButtonElement>('[aria-label="New Chat"]')! : Array.from(element.querySelectorAll('button')).find((button) => button.textContent === 'New Chat and discard unsaved answer')!
    await act(async () => { button.click() })
    expect(native.invoke.mock.calls.filter(([command]) => command === 'discard_unsaved_and_new_chat')).toEqual([['discard_unsaved_and_new_chat', { executionId: { session_id: durableId(1), generation: durableId(20) } }]])
    expect(native.invoke.mock.calls.some(([command]) => command === 'reset_session' || command === 'submit_prompt')).toBe(false)
    expect(authority.fieldRef('unsavedResult').current).toBeNull()
    expect(authority.selection.canonicalSession.current).toBe(durableId(2))
    expect(element.querySelector('textarea')!.value).toBe('Synthetic preserved draft')
  })
  it('accepts native first-request durable preparation from an empty desktop selection', async () => {
    let receipt: ReturnType<typeof savedOutcome> | null = null
    const invoke = vi.fn(async (command: string, args?: unknown): Promise<unknown> => {
      if (command === 'get_app_snapshot') return appSnapshot(receipt === null ? [] : [receipt['turn'] as Record<string, unknown>], {}, { session_id: receipt === null ? '0' : '1' })
      if (command === 'get_startup_state') return readyStartupState()
      if (command === 'get_assistant_settings') return defaultAssistantSettings()
      if (command === 'submit_prompt') { receipt = savedOutcome((args as { requestId: string }).requestId, 'Synthetic first question', 'Synthetic first saved answer'); return receipt }
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke, createChannel: <T,>(send: (message: T) => void) => ({ send }) }
    const { element, authority } = await mount()
    await draft(element, 'Synthetic first question')
    await act(async () => { element.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    expect(element.textContent).toContain('Synthetic first saved answer')
    expect(element.textContent).not.toContain('Response failed')
    expect(authority.selection.canonicalSession.current).toBe('1')
    expect(authority.selection.transcript.current).toHaveLength(1)
  })
  it('does not reconstruct process-owned output when a fresh native owner reports no unsaved result', async () => {
    fixture(true)
    const first = await mount()
    expect(first.element.querySelector('[aria-label="Unsaved answer"]')).not.toBeNull()
    await act(async () => { root?.unmount() })
    first.element.remove()
    fixture(false)
    const fresh = await mount()
    expect(fresh.element.querySelector('[aria-label="Unsaved answer"]')).toBeNull()
    expect(fresh.element.textContent).not.toContain('Synthetic unsaved answer')
    expect(fresh.authority.selection.transcript.current).toHaveLength(1)
  })
  it('fails closed and retires presentation for an unsaved receipt from another durable conversation', async () => {
    const native = fixture()
    const original = native.invoke.getMockImplementation()!
    native.invoke.mockImplementation(async (command, args) => command === 'submit_prompt'
      ? { kind: 'unsaved', save_disposition: 'unsaved', unsaved_result: { ...projection((args as { requestId: string }).requestId), execution_id: { session_id: durableId(2), generation: durableId(20) } } }
      : original(command, args))
    const { element, authority } = await mount()
    await draft(element, 'Synthetic current conversation question')
    await act(async () => { element.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    expect(element.textContent).toContain('The unsaved response did not match the active execution.')
    expect(element.querySelector('[aria-label="Unsaved answer"]')).toBeNull()
    expect(authority.fieldRef('promptState').current).toBe('idle')
    expect(authority.selection.transcript.current).toHaveLength(1)
  })
  it('does not request or publish prediction completion while an unsaved answer blocks follow-up', async () => {
    const native = fixture(false, true)
    const { element, authority } = await mount()
    await draft(element, 'Synthetic unsaved question')
    expect(native.invoke.mock.calls.filter(([command]) => command === 'request_completion')).toHaveLength(1)
    await act(async () => { element.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    await draft(element, 'Synthetic blocked draft')
    await act(async () => { native.completion({ source: 'typed', revision: 1, voice_session_id: null, suffix: 'Synthetic stale completion' }) })
    expect(native.invoke.mock.calls.filter(([command]) => command === 'request_completion')).toHaveLength(1)
    expect(authority.fieldRef('typedCompletionSuffix').current).toBe('')
    expect(element.querySelector<HTMLButtonElement>('button[type="submit"]')!.disabled).toBe(true)
  })
  it('shows failed-save output separately, blocks admission and keeps Copy presentation-only', async () => {
    vi.useFakeTimers()
    const native = fixture()
    const writeText = vi.fn().mockResolvedValue(undefined)
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } })
    const { element, authority } = await mount()
    await draft(element, 'Synthetic unsaved question')
    await act(async () => { element.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    expect(element.querySelector('[aria-label="Unsaved answer"]')?.textContent).toContain('Synthetic unsaved answer')
    expect(element.querySelector('[aria-label="Answer not saved"]')?.textContent).toContain('Answer not saved')
    expect(authority.selection.transcript.current).toHaveLength(1)
    expect(authority.selection.transcript.current[0]?.requestId).toBe('synthetic-saved')
    await draft(element, 'Synthetic follow-up draft')
    expect(element.querySelector<HTMLButtonElement>('button[type="submit"]')!.disabled).toBe(true)
    await act(async () => { element.querySelector<HTMLButtonElement>('[aria-label="Copy unsaved response"]')!.click() })
    expect(writeText).toHaveBeenCalledWith('Synthetic unsaved answer')
    expect(authority.fieldRef('unsavedResult').current).not.toBeNull()
    expect(native.invoke.mock.calls.filter(([command]) => command === 'submit_prompt')).toHaveLength(1)
    await act(async () => { await vi.advanceTimersByTimeAsync(5_000) })
    expect(element.querySelector('[aria-label="Answer not saved"]')).not.toBeNull()
    expect(element.querySelector<HTMLButtonElement>('button[type="submit"]')!.disabled).toBe(true)
  })

  it('keeps a saved receipt saved when both the optional terminal projection and subsequent snapshot reads fail', async () => {
    vi.useFakeTimers()
    let committed = false
    const invoke = vi.fn(async (command: string, args?: unknown): Promise<unknown> => {
      if (command === 'get_app_snapshot') {
        if (committed) throw new Error('Synthetic projection unavailable after commit')
        return appSnapshot()
      }
      if (command === 'get_startup_state') return readyStartupState()
      if (command === 'get_assistant_settings') return defaultAssistantSettings()
      if (command === 'submit_prompt') {
        committed = true
        return { ...savedOutcome((args as { requestId: string }).requestId, 'Synthetic committed question', 'Synthetic committed answer'), app_snapshot: { invalid: true } }
      }
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke, createChannel: <T,>(send: (message: T) => void) => ({ send }) }
    const { element, authority } = await mount()
    await draft(element, 'Synthetic committed question')
    await act(async () => { element.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    await act(async () => { await vi.advanceTimersByTimeAsync(1_000) })
    expect(element.textContent).toContain('Synthetic committed answer')
    expect(element.textContent).not.toContain('Response failed')
    expect(element.querySelector('[aria-label="Answer not saved"]')).toBeNull()
    expect(authority.selection.transcript.current).toHaveLength(1)
    expect(authority.fieldRef('unsavedResult').current).toBeNull()
    expect(invoke.mock.calls.filter(([command]) => command === 'submit_prompt')).toHaveLength(1)
  })

  it('recovers a single unsaved answer; failed New Chat preserves answer, draft and admission block', async () => {
    const native = fixture(true)
    const { element, authority } = await mount()
    expect(element.querySelectorAll('[aria-label="Unsaved answer"]')).toHaveLength(1)
    expect(element.querySelectorAll('.message--user')).toHaveLength(2)
    await draft(element, 'Synthetic preserved draft')
    native.failReset(true)
    await act(async () => { Array.from(element.querySelectorAll('button')).find((button) => button.textContent === 'New Chat and discard unsaved answer')!.click() })
    expect(element.querySelector('textarea')!.value).toBe('Synthetic preserved draft')
    expect(element.querySelector('[aria-label="Unsaved answer"]')?.textContent).toContain('Synthetic unsaved answer')
    expect(element.querySelector<HTMLButtonElement>('button[type="submit"]')!.disabled).toBe(true)
    expect(authority.selection.transcript.current).toHaveLength(1)
    native.failReset(false)
    await act(async () => { Array.from(element.querySelectorAll('button')).find((button) => button.textContent === 'New Chat and discard unsaved answer')!.click() })
    expect(element.querySelector('[aria-label="Unsaved answer"]')).toBeNull()
    expect(element.querySelector('textarea')!.value).toBe('Synthetic preserved draft')
    expect(authority.selection.canonicalSession.current).toBe(durableId(2))
    expect(element.querySelector<HTMLButtonElement>('button[type="submit"]')!.disabled).toBe(false)
  })
})

import { act } from 'react'
import { readFileSync } from 'node:fs'
import { createRoot } from 'react-dom/client'
import type { Root } from 'react-dom/client'
import { afterEach, describe, expect, it, vi } from 'vitest'

// Exercise the actual runtime boundary. Partial mocks of the cyclic native/browser
// adapter graph can leave consumers holding the original desktop export.
vi.hoisted(() => { vi.stubEnv('VITE_VOXGOLEM_RUNTIME', 'android') })

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
  vi.restoreAllMocks()
})

describe('Android App lifecycle', () => {
  it('still admits a correlated native loop final exactly once while manual Send is blocked by active voice', async () => {
    let input: (value: unknown) => void = () => undefined
    let sessionId = ''
    let finish: (value: unknown) => void = () => undefined
    const voice = { kind: 'voice_status', available: true, reason: 'Synthetic speech service', mode: 'loop', dictation: 'idle', loop: 'idle', ttsEnabled: false, ttsAvailable: true, ttsPhase: 'idle', error: null }
    const invoke = vi.fn(async (command: string, args?: unknown) => {
      if (command === 'get_app_snapshot') return appSnapshot()
      if (command === 'get_startup_state') return readyStartupState()
      if (command === 'assistant_voice_status') return voice
      if (command === 'start_assistant_voice') {
        const request = args as { sessionId: string; loopMode: boolean; updates: { send: (value: unknown) => void } }
        expect(request.loopMode).toBe(true)
        sessionId = request.sessionId; input = request.updates.send
        return { ...voice, dictation: 'listening', loop: 'listening' }
      }
      if (command === 'submit_prompt') return new Promise((resolve) => { finish = resolve })
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke, createChannel: <T,>(send: (value: T) => void) => ({ send }) }
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="Talk"]')!.click() })
    await act(async () => { input({ sessionId, kind: 'dictation', phase: 'final', draft: 'Synthetic authoritative loop final' }); input({ ...voice, sessionId, dictation: 'final', loop: 'generating' }) })
    expect(container.querySelector<HTMLButtonElement>('button[type="submit"]')!.disabled).toBe(true)
    await act(async () => { input({ sessionId, kind: 'submit', requestId: 'synthetic-native-loop-request', text: 'Synthetic authoritative loop final' }); input({ sessionId, kind: 'submit', requestId: 'synthetic-native-loop-request', text: 'Synthetic duplicate' }) })
    expect(invoke.mock.calls.filter(([command]) => command === 'submit_prompt')).toHaveLength(1)
    expect(invoke).toHaveBeenCalledWith('submit_prompt', expect.objectContaining({ requestId: 'synthetic-native-loop-request', prompt: 'Synthetic authoritative loop final' }))
    await act(async () => { finish({ kind: 'cancelled' }) })
  })
  it('blocks every manual submission of a live partial and allows Enter only after native dictation retires', async () => {
    let input: (value: unknown) => void = () => undefined
    let sessionId = ''
    const voice = { kind: 'voice_status', available: true, reason: 'Synthetic speech service', mode: 'dictation', dictation: 'idle', loop: 'idle', ttsEnabled: false, ttsAvailable: true, ttsPhase: 'idle', error: null }
    const submit = vi.fn(async () => ({ kind: 'cancelled' }))
    window.__TAURI_INTERNALS__ = {
      createChannel: <T,>(handler: (value: T) => void) => { input = handler as (value: unknown) => void; return {} },
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') return appSnapshot()
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'assistant_voice_status') return voice
        if (command === 'start_assistant_voice') { sessionId = (args as { sessionId: string }).sessionId; return { ...voice, dictation: 'listening' } }
        if (command === 'submit_prompt') return submit()
        return null
      },
    }
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="Ask"]')!.click() })
    await act(async () => { input({ sessionId, kind: 'dictation', phase: 'partial', draft: 'Synthetic unfinished words' }) })
    const composer = container.querySelector<HTMLTextAreaElement>('#promptComposer')!
    expect(composer.value).toBe('Synthetic unfinished words')
    expect(container.querySelector<HTMLButtonElement>('button[type="submit"]')!.disabled).toBe(true)
    await act(async () => { composer.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })) })
    expect(submit).not.toHaveBeenCalled()
    await act(async () => { container!.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); container!.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    expect(submit).not.toHaveBeenCalled()
    expect(composer.value).toBe('Synthetic unfinished words')
    await act(async () => { input({ ...voice, sessionId, dictation: 'error', diagnosticError: 'recognition_error_7' }) })
    expect(container.querySelector<HTMLButtonElement>('button[type="submit"]')!.disabled).toBe(false)
    await act(async () => { composer.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })) })
    expect(submit).toHaveBeenCalledTimes(1)
  })

  it.each(['close', 'back'] as const)('opens common Settings over Voice notes and restores that screen, focus and scrolling on %s', async (action) => {
    vi.useFakeTimers()
    const originalX = document.body.style.overflowX
    const originalY = document.body.style.overflowY
    const previousState = { syntheticPreviousScreen: 'voice-notes' }
    window.history.replaceState(previousState, '')
    vi.spyOn(window.history, 'back').mockImplementation(() => { window.history.replaceState(previousState, ''); window.dispatchEvent(new PopStateEvent('popstate', { state: previousState })) })
    window.__TAURI_INTERNALS__ = { invoke: async (command) => {
      if (command === 'get_app_snapshot') return appSnapshot()
      if (command === 'get_startup_state') return readyStartupState()
      if (command === 'plugin:capture|snapshot') return { available: true, setupRequired: false, notesFolderUri: null, captures: [], error: null }
      return null
    } }
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    try {
      document.body.style.overflowX = 'scroll'; document.body.style.overflowY = 'auto'
      await act(async () => { root?.render(<App />) })
      await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="Voice notes"]')!.click() })
      const notes = container.querySelector('.capture-history')!
      const opener = container.querySelector<HTMLButtonElement>('[aria-label="Settings"]')!
      await act(async () => { opener.click() })
      expect(container.querySelector('[role="dialog"]')).not.toBeNull()
      expect(document.activeElement?.getAttribute('aria-label')).toBe('Close settings')
      expect(document.body.style.overflowX).toBe('hidden')
      expect(window.history.state).toMatchObject(previousState)
      await act(async () => { if (action === 'close') container!.querySelector<HTMLButtonElement>('[aria-label="Close settings"]')!.click(); else window.history.back(); await vi.advanceTimersByTimeAsync(0) })
      expect(container.querySelector('[role="dialog"]')).toBeNull()
      expect(container.querySelector('.capture-history')).toBe(notes)
      expect(container.querySelector('form.composer')).toBeNull()
      expect(document.activeElement).toBe(opener)
      expect(window.history.state).toEqual(previousState)
      expect(document.body.style.overflowX).toBe('scroll')
      expect(document.body.style.overflowY).toBe('auto')
    } finally { document.body.style.overflowX = originalX; document.body.style.overflowY = originalY; window.history.replaceState(null, '') }
  })
  it('keeps phone controls icon-only, native and accessible with Send beside the prompt and compact canonical answers', async () => {
    window.history.replaceState(null, '')
    const voice = { kind: 'voice_status', available: true, reason: 'Synthetic speech service', mode: 'dictation', dictation: 'idle', loop: 'idle', ttsEnabled: true, ttsAvailable: true, ttsPhase: 'idle', error: null }
    window.__TAURI_INTERNALS__ = { invoke: async (command) => {
      if (command === 'get_app_snapshot') return appSnapshot([canonicalTurn('2', '3', 'Synthetic prompt', 'Synthetic saved answer', 'synthetic-request')])
      if (command === 'get_startup_state') return readyStartupState()
      if (command === 'assistant_voice_status') return voice
      return null
    } }
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    const logo = container.querySelector('img.shell__logo')!
    expect(logo.getAttribute('width')).toBe('32')
    expect(container.querySelector('[data-runtime-phase]')).toBeNull()
    expect(container.querySelector('.shell')?.getAttribute('data-platform')).toBe('android')
    for (const name of ['Ask', 'Talk', 'Speak replies']) {
      const button = container.querySelector<HTMLButtonElement>(`button[aria-label="${name}"]`)!
      expect(button.querySelector('svg[aria-hidden="true"]')).toBeTruthy()
      expect(button.textContent).toBe('')
      expect(button.getAttribute('title')).toContain(name)
      const descriptionId = button.getAttribute('aria-describedby')!
      expect(descriptionId).toBeTruthy()
      expect(document.getElementById(descriptionId)?.classList.contains('sr-only')).toBe(true)
      expect(document.getElementById(descriptionId)?.textContent).toBeTruthy()
      expect(button.classList.contains('shell__icon-control')).toBe(true)
      expect(button.getAttribute('aria-pressed')).toBe(name === 'Speak replies' ? 'true' : 'false')
    }
    const picker = container.querySelector<HTMLSelectElement>('#composerInstantSelect')!
    expect(picker.value).toBe('custom-sol-high')
    expect(picker.getAttribute('aria-label')).toBe('Instant model')
    expect(picker.closest('[aria-label="Android voice controls"]')).toBeTruthy()
    expect(picker.closest('.native-icon-select')?.querySelector('svg')).toBeTruthy()
    const send = container.querySelector('button[aria-label="Send"]')!
    expect(send.closest('.composer__prompt-row')?.querySelector('textarea')).toBeTruthy()
    const search = container.querySelector('[aria-label="Search mode"]')!
    const toolbar = container.querySelector('.shell__header-actions')!
    const overflow = toolbar.querySelector('details.shell__overflow')!
    expect(overflow).not.toBeNull()
    expect(overflow.hasAttribute('open')).toBe(false)
    expect(overflow.querySelector('summary')?.getAttribute('aria-label')).toBe('More actions')
    expect(toolbar.querySelector('[aria-label="Ask"]')?.closest('details')).toBeNull()
    expect(toolbar.querySelector('[aria-label="Talk"]')?.closest('details')).toBeNull()
    expect(toolbar.querySelector('[aria-label="Speak replies"]')?.closest('details')).toBe(overflow)
    await act(async () => {
      overflow.setAttribute('open', '')
      toolbar.querySelector<HTMLButtonElement>('[aria-label="Speak replies"]')!.focus()
      overflow.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
    })
    expect(overflow.hasAttribute('open')).toBe(false)
    expect(document.activeElement).toBe(overflow.querySelector('summary'))
    expect(search.closest('.shell__header-actions')).toBe(toolbar)
    expect(picker.closest('.shell__header-actions')).toBe(toolbar)
    expect(container.querySelector('.composer__search')).toBeNull()
    expect(container.querySelector('.composer [aria-label="Android voice controls"]')).toBeNull()
    expect(toolbar.querySelector('[aria-label="Switch to light mode"]')).toBeNull()
    expect(toolbar.querySelector('[aria-label="Settings"] svg')).not.toBeNull()
    await act(async () => { toolbar.querySelector<HTMLButtonElement>('[aria-label="Settings"]')!.click() })
    expect(container.querySelector('[role="dialog"] [aria-label="Switch to light mode"]')).not.toBeNull()
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="Close settings"]')!.click() })
    expect(search.closest('.native-icon-select')?.querySelector('svg[data-icon-kind="search"]')).not.toBeNull()
    expect(search.closest('.search-mode')?.querySelector('.search-mode__label')).toBeNull()
    expect(search.getAttribute('title')).toBe('Search mode: Quick')
    expect(container.querySelectorAll('[aria-label="Search mode"]')).toHaveLength(1)
    expect(container.querySelectorAll('button[type="submit"]')).toHaveLength(1)
    expect(container.querySelector('.answer-stage__answer')?.textContent).toBe('Synthetic saved answer')
    expect(container.querySelector('.answer-stage h2')).toBeNull()
    expect(container.querySelector('.answer-stage [data-status="completed"]')).toBeNull()
  })

  it('matches the compact 44px Send button to the single-row Android input without truncating long drafts', async () => {
    window.__TAURI_INTERNALS__ = { invoke: async (command) => {
      if (command === 'get_app_snapshot') return appSnapshot()
      if (command === 'get_startup_state') return readyStartupState()
      return null
    } }
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    const sheet = new CSSStyleSheet()
    sheet.replaceSync(readFileSync('src/App.css', 'utf8'))
    const rules = Array.from(sheet.cssRules).filter((rule): rule is CSSStyleRule => rule instanceof CSSStyleRule)
    const styleFor = (suffix: string, element: Element): CSSStyleDeclaration => {
      const rule = rules.find((rule) => rule.selectorText === `.shell[data-platform='android'] ${suffix}`)!
      expect(element.matches(rule.selectorText)).toBe(true)
      return rule.style
    }
    const input = container.querySelector<HTMLTextAreaElement>('#promptComposer')!
    const send = container.querySelector('[aria-label="Send"]')!
    const row = send.closest('.composer__prompt-row')!
    expect(styleFor('.composer__prompt-row', row).marginTop).toBe('0px')
    expect(styleFor('.prompt-composer', input.parentElement!).display).toBe('contents')
    expect(styleFor('.composer__input', input).display).toBe('block')
    expect(styleFor('.composer__input', input).gridRow).toBe('1')
    expect(styleFor('.composer__prompt-row > .composer__button', send).gridRow).toBe('1')
    expect(styleFor('.composer__prompt-row > .composer__button', send).alignSelf).toBe('stretch')
    expect(styleFor('.composer__button', send).minWidth).toBe('44px')
    expect(styleFor('.composer__button', send).minHeight).toBe('44px')
    expect(styleFor('.composer__input', input).minHeight).toBe('44px')
    expect(input.getAttribute('rows')).toBe('1')
    const search = container.querySelector('[aria-label="Search mode"]')!
    expect(styleFor('.shell__header-actions select', search).minHeight).toBe('48px')
    expect(styleFor('.shell__header-actions', search.closest('.shell__header-actions')!).flexWrap).toBe('nowrap')
    const ask = container.querySelector('[aria-label="Ask"]')!
    expect(styleFor('.shell__header-actions .shell__icon-control', ask).minWidth).toBe('48px')
    for (const icon of container.querySelectorAll('.shell__header-actions .shell__icon-control svg')) {
      const style = styleFor('.shell__header-actions .shell__icon-control svg', icon)
      expect(style.width).toBe('20px'); expect(style.height).toBe('20px')
    }

    vi.spyOn(window, 'getComputedStyle').mockReturnValue({ lineHeight: '22px', paddingTop: '10px', paddingBottom: '10px', borderTopWidth: '1px', borderBottomWidth: '1px' } as CSSStyleDeclaration)
    Object.defineProperty(input, 'scrollHeight', { configurable: true, value: 104 })
    await act(async () => { window.dispatchEvent(new Event('resize')) })
    expect(input.style.height).toBe('44px')
    expect(input.style.overflowY).toBe('auto')
    const draft = 'Synthetic long draft\nSynthetic additional line'
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set?.call(input, draft)
      input.dispatchEvent(new Event('input', { bubbles: true }))
    })
    expect(input.value).toBe(draft)
    Object.defineProperty(input, 'scrollHeight', { configurable: true, value: 42 })
    await act(async () => { window.dispatchEvent(new Event('resize')) })
    expect(input.style.height).toBe('44px')
    expect(input.style.overflowY).toBe('hidden')
    expect(send.closest('.composer__prompt-row')).toBe(row)
  })

  it.each(['completed', 'cancelled', 'error'] as const)('rotates the phone emblem from pending through stream and stage completion until %s retirement', async (outcome) => {
    vi.useFakeTimers()
    let updates: (value: unknown) => void = () => undefined
    let finish: (value: unknown) => void = () => undefined
    let fail: (value: unknown) => void = () => undefined
    let requestId = ''
    let turn: Record<string, unknown> | null = null
    window.__TAURI_INTERNALS__ = {
      createChannel: <T,>(handler: (value: T) => void) => ({ send: handler }),
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') return appSnapshot(turn === null ? [] : [turn])
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'submit_prompt') {
          updates = (args as { updates: { send: (value: unknown) => void } }).updates.send
          requestId = (args as { requestId: string }).requestId
          return new Promise((resolve, reject) => { finish = resolve; fail = reject })
        }
        return null
      },
    }
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    const logo = container.querySelector('.shell__logo')!
    expect(logo.getAttribute('data-busy')).toBe('false')
    await act(async () => {
      const composer = container!.querySelector('textarea')!
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set?.call(composer, 'Synthetic pending question')
      composer.dispatchEvent(new Event('input', { bubbles: true }))
    })
    await act(async () => { container!.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    expect(logo.getAttribute('data-busy')).toBe('true')
    const stop = container.querySelector('[aria-label="Stop"]')
    expect(stop).not.toBeNull()
    expect(stop?.closest('details')).toBeNull()
    await act(async () => { updates({ kind: 'delta', text: 'Synthetic streamed answer' }); updates({ kind: 'stage', stage: 'instant', status: 'completed', detail: null }); await vi.advanceTimersByTimeAsync(20) })
    expect(container.querySelector('.answer-stage__answer')?.textContent).toBe('Synthetic streamed answer')
    expect(logo.getAttribute('data-busy')).toBe('true')
    await act(async () => {
      if (outcome === 'error') fail({ code: 'internal', message: 'Synthetic generation failed' })
      else if (outcome === 'completed') { turn = canonicalTurn('2', '3', 'Synthetic pending question', 'Synthetic saved answer', requestId); finish({ kind: 'completed', turn }) }
      else finish({ kind: 'cancelled' })
    })
    expect(logo.getAttribute('data-busy')).toBe('false')
  })
  it('keeps full synced conversation titles in an accessible header icon native chooser', async () => {
    const id = '11111111-1111-4111-8111-111111111111:1'
    const title = `Synthetic offline conversation ${'long native dropdown title '.repeat(8)}`
    window.__TAURI_INTERNALS__ = { invoke: async (command) => {
      if (command === 'get_app_snapshot') return { ...appSnapshot(), session_id: id, assistant: {
        revision: '1', conversations: [{ id, title }], selected_head: null, branch_choices: [], draft: '', interrupted: false,
      } }
      if (command === 'get_startup_state') return readyStartupState()
      return null
    } }
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    const chooser = container.querySelector<HTMLSelectElement>('[aria-label="Conversation"]')!
    expect(chooser.selectedOptions[0]?.textContent).toBe(title)
    const sheet = new CSSStyleSheet()
    sheet.replaceSync(readFileSync('src/App.css', 'utf8'))
    const rules = Array.from(sheet.cssRules).filter((rule): rule is CSSStyleRule => rule instanceof CSSStyleRule)
    const composer = rules.find((rule) => rule.selectorText === '.composer')
    const select = rules.find((rule) => rule.selectorText === '.shell__conversation-select')
    expect(composer?.style.minWidth).toBe('0')
    expect(chooser.closest('header')).not.toBeNull()
    expect(chooser.closest('.shell__conversation-picker')?.querySelector('svg')).not.toBeNull()
    expect(select?.style.opacity).toBe('0')
    expect(chooser.matches(select!.selectorText)).toBe(true)
    expect(container.querySelector('.composer [aria-label="Conversation"]')).toBeNull()
    for (const name of ['Voice notes', 'New Chat']) {
      const button = container.querySelector<HTMLButtonElement>(`header button[aria-label="${name}"]`)!
      expect(button.querySelector('svg')).not.toBeNull()
      expect(button.textContent).toBe('')
    }
    await act(async () => { chooser.focus(); chooser.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })) })
    expect(document.activeElement).toBe(chooser)
  })

  it('shows phone speech routes only in Settings without losing the live Channel or adding a poller', async () => {
    vi.useFakeTimers()
    window.history.replaceState(null, '')
    const voice = { kind: 'voice_status', available: true, reason: 'Synthetic configured recognizer', mode: 'dictation', dictation: 'idle', loop: 'idle', ttsEnabled: false, ttsAvailable: true, ttsPhase: 'idle', error: null,
      recognitionRoute: 'Synthetic STT · requested en-AU · reported en-GB · local preferred · provider-controlled', ttsRoute: 'Synthetic TTS · selected en-GB · installed local voice' }
    let sessionId = ''
    let channel: (value: unknown) => void = () => undefined
    const invoke = vi.fn(async (command: string, args?: unknown) => {
      if (command === 'get_app_snapshot') return appSnapshot()
      if (command === 'get_startup_state') return readyStartupState()
      if (command === 'assistant_voice_status') return voice
      if (command === 'start_assistant_voice') { sessionId = (args as { sessionId: string }).sessionId; return { ...voice, dictation: 'listening' } }
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke, createChannel: <T,>(handler: (value: T) => void) => { channel = handler as (value: unknown) => void; return {} } }
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />); await vi.advanceTimersByTimeAsync(0) })
    expect(container.textContent).not.toContain(voice.reason)
    expect(container.textContent).not.toContain(voice.recognitionRoute)
    expect(container.textContent).not.toContain(voice.ttsRoute)
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="Ask"]')!.click() })
    const polls = invoke.mock.calls.filter(([command]) => command === 'assistant_voice_status').length
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="Settings"]')!.click() })
    const dialog = container.querySelector('[role="dialog"]')!
    expect(dialog.textContent).toContain(voice.reason)
    expect(dialog.textContent).toContain(voice.recognitionRoute)
    expect(dialog.textContent).toContain(voice.ttsRoute)
    expect(invoke.mock.calls.some(([command]) => command === 'stop_assistant_voice')).toBe(false)
    expect(invoke.mock.calls.filter(([command]) => command === 'assistant_voice_status')).toHaveLength(polls)
    await act(async () => { channel({ sessionId, kind: 'dictation', phase: 'partial', draft: 'Synthetic live words in Settings' }); await vi.advanceTimersByTimeAsync(1000) })
    expect(container.querySelector<HTMLTextAreaElement>('#promptComposer')?.value).toBe('Synthetic live words in Settings')
    expect(invoke.mock.calls.filter(([command]) => command === 'assistant_voice_status')).toHaveLength(polls + 1)
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="Close settings"]')!.click() })
    expect(container.querySelector('[role="dialog"]')).toBeNull()
    expect(container.textContent).not.toContain(voice.recognitionRoute)
    expect(invoke.mock.calls.some(([command]) => command === 'stop_assistant_voice')).toBe(false)
    window.history.replaceState(null, '')
  })

  it.each(['natural', 'cancelled', 'background', 'typing', 'different-context', 'different-head'] as const)('handles an authorized shortcut final across deferred canonical freshness: %s', async (settlement) => {
    vi.useFakeTimers()
    const id = (sequence: number): string => `88888888-8888-4888-8888-888888888888:${sequence}`
    const token = '99999999-9999-4999-8999-999999999999'
    let selected = id(1)
    let selectedHead: string | null = null
    let launch = false
    let sessionId = ''
    let holdSnapshot = false
    let settleSnapshot: () => void = () => undefined
    let input: (value: unknown) => void = () => undefined
    const drafts = new Map([[id(1), 'Synthetic old draft'], [id(2), '']])
    const snapshot = () => ({ ...appSnapshot(), session_id: selected, transcript: [], assistant: { revision: '2', conversations: [{ id: id(1), title: 'Synthetic old chat' }, { id: id(2), title: 'Synthetic fresh chat' }], selected_head: selectedHead, branch_choices: [], draft: drafts.get(selected), interrupted: false } })
    const voice = { kind: 'voice_status', available: true, reason: 'Synthetic on-device speech', mode: 'dictation', dictation: 'idle', loop: 'idle', ttsEnabled: false, ttsAvailable: false, ttsPhase: 'idle', error: null }
    const invoke = vi.fn(async (command: string, args?: unknown) => {
      if (command === 'get_app_snapshot') {
        if (holdSnapshot) return new Promise((resolve) => { settleSnapshot = () => { holdSnapshot = false; resolve(snapshot()) } })
        return snapshot()
      }
      if (command === 'get_startup_state') return readyStartupState()
      if (command === 'assistant_voice_status') return voice
      if (command === 'consume_new_voice_shortcut') { const value = launch ? token : null; launch = false; return { token: value, error: null } }
      if (command === 'start_assistant_voice') { sessionId = (args as { sessionId: string }).sessionId; return { ...voice, mode: 'ask', dictation: 'listening', loop: 'listening' } }
      if (command === 'reset_session') { selected = id(2); return snapshot() }
      if (command === 'save_assistant_draft') { drafts.set(selected, (args as { text: string }).text); return null }
      if (command === 'submit_prompt') { expect(selected).toBe(id(1)); return { kind: 'cancelled' } }
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke, createChannel: <T,>(handler: (value: T) => void) => { input = handler as (value: unknown) => void; return {} } }
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />); await vi.advanceTimersByTimeAsync(0) })
    launch = true
    await act(async () => { window.dispatchEvent(new Event('voxgolem-new-voice-chat')); await vi.advanceTimersByTimeAsync(0) })
    expect(invoke).toHaveBeenCalledWith('start_assistant_voice', expect.objectContaining({ draft: 'Synthetic old draft', completionPolicy: 'single_turn', launchToken: token }))
    await act(async () => { input({ sessionId, kind: 'dictation', phase: 'partial', draft: 'Synthetic recognized words', utterance: '1', error: null }) })
    expect(invoke.mock.calls.some(([command]) => command === 'reset_session' || command === 'prepare_new_voice_shortcut')).toBe(false)
    expect(invoke.mock.calls.some(([command]) => command === 'submit_prompt')).toBe(false)
    holdSnapshot = true
    await act(async () => {
      input({ sessionId, kind: 'dictation', phase: 'final', draft: 'Synthetic authoritative final', utterance: '1', error: null })
      input({ sessionId, kind: 'submit', requestId: 'synthetic-authorized-final', text: 'Synthetic authoritative final', context: { sessionId: id(1), selectedHead: null } })
      input({ ...voice, sessionId, mode: 'ask', dictation: 'final', loop: 'generating' })
    })
    expect(invoke.mock.calls.some(([command]) => command === 'submit_prompt')).toBe(false)
    expect(container.querySelector<HTMLButtonElement>('button[type="submit"]')?.disabled).toBe(true)
    await act(async () => {
      if (settlement === 'cancelled') container!.querySelector<HTMLButtonElement>('[aria-label="New Chat"]')!.click()
      if (settlement === 'background') { vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('hidden'); document.dispatchEvent(new Event('visibilitychange')) }
      if (settlement === 'typing') {
        const composer = container!.querySelector<HTMLTextAreaElement>('#promptComposer')!
        Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set?.call(composer, 'Synthetic replacement typing')
        composer.dispatchEvent(new Event('input', { bubbles: true }))
      }
      if (settlement === 'different-context') selected = id(2)
      if (settlement === 'different-head') selectedHead = id(3)
    })
    await act(async () => { settleSnapshot() })
    expect(invoke.mock.calls.filter(([command]) => command === 'submit_prompt')).toHaveLength(settlement === 'natural' ? 1 : 0)
    if (settlement === 'natural') expect(invoke).toHaveBeenCalledWith('submit_prompt', expect.objectContaining({ requestId: 'synthetic-authorized-final', prompt: 'Synthetic authoritative final' }))
    await act(async () => { input({ sessionId, kind: 'submit', requestId: 'synthetic-authorized-final', text: 'Synthetic duplicate' }) })
    expect(invoke.mock.calls.filter(([command]) => command === 'submit_prompt')).toHaveLength(settlement === 'natural' ? 1 : 0)
  })

  it('retains a no-speech Ask draft and permits another deliberate Ask without changing old history', async () => {
    vi.useFakeTimers()
    const oldSession = 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa:1'
    const token = 'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb'
    let launch = false
    let sessionId = ''
    let input: (value: unknown) => void = () => undefined
    const oldDraft = 'Synthetic preserved unsent text'
    const snapshot = () => ({ ...appSnapshot(), session_id: oldSession, transcript: [canonicalTurn('2', '3', 'Synthetic preserved question', 'Synthetic preserved answer', 'synthetic-preserved-request')], assistant: { revision: '1', conversations: [{ id: oldSession, title: 'Synthetic preserved conversation' }], selected_head: null, branch_choices: [], draft: oldDraft, interrupted: false } })
    const voice = { kind: 'voice_status', available: true, reason: 'Synthetic on-device speech', mode: 'dictation', dictation: 'idle', loop: 'idle', ttsEnabled: false, ttsAvailable: false, ttsPhase: 'idle', error: null }
    const invoke = vi.fn(async (command: string, args?: unknown) => {
      if (command === 'get_app_snapshot') return snapshot()
      if (command === 'get_startup_state') return readyStartupState()
      if (command === 'assistant_voice_status') return voice
      if (command === 'consume_new_voice_shortcut') { const value = launch ? token : null; launch = false; return { token: value, error: null } }
      if (command === 'start_assistant_voice') { const value = args as { sessionId: string; launchToken?: string }; sessionId = value.sessionId; return { ...voice, mode: 'ask', dictation: 'listening', loop: 'listening' } }
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke, createChannel: <T,>(handler: (value: T) => void) => { input = handler as (value: unknown) => void; return {} } }
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />); await vi.advanceTimersByTimeAsync(0) })
    launch = true
    await act(async () => { window.dispatchEvent(new Event('voxgolem-new-voice-chat')); await vi.advanceTimersByTimeAsync(0) })
    expect(container.querySelector('#new-voice-draft-status')).toBeNull()
    expect(container.querySelector<HTMLTextAreaElement>('#promptComposer')?.value).toBe(oldDraft)
    expect(container.querySelector('.shell__header-actions [aria-label="Android voice controls"]')).not.toBeNull()
    await act(async () => {
      input({ ...voice, sessionId, mode: 'ask', dictation: 'error', loop: 'error', error: 'no_speech' })
    })
    expect(container.querySelector<HTMLTextAreaElement>('#promptComposer')?.value).toBe(oldDraft)
    expect(container.textContent).toContain('Synthetic preserved question')
    expect(container.textContent).toContain('Synthetic preserved answer')
    expect(container.textContent).not.toContain('New voice draft —')
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="Ask"]')!.click() })
    expect(invoke.mock.calls.filter(([command]) => command === 'start_assistant_voice')).toHaveLength(2)
    expect(invoke).toHaveBeenCalledWith('start_assistant_voice', expect.objectContaining({ draft: oldDraft, loopMode: false }))
    expect(invoke).toHaveBeenCalledWith('release_new_voice_shortcut', { token })
    expect(invoke.mock.calls.some(([command]) => command === 'reset_session' || command === 'submit_prompt')).toBe(false)
  })

  it.each(['stop', 'background', 'new-chat'] as const)('fences late Ask callbacks and preserves owned input across %s', async (outcome) => {
    vi.useFakeTimers()
    const id = (sequence: number): string => `44444444-4444-4444-8444-444444444444:${sequence}`
    const token = '55555555-5555-4555-8555-555555555555'
    let selected = id(1)
    let launch = false
    let sessionId = ''
    let input: (value: unknown) => void = () => undefined
    const snapshot = () => ({ ...appSnapshot(), session_id: selected, transcript: [], assistant: { revision: '2', conversations: [{ id: id(1), title: 'Synthetic old' }, { id: id(2), title: 'Synthetic fresh' }], selected_head: null, branch_choices: [], draft: selected === id(1) ? 'Synthetic preserved old draft' : '', interrupted: false } })
    const voice = { kind: 'voice_status', available: true, reason: 'Synthetic recognizer', mode: 'dictation', dictation: 'idle', loop: 'idle', ttsEnabled: false, ttsAvailable: false, ttsPhase: 'idle', error: null }
    const invoke = vi.fn(async (command: string, args?: unknown) => {
      if (command === 'get_app_snapshot') return snapshot()
      if (command === 'get_startup_state') return readyStartupState()
      if (command === 'assistant_voice_status') return voice
      if (command === 'consume_new_voice_shortcut') { const tokenValue = launch ? token : null; launch = false; return { token: tokenValue, error: null } }
      if (command === 'start_assistant_voice') { sessionId = (args as { sessionId: string }).sessionId; return { ...voice, mode: 'ask', dictation: 'listening', loop: 'listening' } }
      if (command === 'reset_session') { selected = id(2); return snapshot() }
      if (command === 'submit_prompt') return { kind: 'cancelled' }
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke, createChannel: <T,>(handler: (value: T) => void) => { input = handler as (value: unknown) => void; return {} } }
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />); await vi.advanceTimersByTimeAsync(0) })
    launch = true
    await act(async () => { window.dispatchEvent(new Event('voxgolem-new-voice-chat')); await vi.advanceTimersByTimeAsync(0) })
    const composer = container.querySelector<HTMLTextAreaElement>('#promptComposer')!
    expect(composer.value).toBe('Synthetic preserved old draft')
    expect(invoke.mock.calls.some(([command]) => command === 'reset_session')).toBe(false)
    await act(async () => { input({ sessionId, kind: 'dictation', phase: 'partial', draft: 'Synthetic voice draft', utterance: '1', error: null }) })
    expect(composer.value).toBe('Synthetic voice draft')
    await act(async () => {
      if (outcome === 'stop') container!.querySelector<HTMLButtonElement>('[aria-label="Stop voice"]')!.click()
      if (outcome === 'new-chat') container!.querySelector<HTMLButtonElement>('[aria-label="New Chat"]')!.click()
      if (outcome === 'background') { vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('hidden'); document.dispatchEvent(new Event('visibilitychange')) }
    })
    const retained = composer.value
    await act(async () => {
      input({ sessionId, kind: 'dictation', phase: 'final', draft: 'Synthetic obsolete final' })
      input({ sessionId, kind: 'submit', requestId: 'synthetic-obsolete-submit', text: 'Synthetic voice draft' })
    })
    expect(composer.value).toBe(retained)
    if (outcome !== 'new-chat') expect(retained).toBe('Synthetic voice draft')
    expect(invoke.mock.calls.some(([command]) => command === 'submit_prompt' || command === 'prepare_new_voice_shortcut')).toBe(false)
    expect(invoke).toHaveBeenCalledWith('stop_assistant_voice', undefined)
  })
  it.each([true, false])('Ask preserves current context and refuses microphone entry without provider admission: %s', async (accessReady) => {
    vi.useFakeTimers()
    const id = (sequence: number): string => `11111111-1111-4111-8111-111111111111:${sequence}`
    const token = '22222222-2222-4222-8222-222222222222'
    const providerReady = accessReady
    let launch = false
    let selected = id(1)
    let sessionId = ''
    let input: (value: unknown) => void = () => undefined
    const drafts = new Map([[id(1), 'Synthetic old unsent draft'], [id(2), '']])
    const snapshot = () => ({ ...appSnapshot(), transcript: selected === id(1) ? [canonicalTurn('2', '3', 'Synthetic old context', 'Synthetic old answer', 'synthetic-old-request')] : [], session_id: selected,
      model_options: [{ id: 'custom-sol-high', label: 'Sol High', available: providerReady, unavailable_reason: providerReady ? null : 'Provider access has expired' }],
      assistant: { revision: '2', conversations: [{ id: id(1), title: 'Synthetic old conversation' }, { id: id(2), title: 'Synthetic new conversation' }], selected_head: null, branch_choices: [], draft: drafts.get(selected), interrupted: false } })
    const voice = { kind: 'voice_status', available: true, reason: 'Synthetic on-device recognizer', mode: 'dictation', dictation: 'idle', loop: 'idle', ttsEnabled: false, ttsAvailable: false, ttsPhase: 'idle', error: null }
    const invoke = vi.fn(async (command: string, args?: unknown) => {
      if (command === 'get_app_snapshot') return snapshot()
      if (command === 'get_startup_state') return readyStartupState()
      if (command === 'assistant_voice_status') return voice
      if (command === 'consume_new_voice_shortcut') { if (!launch) return { token: null, error: null }; launch = false; return { token, error: null } }
      if (command === 'start_assistant_voice') { sessionId = (args as { sessionId: string }).sessionId; return { ...voice, mode: 'ask', dictation: 'listening', loop: 'listening' } }
      if (command === 'reset_session') { drafts.set(selected, (args as { draft: string }).draft); selected = id(2); return snapshot() }
      if (command === 'save_assistant_draft') { drafts.set(selected, (args as { text: string }).text); return null }
      if (command === 'submit_prompt') { expect(selected).toBe(id(1)); expect(snapshot().transcript).toHaveLength(1); return { kind: 'cancelled' } }
      if (command === 'select_conversation') { selected = (args as { conversationId: string }).conversationId; return snapshot() }
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke, createChannel: <T,>(handler: (value: T) => void) => { input = handler as (value: unknown) => void; return {} } }
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    await act(async () => { await vi.advanceTimersByTimeAsync(0) })
    launch = true
    await act(async () => { window.dispatchEvent(new Event('voxgolem-new-voice-chat')); await vi.advanceTimersByTimeAsync(0) })
    expect(invoke.mock.calls.some(([command]) => command === 'reset_session')).toBe(false)
    expect(drafts.get(id(1))).toBe('Synthetic old unsent draft')
    expect(container.querySelector<HTMLTextAreaElement>('#promptComposer')?.value).toBe('Synthetic old unsent draft')
    expect(container.textContent).toContain('Synthetic old context')
    expect(container.textContent).toContain('Synthetic old answer')
    expect(invoke.mock.calls.filter(([command]) => command === 'start_assistant_voice')).toHaveLength(accessReady ? 1 : 0)
    if (!accessReady) {
      expect(invoke).toHaveBeenCalledWith('release_new_voice_shortcut', { token })
      expect(invoke.mock.calls.some(([command]) => command === 'submit_prompt')).toBe(false)
      return
    }
    expect(invoke).toHaveBeenCalledWith('start_assistant_voice', expect.objectContaining({ draft: 'Synthetic old unsent draft', completionPolicy: 'single_turn', launchToken: token }))
    await act(async () => { input({ sessionId, kind: 'dictation', phase: 'final', draft: 'Synthetic new voice words', utterance: '1', error: null }) })
    expect(invoke.mock.calls.some(([command]) => command === 'reset_session')).toBe(false)
    expect(container.querySelector<HTMLTextAreaElement>('#promptComposer')?.value).toBe('Synthetic new voice words')
    expect(invoke.mock.calls.some(([command]) => command === 'submit_prompt')).toBe(false)
    await act(async () => { input({ sessionId, kind: 'submit', requestId: 'synthetic-shortcut-execution', text: 'Synthetic new voice words' }); input({ sessionId, kind: 'submit', requestId: 'synthetic-shortcut-execution', text: 'Synthetic duplicate' }) })
    expect(invoke.mock.calls.filter(([command]) => command === 'submit_prompt')).toHaveLength(accessReady ? 1 : 0)
  })
  it.each([
    ['select_conversation', false], ['select_conversation_head', false],
    ['select_conversation', true], ['select_conversation_head', true],
  ] as const)('fences post-mutation rejection of %s until canonical recovery (exhausted: %s)', async (selectionCommand, exhaustRecovery) => {
    vi.useFakeTimers()
    const id = (sequence: number): string => `11111111-1111-4111-8111-111111111111:${sequence}`
    const headSelection = selectionCommand === 'select_conversation_head'
    let selected = id(1)
    let head: string | null = null
    let mutated = false
    let recoveryReads = 0
    let permitResume = false
    let settleCanonical: () => void = () => undefined
    let nativeDraft = 'Stored synthetic draft'
    const saves: Array<{ conversation: string; head: string | null; text: string }> = []
    const snapshot = () => ({ ...appSnapshot(), session_id: selected, assistant: {
      revision: '3', conversations: [{ id: id(1), title: 'Synthetic A' }, { id: id(2), title: 'Synthetic B' }], selected_head: head,
      branch_choices: headSelection && head === null ? [id(3), id(4)] : [], draft: nativeDraft, interrupted: false,
      error: mutated ? 'not_saved' : null,
    } })
    const invoke = vi.fn(async (command: string, args?: unknown) => {
      if (command === 'get_app_snapshot') {
        if (!mutated || permitResume) return snapshot()
        recoveryReads += 1
        if (recoveryReads <= 2 || exhaustRecovery) throw { code: 'internal', message: 'Synthetic snapshot temporarily unavailable' }
        return new Promise((resolve) => { settleCanonical = () => resolve(snapshot()) })
      }
      if (command === 'get_startup_state') return readyStartupState()
      if (command === selectionCommand) {
        if (headSelection) head = (args as { headId: string }).headId
        else selected = (args as { conversationId: string }).conversationId
        mutated = true
        throw { code: 'internal', message: 'Synthetic post-mutation projection failure' }
      }
      if (command === 'save_assistant_draft') { nativeDraft = (args as { text: string }).text; saves.push({ conversation: selected, head, text: nativeDraft }); return null }
      if (command === 'submit_prompt') throw new Error('No provider execution is allowed before canonical recovery')
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke, createChannel: () => ({}) }
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    const composer = container.querySelector<HTMLTextAreaElement>('#promptComposer')!
    const type = (text: string): void => { Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set?.call(composer, text); composer.dispatchEvent(new Event('input', { bubbles: true })) }
    await act(async () => {
      const picker = container!.querySelector<HTMLSelectElement>(headSelection ? '[aria-label="Choose continuation"]' : '[aria-label="Conversation"]')!
      picker.value = headSelection ? id(3) : id(2); picker.dispatchEvent(new Event('change', { bubbles: true }))
    })
    await act(async () => { type('Synthetic pending selection text') })
    expect(saves).toEqual([])
    expect(container.querySelector<HTMLButtonElement>('button[type="submit"]')?.disabled).toBe(true)
    await act(async () => { await vi.advanceTimersByTimeAsync(250); type('Synthetic newer recovery text'); await vi.advanceTimersByTimeAsync(500) })
    expect(recoveryReads).toBe(3)
    expect(saves).toEqual([])
    expect(composer.value).toBe('Synthetic newer recovery text')
    expect(container.querySelector<HTMLButtonElement>('button[type="submit"]')?.disabled).toBe(true)
    if (exhaustRecovery) {
      expect(container.textContent).toContain('selection recovery required')
      await act(async () => { type('Synthetic after recovery exhaustion') })
      expect(saves).toEqual([])
      permitResume = true
      await act(async () => { window.dispatchEvent(new Event('focus')) })
    } else await act(async () => { settleCanonical() })
    const text = exhaustRecovery ? 'Synthetic after recovery exhaustion' : 'Synthetic newer recovery text'
    expect(composer.value).toBe(text)
    expect(saves).toEqual([{ conversation: headSelection ? id(1) : id(2), head: headSelection ? id(3) : null, text }])
    expect(container.textContent).toContain('Native conversation status: not saved')
    expect(invoke.mock.calls.filter(([command]) => command === selectionCommand)).toHaveLength(1)
    expect(invoke.mock.calls.some(([command]) => command === 'submit_prompt')).toBe(false)
  })
  it.each([
    ['select_conversation', false], ['select_conversation_head', false],
    ['select_conversation', true], ['select_conversation_head', true],
  ] as const)('preserves and saves typing across deferred %s (native failure: %s)', async (selectionCommand, failSelection) => {
    const id = (sequence: number): string => `11111111-1111-4111-8111-111111111111:${sequence}`
    const headSelection = selectionCommand === 'select_conversation_head'
    let selected = id(1)
    let selectedHead: string | null = null
    const drafts = new Map([[id(1), 'Stored synthetic old draft'], [id(2), 'Stored synthetic destination draft']])
    const saves: Array<{ conversation: string; text: string }> = []
    let completeOldSave: () => void = () => undefined
    let completeSelection: () => void = () => undefined
    let selectionStarted = false
    const snapshot = () => ({ ...appSnapshot(), session_id: selected, assistant: {
      revision: '3', conversations: [{ id: id(1), title: 'Synthetic old conversation' }, { id: id(2), title: 'Synthetic destination conversation' }],
      selected_head: selectedHead, branch_choices: headSelection && selectedHead === null ? [id(3), id(4)] : [], draft: drafts.get(selected), interrupted: false,
    } })
    window.__TAURI_INTERNALS__ = { invoke: async (command, args) => {
      if (command === 'get_app_snapshot') return snapshot()
      if (command === 'get_startup_state') return readyStartupState()
      if (command === 'save_assistant_draft') {
        const save = { conversation: selected, text: (args as { text: string }).text }
        saves.push(save)
        if (saves.length === 1) return new Promise((resolve) => { completeOldSave = () => { drafts.set(save.conversation, save.text); resolve(null) } })
        drafts.set(save.conversation, save.text)
        return null
      }
      if (command === selectionCommand) {
        selectionStarted = true
        return new Promise((resolve, reject) => { completeSelection = () => {
          if (failSelection) { reject({ code: 'busy', message: 'Synthetic selection could not settle' }); return }
          if (headSelection) selectedHead = (args as { headId: string }).headId
          else selected = (args as { conversationId: string }).conversationId
          resolve(snapshot())
        } })
      }
      return null
    } }
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    const composer = container.querySelector<HTMLTextAreaElement>('#promptComposer')!
    const type = (text: string): void => { Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set?.call(composer, text); composer.dispatchEvent(new Event('input', { bubbles: true })) }
    await act(async () => { type('Synthetic before selection') })
    await act(async () => {
      const picker = container!.querySelector<HTMLSelectElement>(headSelection ? '[aria-label="Choose continuation"]' : '[aria-label="Conversation"]')!
      picker.value = headSelection ? id(3) : id(2)
      picker.dispatchEvent(new Event('change', { bubbles: true }))
    })
    expect(selectionStarted).toBe(false)
    await act(async () => { type('Synthetic while prior save is pending'); completeOldSave() })
    expect(selectionStarted).toBe(true)
    await act(async () => { type('Synthetic while native selection is pending') })
    await act(async () => { completeSelection() })
    expect(composer.value).toBe('Synthetic while native selection is pending')
    const target = !headSelection && !failSelection ? id(2) : id(1)
    expect(saves).toEqual([
      { conversation: id(1), text: 'Synthetic before selection' },
      { conversation: target, text: 'Synthetic while native selection is pending' },
    ])
    expect(drafts.get(target)).toBe('Synthetic while native selection is pending')
    expect(drafts.get(id(2))).toBe(target === id(2) ? 'Synthetic while native selection is pending' : 'Stored synthetic destination draft')
  })
  it('queues cold-hydration typing for native persistence before switching away and restoring it', async () => {
    const id = (sequence: number): string => `11111111-1111-4111-8111-111111111111:${sequence}`
    const drafts = new Map([[id(1), ''], [id(2), '']])
    let selected = id(1)
    let initial: (value: unknown) => void = () => undefined
    let first = true
    const snapshot = () => ({ ...appSnapshot(), session_id: selected, assistant: {
      revision: '2', conversations: [{ id: id(1), title: 'Synthetic first conversation' }, { id: id(2), title: 'Synthetic second conversation' }],
      selected_head: null, branch_choices: [], draft: drafts.get(selected), interrupted: false,
    } })
    const invoke = vi.fn(async (command: string, args?: unknown) => {
      if (command === 'get_app_snapshot') { if (first) { first = false; return new Promise((resolve) => { initial = resolve }) } return snapshot() }
      if (command === 'save_assistant_draft') { drafts.set(selected, (args as { text: string }).text); return null }
      if (command === 'select_conversation') { selected = (args as { conversationId: string }).conversationId; return snapshot() }
      if (command === 'get_startup_state') return readyStartupState()
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke }
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    const composer = container.querySelector<HTMLTextAreaElement>('#promptComposer')!
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set?.call(composer, 'Synthetic cold typing')
      composer.dispatchEvent(new Event('input', { bubbles: true }))
      initial(snapshot())
    })
    expect(composer.value).toBe('Synthetic cold typing')
    const picker = container.querySelector<HTMLSelectElement>('[aria-label="Conversation"]')!
    await act(async () => { picker.value = id(2); picker.dispatchEvent(new Event('change', { bubbles: true })) })
    expect(drafts.get(id(1))).toBe('Synthetic cold typing')
    await act(async () => { picker.value = id(1); picker.dispatchEvent(new Event('change', { bubbles: true })) })
    expect(composer.value).toBe('Synthetic cold typing')
  })

  it('passes the visible follow-up into callback-settled New while retirement is delayed', async () => {
    const id = (sequence: number): string => `11111111-1111-4111-8111-111111111111:${sequence}`
    let selected = id(1)
    let busy = false
    let finishPrompt: (value: unknown) => void = () => undefined
    let retireAndNew: () => void = () => undefined
    const drafts = new Map([[id(1), ''], [id(2), '']])
    const snapshot = () => ({ ...appSnapshot(), session_id: selected, assistant: {
      revision: '2', conversations: [{ id: id(1), title: 'Synthetic old conversation' }, { id: id(2), title: 'Synthetic new conversation' }],
      selected_head: null, branch_choices: [], draft: drafts.get(selected), interrupted: false,
    } })
    const invoke = vi.fn(async (command: string, args?: unknown) => {
      if (command === 'get_app_snapshot') return snapshot()
      if (command === 'get_startup_state') return readyStartupState()
      if (command === 'submit_prompt') { busy = true; return new Promise((resolve) => { finishPrompt = resolve }) }
      if (command === 'cancel_prompt') return true
      if (command === 'save_assistant_draft') { if (busy) throw { code: 'busy', message: 'Synthetic retirement is pending' }; drafts.set(selected, (args as { text: string }).text); return null }
      if (command === 'reset_session') return new Promise((resolve) => { retireAndNew = () => { busy = false; drafts.set(selected, (args as { draft: string }).draft); selected = id(2); finishPrompt({ kind: 'cancelled' }); resolve(snapshot()) } })
      if (command === 'select_conversation') { selected = (args as { conversationId: string }).conversationId; return snapshot() }
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke, createChannel: () => ({}) }
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    const composer = container.querySelector<HTMLTextAreaElement>('#promptComposer')!
    const type = (text: string): void => { Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set?.call(composer, text); composer.dispatchEvent(new Event('input', { bubbles: true })) }
    await act(async () => { type('Synthetic first prompt') })
    await act(async () => { container!.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    await act(async () => { type('Synthetic visible follow-up') })
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="New Chat"]')!.click(); await Promise.resolve(); await Promise.resolve() })
    expect(invoke).toHaveBeenCalledWith('reset_session', { draft: 'Synthetic visible follow-up' })
    expect(busy).toBe(true)
    await act(async () => { retireAndNew() })
    expect(drafts.get(id(1))).toBe('Synthetic visible follow-up')
    const picker = container.querySelector<HTMLSelectElement>('[aria-label="Conversation"]')!
    await act(async () => { picker.value = id(1); picker.dispatchEvent(new Event('change', { bubbles: true })) })
    expect(composer.value).toBe('Synthetic visible follow-up')
    expect(container.textContent).not.toContain('New chat failed')
  })
  it('loads another bounded native conversation page without truncating older history', async () => {
    const id = (sequence: number): string => `11111111-1111-4111-8111-111111111111:${sequence}`
    const invoke = vi.fn(async (command: string) => {
      if (command === 'get_app_snapshot') return { ...appSnapshot(), session_id: id(1), assistant: {
        revision: '1', conversations: Array.from({ length: 128 }, (_, index) => ({ id: id(index + 1), title: `Synthetic conversation ${index + 1}` })),
        conversation_count: 129, conversation_offset: 128, selected_head: null, branch_choices: [], draft: '', interrupted: false,
      } }
      if (command === 'list_assistant_conversations') return { revision: '1', conversations: [{ id: id(129), title: 'Synthetic older conversation' }], next_offset: 129 }
      if (command === 'get_startup_state') return readyStartupState()
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke }
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    await act(async () => { Array.from(container!.querySelectorAll('button')).find((button) => button.textContent === 'More conversations')!.click() })
    expect(invoke).toHaveBeenCalledWith('list_assistant_conversations', { offset: 128, expectedRevision: '1' })
    expect(container.querySelector<HTMLOptionElement>(`option[value="${id(129)}"]`)?.textContent).toBe('Synthetic older conversation')
    expect(container.textContent).not.toContain('More conversations')
  })
  it('adopts synchronized canonical history and the saved draft on foreground resume', async () => {
    const id = '11111111-1111-4111-8111-111111111111:1'
    let wire = { ...appSnapshot(), transcript: [] as readonly Record<string, unknown>[], session_id: id, assistant: {
      revision: '1', conversations: [{ id, title: 'Synthetic conversation' }], selected_head: null,
      branch_choices: [], draft: 'Saved synthetic draft', interrupted: true,
    } }
    window.__TAURI_INTERNALS__ = { invoke: async (command) => {
      if (command === 'get_app_snapshot') return wire
      if (command === 'get_startup_state') return readyStartupState()
      return null
    } }
    container = document.createElement('div')
    document.body.append(container)
    root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    expect(container.querySelector<HTMLTextAreaElement>('#promptComposer')?.value).toBe('Saved synthetic draft')
    const turn = canonicalTurn('2', '3', 'Watch synthetic question', 'Watch synthetic answer', 'synthetic-request')
    wire = { ...wire, transcript: [turn], assistant: { ...wire.assistant, revision: '2', draft: 'Restored next draft' } }
    await act(async () => { window.dispatchEvent(new Event('focus')); await Promise.resolve(); await Promise.resolve() })
    expect(container.textContent).toContain('Watch synthetic answer')
    expect(container.querySelector<HTMLTextAreaElement>('#promptComposer')?.value).toBe('Restored next draft')
  })

  it('requires explicit continuation selection and never concatenates sibling branches', async () => {
    const id = '11111111-1111-4111-8111-111111111111:1'
    const head = '11111111-1111-4111-8111-111111111111:3'
    const commands: string[] = []
    let wire = { ...appSnapshot(), session_id: id, assistant: {
      revision: '1', conversations: [{ id, title: 'Synthetic fork' }], selected_head: null as string | null,
      branch_choices: [head], draft: 'Continue synthetic branch', interrupted: false,
    } }
    window.__TAURI_INTERNALS__ = { invoke: async (command) => {
      commands.push(command)
      if (command === 'get_app_snapshot') return wire
      if (command === 'select_conversation_head') {
        wire = { ...wire, assistant: { ...wire.assistant, selected_head: head, branch_choices: [] } }
        return wire
      }
      if (command === 'get_startup_state') return readyStartupState()
      return null
    } }
    container = document.createElement('div')
    document.body.append(container)
    root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    expect(container.textContent).toContain('Choose continuation')
    expect(container.querySelector<HTMLButtonElement>('button[type="submit"]')?.disabled).toBe(true)
    const chooser = container.querySelector<HTMLSelectElement>('[aria-label="Choose continuation"]')
    expect(chooser).not.toBeNull()
    await act(async () => { chooser!.value = head; chooser!.dispatchEvent(new Event('change', { bubbles: true })) })
    expect(commands).toContain('select_conversation_head')
    expect(container.querySelector<HTMLButtonElement>('button[type="submit"]')?.disabled).toBe(false)
  })

  it('persists typed drafts and does not overwrite newer typing with a stale resume snapshot', async () => {
    const id = '11111111-1111-4111-8111-111111111111:1'
    const wire = { ...appSnapshot(), session_id: id, assistant: {
      revision: '1', conversations: [{ id, title: 'Synthetic conversation' }], selected_head: null,
      branch_choices: [], draft: 'Saved draft', interrupted: false,
    } }
    let calls = 0
    let resume: (value: unknown) => void = () => undefined
    const saved: string[] = []
    window.__TAURI_INTERNALS__ = { invoke: async (command, args) => {
      if (command === 'get_app_snapshot') {
        calls += 1
        if (calls === 1) return wire
        return new Promise((resolve) => { resume = resolve })
      }
      if (command === 'save_assistant_draft') { saved.push((args as { text: string }).text); return null }
      if (command === 'get_startup_state') return readyStartupState()
      return null
    } }
    container = document.createElement('div')
    document.body.append(container)
    root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    await act(async () => { window.dispatchEvent(new Event('focus')); await Promise.resolve() })
    const composer = container.querySelector<HTMLTextAreaElement>('#promptComposer')!
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set?.call(composer, 'Newer synthetic typing')
      composer.dispatchEvent(new Event('input', { bubbles: true }))
      await Promise.resolve()
    })
    expect(saved).toContain('Newer synthetic typing')
    await act(async () => { resume(wire); await Promise.resolve(); await Promise.resolve() })
    expect(composer.value).toBe('Newer synthetic typing')
  })
  it('uses snapshot membership for both Instant selectors, not the desktop catalog or a fixed Android list', async () => {
    const snapshot = { ...appSnapshot(), model_options: [
      { id: 'custom-sol-high', label: 'Synthetic Sol High', available: true, unavailable_reason: null },
      { id: 'custom-sol-none', label: 'Synthetic Sol None', available: false, unavailable_reason: 'Synthetic model unavailable' },
      { id: 'custom-luna-none', label: 'Synthetic Luna None', available: true, unavailable_reason: null },
      { id: 'custom-luna-low', label: 'Synthetic Luna Low', available: true, unavailable_reason: null },
    ] }
    window.__TAURI_INTERNALS__ = { invoke: async (command) => {
      if (command === 'get_app_snapshot') return snapshot
      if (command === 'get_startup_state') return readyStartupState()
      return null
    } }
    container = document.createElement('div')
    document.body.append(container)
    root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    for (let index = 0; index < 5; index += 1) await act(async () => { await Promise.resolve() })
    await act(async () => { container?.querySelector<HTMLButtonElement>('[aria-label="Settings"]')?.click() })
    for (const id of ['composerInstantSelect', 'assistantInstantSelect']) {
      const select = container.querySelector<HTMLSelectElement>(`#${id}`)!
      expect(select).not.toBeNull()
      expect(Array.from(select.options).map((option) => option.value)).toEqual([
        'custom-sol-high', 'custom-sol-none', 'custom-luna-none', 'custom-luna-low',
      ])
      const unavailable = select.querySelector<HTMLOptionElement>('[value="custom-sol-none"]')!
      expect(unavailable.disabled).toBe(true)
      expect(unavailable.textContent).toContain('Synthetic model unavailable')
    }
    snapshot.model_options = snapshot.model_options.filter((option) => option.id === 'custom-sol-high')
    await act(async () => { window.dispatchEvent(new Event('focus')); await Promise.resolve(); await Promise.resolve() })
    expect(container.querySelector<HTMLSelectElement>('#composerInstantSelect')?.options.length).toBe(1)
  })

  it('adds a separate voice-notes entry without enabling unsupported voice controls', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') return appSnapshot()
        if (command === 'get_startup_state') return readyStartupState()
        return null
      },
    }
    container = document.createElement('div')
    document.body.append(container)
    root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    for (let index = 0; index < 5; index += 1) await act(async () => { await Promise.resolve() })

    expect(container.textContent).not.toContain('Start mic')
    expect(container.querySelector('header button[aria-label="Voice notes"]')).not.toBeNull()
    expect(container.querySelector('#promptComposer')).not.toBeNull()
  })

  it.each(['Provider access is not available', 'Provider access has expired', 'Provider rejected access'])('keeps the provider error %s without credential-management instructions', async (reason) => {
    const unavailable = capabilities().map((capability) => capability['id'] === 'custom_provider'
      ? { ...capability, state: 'unavailable', reason }
      : capability)
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') return { ...appSnapshot(), capabilities: unavailable, model_options: [{ id: 'custom-sol-high', label: 'Sol High', available: false, unavailable_reason: reason }] }
        if (command === 'get_startup_state') return { ...readyStartupState(), capabilities: unavailable }
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
    expect(container.textContent).toContain(reason)
    expect(container.querySelector('[aria-label="Retry Custom"]')).toBeNull()
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="Settings"]')!.click() })
    expect(container.textContent).toContain(`Custom connection: ${reason}`)
    expect(container.textContent).not.toMatch(/import.*credential|copy.*credential|renew|log in/i)
  })

  it('retains ordinary Android settings without credential controls, identity labels or polling', async () => {
    vi.useFakeTimers()
    const commands: string[] = []
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        commands.push(command)
        if (command === 'get_app_snapshot') return appSnapshot()
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
      container?.querySelector<HTMLButtonElement>('[aria-label="Settings"]')?.click()
      await Promise.resolve()
    })
    expect(commands).not.toContain('list_audio_input_devices')
    expect(container.querySelector('#audioInputDevice')).toBeNull()
    expect(container.querySelector('#assistantInstantSelect')).not.toBeNull()
    expect(container.textContent).toContain('Voice input')
    expect(container.textContent).toContain('Speech output')
    expect(container.querySelector('header button[aria-label="Voice notes"]')).not.toBeNull()
    expect(container.textContent).toContain('Add Ask to Home screen')
    expect(container.querySelector('[aria-label="Android access credentials"]')).toBeNull()
    expect(container.textContent).not.toMatch(/access credential|credential sharing|installation id|replica id|association id|automatic watch sharing/i)
    await act(async () => { await vi.advanceTimersByTimeAsync(5000) })
    for (const command of ['credential_status', 'import_access_credential', 'retry_credential_transfer', 'assistant_pairing_status']) expect(commands).not.toContain(command)
  })

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
    // Android derives startup from the accepted snapshot without another IPC.
    expect(startupCalls).toBe(0)
    expect(container.querySelector('.shell')?.getAttribute('data-runtime-status')).toBe('sleeping')
    expect(container.querySelector<HTMLSelectElement>('#composerInstantSelect')?.disabled).toBe(false)
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

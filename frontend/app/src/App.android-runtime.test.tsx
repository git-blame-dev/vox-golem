import { StrictMode, act } from 'react'
import { createRoot } from 'react-dom/client'
import type { Root } from 'react-dom/client'
import { afterEach, describe, expect, it, vi } from 'vitest'

let root: Root | null = null
let container: HTMLElement | null = null

afterEach(() => {
  act(() => root?.unmount())
  root = null
  container?.remove()
  container = null
  delete window.__TAURI_INTERNALS__
  vi.useRealTimers()
  vi.unstubAllEnvs()
  vi.restoreAllMocks()
  vi.resetModules()
})

describe('production Android App runtime', () => {
  it.each([
    { streamed: true, enabled: false }, { streamed: false, enabled: false },
    { streamed: true, enabled: true }, { streamed: false, enabled: true },
  ])('keeps Android timing on the frontend admission clock (streamed=$streamed, telemetry=$enabled)', async ({ streamed, enabled }) => {
    vi.stubEnv('VITE_VOXGOLEM_RUNTIME', 'android'); vi.resetModules(); window.history.replaceState(null, '')
    vi.stubEnv('VITE_VOXGOLEM_TIMING', enabled ? '1' : '0')
    delete document.documentElement.dataset['appReadyToSendMs']
    delete (window as unknown as { __appReadyToSendMs?: number }).__appReadyToSendMs
    let now = 100
    vi.spyOn(performance, 'now').mockImplementation(() => now)
    let frame: FrameRequestCallback | undefined
    vi.spyOn(window, 'requestAnimationFrame').mockImplementation((callback) => { frame = callback; return 1 })
    const log = vi.spyOn(console, 'info').mockImplementation((_label, value) => {
      const metadata = typeof value === 'string' ? JSON.parse(value) as Record<string, unknown> : undefined
      if (metadata?.['stage'] === 'FIRST_TEXT_DOM_COMMITTED') {
        expect(Array.from(container!.querySelectorAll('.answer-stage__answer')).some((answer) => answer.textContent?.trim() === 'Synthetic answer')).toBe(true)
      }
    })
    let updates: (value: unknown) => void = () => undefined
    let finish: (value: unknown) => void = () => undefined
    let requestId = ''
    let transcript: unknown[] = [{ request_id: 'synthetic-restored-request', execution_id: { session_id: '1', generation: '1' }, user: { id: '1', role: 'user', content: { kind: 'text', text: 'Synthetic restored question' } }, answer: { id: '2', role: 'assistant', content: { kind: 'text', text: 'Synthetic restored answer' } }, final_stage: 'instant' }]
    const invoke = vi.fn(async (command: string, args?: unknown) => {
      if (command === 'get_app_snapshot') return { ...wireSnapshot(), transcript }
      if (command === 'submit_prompt') {
        requestId = (args as { requestId: string }).requestId
        updates = (args as { updates: { send: (value: unknown) => void } }).updates.send
        return new Promise((resolve) => { finish = resolve })
      }
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke, createChannel: <T,>(send: (value: T) => void) => ({ send }) }
    const { default: App } = await import('./App')
    const { FrontendSession } = await import('./FrontendSession')
    const session = new FrontendSession({ android: true, nativeSearch: true })
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App session={session} />) })
    expect(invoke.mock.calls.filter(([command, args]) => command === 'report_startup_stage' && (args as { stage: string }).stage === 'first_response_dom')).toHaveLength(0)
    expect(document.documentElement.dataset['appReadyToSendMs']).toBe('100')
    const input = container.querySelector<HTMLTextAreaElement>('#promptComposer')!
    await act(async () => { Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set?.call(input, 'Synthetic timing question'); input.dispatchEvent(new Event('input', { bubbles: true })) })
    await act(async () => { container!.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    now = 125
    await act(async () => { updates({ kind: 'status', message: 'Thinking' }) })
    expect(container.querySelector('.message__thinking')).not.toBeNull()
    expect(log.mock.calls.some(([, metadata]) => typeof metadata === 'string' && JSON.parse(metadata)['stage'] === 'FIRST_NONBLANK_RECEIVED')).toBe(false)
    now = 150
    await act(async () => { updates({ kind: 'delta', text: ' \n' }) })
    expect(container.querySelector('.conversation__timings')).toBeNull()
    await act(async () => { frame?.(now) })
    expect(invoke.mock.calls.filter(([command, args]) => command === 'report_startup_stage' && (args as { stage: string }).stage === 'first_response_dom')).toHaveLength(0)
    if (streamed) {
      now = 200
      await act(async () => { updates({ kind: 'delta', text: 'Synthetic answer' }) })
      expect(invoke.mock.calls.filter(([command, args]) => command === 'report_startup_stage' && (args as { stage: string }).stage === 'first_response_dom')).toHaveLength(0)
      now = 225
      await act(async () => { frame?.(now) })
      now = 250
      await act(async () => { updates({ kind: 'custom_direct_timings', first_text_ms: 7, completed_ms: 900_009 }) })
      expect(container.querySelector('.conversation__timings')?.textContent).toBe('100 ms')
    }
    now = 400
    const turn = { request_id: requestId, execution_id: { session_id: '1', generation: '2' }, user: { id: '3', role: 'user', content: { kind: 'text', text: 'Synthetic timing question' } }, answer: { id: '4', role: 'assistant', content: { kind: 'text', text: 'Synthetic answer' } }, final_stage: 'instant' }
    transcript = [...transcript, turn]
    await act(async () => { finish({ kind: 'completed', turn, first_text_ms: 7, completed_ms: 900_009 }) })
    const answer = session.getMessagesSnapshot().find((message) => message.id === '4')
    expect(answer?.customDirectTimings).toEqual({ firstTextMs: streamed ? 100 : 300, completedMs: 300 })
    const stages = invoke.mock.calls.filter(([command]) => command === 'report_startup_stage').map(([, args]) => (args as { stage: string }).stage)
    expect(stages).toEqual(enabled ? ['frontend_ready', 'first_response_dom'] : [])
    const metadata = log.mock.calls.filter(([label]) => label === '[frontend-timing]').map(([, value]) => JSON.parse(value as string) as Record<string, unknown>)
    if (enabled) {
      expect(metadata.filter((entry) => entry['stage'] === 'FIRST_NONBLANK_RECEIVED')).toEqual([{ scope: 'frontend_request', request: 1, stage: 'FIRST_NONBLANK_RECEIVED', atMs: streamed ? 200 : 400, elapsedMs: streamed ? 100 : 300 }])
      expect(metadata.filter((entry) => entry['stage'] === 'FIRST_TEXT_DOM_COMMITTED')).toEqual([{ scope: 'frontend_request', request: 1, stage: 'FIRST_TEXT_DOM_COMMITTED', atMs: streamed ? 225 : 400, elapsedMs: streamed ? 125 : 300 }])
      expect(metadata.filter((entry) => entry['stage'] === 'SETTLED')).toEqual([{ scope: 'frontend_request', request: 1, stage: 'SETTLED', atMs: 400, elapsedMs: 300 }])
      expect(metadata.filter((entry) => entry['scope'] === 'provider_request')).toHaveLength(0)
      expect(metadata.filter((entry) => entry['scope'] === 'webview_navigation').map((entry) => entry['stage'])).toEqual(['SHELL_DOM_COMMITTED', 'INITIAL_SNAPSHOT_REQUESTED', 'INITIAL_SNAPSHOT_RECEIVED', 'INITIAL_SNAPSHOT_APPLIED', 'INITIAL_SEARCH_MODE_REQUESTED', 'INITIAL_SEARCH_MODE_ACK', 'STATE_READY'])
      expect(JSON.stringify(metadata)).not.toContain('Synthetic')
      expect(JSON.stringify(metadata)).not.toContain(requestId)
    } else expect(log).not.toHaveBeenCalled()
  })

  it.each(['branch', 'storage'] as const)('does not label blocked %s admission as startup ready', async (blocked) => {
    vi.stubEnv('VITE_VOXGOLEM_RUNTIME', 'android'); vi.stubEnv('VITE_VOXGOLEM_TIMING', '1')
    vi.resetModules(); window.history.replaceState(null, '')
    delete document.documentElement.dataset['appReadyToSendMs']
    delete (window as unknown as { __appReadyToSendMs?: number }).__appReadyToSendMs
    const log = vi.spyOn(console, 'info').mockImplementation(() => undefined)
    window.__TAURI_INTERNALS__ = { invoke: async (command) => {
      if (command === 'get_app_snapshot') return { ...wireSnapshot(), assistant: {
        revision: '1', conversations: [], selected_head: null, branch_choices: blocked === 'branch' ? ['2'] : [],
        draft: '', interrupted: false, error: blocked === 'storage' ? 'storage_error' : null,
      } }
      return null
    } }
    const { default: App } = await import('./App')
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    expect(log.mock.calls.some(([label, metadata]) => label === '[frontend-timing]' && JSON.parse(metadata as string)['stage'] === 'STATE_READY')).toBe(false)
    expect(document.documentElement.dataset['appReadyToSendMs']).toBeUndefined()
  })

  it('preserves opted-in desktop provider diagnostics and provider-scoped badges', async () => {
    vi.stubEnv('VITE_VOXGOLEM_RUNTIME', 'desktop'); vi.stubEnv('VITE_VOXGOLEM_TIMING', '1')
    vi.resetModules(); window.history.replaceState(null, '')
    const log = vi.spyOn(console, 'info').mockImplementation(() => undefined)
    let transcript: unknown[] = []
    window.__TAURI_INTERNALS__ = {
      createChannel: <T,>(send: (value: T) => void) => ({ send }),
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') return { ...wireSnapshot(), transcript }
        if (command === 'get_startup_state') return desktopStartupState()
        if (command === 'submit_prompt') {
          const { requestId, updates } = args as { requestId: string; updates: { send: (value: unknown) => void } }
          updates.send({ kind: 'text', text: 'Synthetic desktop answer' })
          updates.send({ kind: 'custom_direct_timings', first_text_ms: 450, completed_ms: 1250 })
          transcript = [{ request_id: requestId, execution_id: { session_id: '1', generation: '1' }, user: { id: '1', role: 'user', content: { kind: 'text', text: 'Synthetic desktop question' } }, answer: { id: '2', role: 'assistant', content: { kind: 'text', text: 'Synthetic desktop answer' } }, final_stage: 'instant' }]
          return { request_id: requestId, outcome: 'completed', runtime_phase: 'sleeping' }
        }
        return null
      },
    }
    const { default: App } = await import('./App')
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    const input = container.querySelector<HTMLTextAreaElement>('#promptComposer')!
    await act(async () => { Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set?.call(input, 'Synthetic desktop question'); input.dispatchEvent(new Event('input', { bubbles: true })) })
    await act(async () => { container!.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    expect(container.querySelector('.conversation__timings')?.textContent).toBe('450 ms')
    const provider = log.mock.calls.filter(([label]) => label === '[frontend-timing]').map(([, value]) => JSON.parse(value as string) as Record<string, unknown>).filter((metadata) => metadata['scope'] === 'provider_request')
    expect(provider).toEqual([{ scope: 'provider_request', request: 1, stage: 'PROVIDER_DURATIONS', firstTextMs: 450, completedMs: 1250 }])
  })

  it.each([false, true])('fences retired history but preserves unchanged context after New Chat (rejected=%s)', async (rejected) => {
    vi.stubEnv('VITE_VOXGOLEM_RUNTIME', 'android'); vi.stubEnv('VITE_VOXGOLEM_TIMING', '1')
    vi.resetModules(); window.history.replaceState(null, '')
    let now = 100
    vi.spyOn(performance, 'now').mockImplementation(() => now)
    const log = vi.spyOn(console, 'info').mockImplementation(() => undefined)
    let sessionId = '1'
    let firstTurn: Record<string, unknown> | undefined
    let freshTurn: Record<string, unknown> | undefined
    let finishFirst: (value: unknown) => void = () => undefined
    let releaseReconciliation: (value: unknown) => void = () => undefined
    let holdReconciliation = false
    const snapshot = () => ({ ...wireSnapshot(), session_id: sessionId,
      transcript: sessionId === '1' ? [firstTurn, freshTurn].filter((turn) => turn !== undefined) : [],
      assistant: { revision: '1', conversations: [{ id: '1', title: 'Synthetic original' }, { id: '2', title: 'Synthetic new' }], selected_head: null, branch_choices: [], draft: '', interrupted: false, error: null },
    })
    const turn = (requestId: string, fresh: boolean) => ({ request_id: requestId, execution_id: { session_id: '1', generation: fresh ? '2' : '1' },
      user: { id: fresh ? '13' : '11', role: 'user', content: { kind: 'text', text: 'Synthetic question' } },
      answer: { id: fresh ? '14' : '12', role: 'assistant', content: { kind: 'text', text: fresh ? 'Synthetic fresh answer' : 'Synthetic old answer' } }, final_stage: 'instant' })
    const invoke = vi.fn(async (command: string, args?: unknown) => {
      if (command === 'get_app_snapshot') {
        if (holdReconciliation) { holdReconciliation = false; return new Promise((resolve) => { releaseReconciliation = resolve }) }
        return snapshot()
      }
      if (command === 'submit_prompt') {
        const requestId = (args as { requestId: string }).requestId
        if (firstTurn === undefined) {
          firstTurn = turn(requestId, false)
          holdReconciliation = true
          return new Promise((resolve) => { finishFirst = resolve })
        }
        now = 900
        freshTurn = turn(requestId, true)
        return { kind: 'completed', turn: freshTurn }
      }
      if (command === 'reset_session') {
        if (rejected) throw { code: 'internal', message: 'Synthetic reset rejected without changing context' }
        sessionId = '2'; return snapshot()
      }
      if (command === 'select_conversation') { sessionId = (args as { conversationId: string }).conversationId; return snapshot() }
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke, createChannel: <T,>(send: (value: T) => void) => ({ send }) }
    const { default: App } = await import('./App')
    const { FrontendSession } = await import('./FrontendSession')
    const session = new FrontendSession({ android: true, nativeSearch: true })
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App session={session} />) })
    const submit = async () => {
      const input = container!.querySelector<HTMLTextAreaElement>('#promptComposer')!
      await act(async () => { Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set?.call(input, 'Synthetic question'); input.dispatchEvent(new Event('input', { bubbles: true })) })
      await act(async () => { container!.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    }
    await submit()
    now = 200
    await act(async () => { finishFirst({ kind: 'completed', turn: firstTurn }) })
    expect(session.getControlsSnapshot().promptState).toBe('reconciling')
    expect(container.querySelector('.answer-stage__answer')?.textContent).not.toBe('Synthetic old answer')
    now = 300
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="New Chat"]')!.click() })
    expect(session.getControlsSnapshot().promptState).toBe('idle')
    now = 400
    await act(async () => { await session.intents.selectAndroidConversation('select_conversation', '1') })
    expect(container.querySelector('.answer-stage__answer')?.textContent).toBe('Synthetic old answer')
    const commits = () => log.mock.calls.filter(([label]) => label === '[frontend-timing]').map(([, value]) => JSON.parse(value as string) as Record<string, unknown>).filter((metadata) => metadata['stage'] === 'FIRST_TEXT_DOM_COMMITTED')
    const nativeCommits = () => invoke.mock.calls.filter(([command, args]) => command === 'report_startup_stage' && (args as { stage: string }).stage === 'first_response_dom')
    const preservedCommit = { scope: 'frontend_request', request: 1, stage: 'FIRST_TEXT_DOM_COMMITTED', atMs: 300, elapsedMs: 200 }
    expect(commits()).toEqual(rejected ? [preservedCommit] : [])
    expect(nativeCommits()).toHaveLength(rejected ? 1 : 0)
    await act(async () => { releaseReconciliation(snapshot()) })
    expect(commits()).toEqual(rejected ? [preservedCommit] : [])
    now = 700
    await submit()
    const freshCommit = { scope: 'frontend_request', request: 2, stage: 'FIRST_TEXT_DOM_COMMITTED', atMs: 900, elapsedMs: 200 }
    expect(commits()).toEqual(rejected ? [preservedCommit, freshCommit] : [freshCommit])
    expect(nativeCommits()).toHaveLength(1)
    expect(container.textContent).toContain('Synthetic fresh answer')
  })

  it.each(['android', 'desktop'] as const)('keeps one native Search selector with unchanged gating and accepted selection on %s', async (platform) => {
    vi.stubEnv('VITE_VOXGOLEM_RUNTIME', platform); vi.resetModules(); window.history.replaceState(null, '')
    let webEnabled = true
    let acceptMode: () => void = () => undefined
    const invoke = vi.fn(async (command: string, args?: unknown) => {
      if (command === 'get_app_snapshot') return { ...wireSnapshot(), preferences: { ...(wireSnapshot()['preferences'] as Record<string, unknown>), web_enabled: webEnabled } }
      if (command === 'get_startup_state') return desktopStartupState()
      if (command === 'set_search_mode') {
        if ((args as { mode: string }).mode === 'quick') return null
        return new Promise((resolve) => { acceptMode = () => resolve(null) })
      }
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke }
    const { default: App } = await import('./App')
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    const search = container.querySelector<HTMLSelectElement>('[aria-label="Search mode"]')!
    const promptRow = container.querySelector('.composer__prompt-row')!
    expect(container.querySelectorAll('[aria-label="Search mode"]')).toHaveLength(1)
    expect(container.querySelector('.shell')?.getAttribute('data-platform')).toBe(platform)
    if (platform === 'android') {
      expect(search.closest('.shell__header-actions')).not.toBeNull()
      expect(search.closest('.shell__header-actions')?.querySelector('[aria-label="Ask"]')).not.toBeNull()
      expect(container.querySelector('.composer__search')).toBeNull()
      expect(search.closest('.native-icon-select')?.querySelector('svg[data-icon-kind="search"]')).not.toBeNull()
      expect(search.closest('.search-mode')?.querySelector('.search-mode__label')).toBeNull()
      expect(search.getAttribute('title')).toBe('Search mode: Quick')
    } else {
      expect(container.querySelector('.composer__search')).toBeNull()
      expect(search.closest('.native-icon-select')).toBeNull()
      expect(search.closest('label')?.firstChild?.textContent).toBe('Search:')
      expect(promptRow.compareDocumentPosition(search) & Node.DOCUMENT_POSITION_FOLLOWING).not.toBe(0)
      expect(search.closest('.composer__actions')?.querySelector('button[type="submit"]')?.textContent).toBe('Send')
    }
    expect(search.value).toBe('quick')
    expect(search.disabled).toBe(false)
    await act(async () => { search.value = 'verify'; search.dispatchEvent(new Event('change', { bubbles: true })) })
    expect(search.disabled).toBe(true)
    expect(search.value).toBe('quick')
    expect(invoke).toHaveBeenCalledWith('set_search_mode', { mode: 'verify' })
    await act(async () => { acceptMode() })
    expect(search.value).toBe('verify')
    if (platform === 'android') expect(search.getAttribute('title')).toBe('Search mode: Verify')
    expect(search.disabled).toBe(false)
    webEnabled = false
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="New Chat"]')!.click() })
    expect(search.disabled).toBe(true)
    expect(search.value).toBe('quick')
    const reasonId = search.getAttribute('aria-describedby')!
    expect(reasonId).toBeTruthy()
    expect(document.getElementById(reasonId)?.textContent).toContain('Web is off. Normal chat remains available')
    expect(document.getElementById(reasonId)?.classList.contains('sr-only')).toBe(platform === 'android')
    if (platform === 'android') expect(search.getAttribute('title')).toContain('Web is off. Normal chat remains available')
  })

  it.each(['android', 'desktop'] as const)('compacts routine composer progress only on Android and retains detailed failures on %s', async (platform) => {
    vi.stubEnv('VITE_VOXGOLEM_RUNTIME', platform); vi.resetModules(); window.history.replaceState(null, '')
    let updates: (value: unknown) => void = () => undefined
    let finish: () => void = () => undefined
    window.__TAURI_INTERNALS__ = {
      createChannel: <T,>(send: (value: T) => void) => ({ send }),
      invoke: async (command, args) => {
        if (command === 'get_app_snapshot') return wireSnapshot()
        if (command === 'get_startup_state') return desktopStartupState()
        if (command === 'submit_prompt') {
          updates = (args as { updates: { send: (value: unknown) => void } }).updates.send
          return new Promise((resolve) => { finish = () => resolve({ kind: 'cancelled' }) })
        }
        return null
      },
    }
    const { default: App } = await import('./App')
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    const input = container.querySelector<HTMLTextAreaElement>('#promptComposer')!
    await act(async () => { Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set?.call(input, 'Synthetic compact status question'); input.dispatchEvent(new Event('input', { bubbles: true })) })
    await act(async () => { container!.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    const stop = container.querySelector<HTMLButtonElement>(platform === 'android' ? '.shell__header-actions button[aria-label="Stop"]' : '.composer__send-side button')!
    expect(stop).not.toBeNull()
    expect(stop.disabled).toBe(false)
    if (platform === 'android') expect(stop.querySelector('svg[aria-hidden="true"]')).not.toBeNull()
    else expect(stop.textContent).toBe('Stop')
    for (const message of ['Executing prompt…', 'Searching', 'Reading']) {
      if (message !== 'Executing prompt…') await act(async () => { updates({ kind: 'status', message }) })
      const status = container.querySelector('.composer__status')!
      expect(status.textContent).toBe(message)
      expect(status.getAttribute('role')).toBe('status')
      expect(status.getAttribute('aria-live')).toBe('polite')
      expect(status.classList.contains('composer__status--compact')).toBe(platform === 'android')
      if (platform === 'android') {
        expect(status.closest('.shell__header-actions')).not.toBeNull()
        expect(status.querySelector('.sr-only')?.textContent).toBe(message)
        expect(status.querySelector('svg[aria-hidden="true"]')).not.toBeNull()
        expect(status.getAttribute('title')).toBe(message)
      }
    }
    for (const [event, expected] of [
      [{ kind: 'stage', stage: 'instant', status: 'failed', detail: 'Synthetic stage failure' }, 'Instant stage failed'],
      [{ kind: 'correction', stage: 'instant', text: 'Synthetic corrected answer', correction: 'Correction: Synthetic actionable correction' }, 'Correction: Synthetic actionable correction'],
      ...(platform === 'desktop' ? [
        [{ kind: 'tool', tool: 'Synthetic tool', status: 'error', detail: 'Synthetic tool failure' }, 'Synthetic tool: error - Synthetic tool failure'],
        [{ kind: 'error', message: 'Synthetic actionable provider failure' }, 'Synthetic actionable provider failure'],
      ] as const : []),
    ] as const) {
      await act(async () => { updates(event) })
      const failure = container.querySelector('.composer__status')!
      expect(failure.textContent).toBe(expected)
      expect(failure.classList.contains('composer__status--compact')).toBe(false)
      expect(failure.querySelector('.sr-only')).toBeNull()
    }
    await act(async () => { stop.click() })
    expect(stop.disabled).toBe(true)
    if (platform === 'android') expect(stop.getAttribute('aria-label')).toBe('Stopping…')
    else expect(stop.textContent).toBe('Stopping…')
    await act(async () => { finish() })
  })

  it.each([
    { kind: 'engine', label: 'TTS engine', value: 'synthetic.tts.b', field: 'speech_tts_provider', changed: 'synthetic.tts.b', original: 'synthetic.tts.a' },
    { kind: 'voice', label: 'TTS voice', value: 'synthetic-a-other', field: 'speech_tts_voice', changed: 'synthetic-a-other', original: 'synthetic-a-local' },
    { kind: 'language', label: 'Speech language', value: 'en-GB', field: 'speech_language', changed: 'en-GB', original: 'en-AU' },
    { kind: 'all Automatic', label: 'Use Automatic speech settings', field: 'speech_tts_voice', changed: '', original: 'synthetic-a-local' },
    { kind: 'fallback', label: 'Allow speech network fallback', field: 'speech_network_fallback', changed: false, original: true },
    { kind: 'web', label: 'Allow web access', field: 'web_enabled', changed: false, original: true },
  ])('keeps rejected $kind writes visible in Voice notes Settings until a retry resolves them', async ({ kind, label, value, field, changed, original }) => {
    vi.useFakeTimers(); vi.stubEnv('VITE_VOXGOLEM_RUNTIME', 'android'); vi.resetModules(); window.history.replaceState(null, '')
    vi.spyOn(window.history, 'back').mockImplementation(() => { window.history.replaceState(null, ''); window.dispatchEvent(new PopStateEvent('popstate', { state: null })) })
    const baseline: Record<string, unknown> = { ...(wireSnapshot() as { preferences: Record<string, unknown> }).preferences, web_enabled: true, speech_network_fallback: true, speech_language: 'en-AU', speech_tts_provider: 'synthetic.tts.a', speech_tts_voice: 'synthetic-a-local' }
    let preferences = baseline
    const proposals: Record<string, unknown>[] = []
    let settle: (accepted: boolean, message?: string) => void = () => undefined
    window.__TAURI_INTERNALS__ = { invoke: async (command, args) => {
      if (command === 'get_app_snapshot') return { ...wireSnapshot(), preferences }
      if (command === 'assistant_voice_status') return speechStatus(preferences, true)
      if (command === 'plugin:capture|snapshot') return { available: true, notesFolderUri: null, captures: [], error: null }
      if (command === 'set_preferences') {
        const proposal = (args as { preferences: Record<string, unknown> }).preferences
        proposals.push(proposal)
        return new Promise((resolve, reject) => { settle = (accepted, message) => {
          if (accepted) { preferences = proposal; resolve(preferences) }
          else reject({ code: 'invalid_input', message })
        } })
      }
      return null
    } }
    const { default: App } = await import('./App')
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="Voice notes"]')!.click() })
    const notes = container.querySelector('.capture-history')!
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="Settings"]')!.click() })
    const dialog = container.querySelector('[role="dialog"]')!
    const close = dialog.querySelector<HTMLButtonElement>('[aria-label="Close settings"]')!
    const historyEntry = window.history.state
    const change = async (): Promise<void> => {
      await act(async () => {
        if (value !== undefined) { const select = dialog.querySelector<HTMLSelectElement>(`[aria-label="${label}"]`)!; select.value = value; select.dispatchEvent(new Event('change', { bubbles: true })) }
        else dialog.querySelector<HTMLButtonElement | HTMLInputElement>(`[aria-label="${label}"]`)!.click()
      })
    }
    const assertRolledBack = (): void => {
      if (value !== undefined) expect(dialog.querySelector<HTMLSelectElement>(`[aria-label="${label}"]`)!.value).toBe(original)
      else if (typeof original === 'boolean') expect(dialog.querySelector<HTMLInputElement>(`[aria-label="${label}"]`)!.checked).toBe(original)
      else for (const [name, expected] of [['Speech language', 'en-AU'], ['TTS engine', 'synthetic.tts.a'], ['TTS voice', 'synthetic-a-local']]) expect(dialog.querySelector<HTMLSelectElement>(`[aria-label="${name}"]`)!.value).toBe(expected)
      expect(preferences).toEqual(baseline)
      expect(container!.querySelector('.capture-history')).toBe(notes)
      expect(container!.querySelector('[role="dialog"]')).toBe(dialog)
      expect(window.history.state).toEqual(historyEntry)
      expect(document.body.style.overflowY).toBe('hidden')
      expect(document.activeElement).toBe(close)
    }
    const assertWriteError = (message: string): void => {
      const alert = dialog.querySelector('[role="alert"]')
      expect(alert).not.toBeNull()
      expect(alert!.textContent).toContain(message)
    }
    await change()
    expect(proposals[0]?.[field]).toBe(changed)
    await act(async () => { settle(false, `Synthetic ${kind} rejection one`) })
    assertRolledBack()
    assertWriteError(`Synthetic ${kind} rejection one`)
    await act(async () => { await vi.advanceTimersByTimeAsync(5000) })
    assertWriteError(`Synthetic ${kind} rejection one`)
    await change()
    expect(dialog.querySelector('[role="alert"]')).toBeNull()
    await act(async () => { settle(false, `Synthetic ${kind} rejection two`) })
    assertRolledBack()
    assertWriteError(`Synthetic ${kind} rejection two`)
    expect(dialog.textContent).not.toContain(`Synthetic ${kind} rejection one`)
    await change()
    expect(dialog.querySelector('[role="alert"]')).toBeNull()
    await act(async () => { settle(true) })
    expect(proposals).toHaveLength(3)
    expect(preferences[field]).toBe(changed)
    expect(dialog.querySelector('[role="alert"]')).toBeNull()
    expect(container.querySelector('.capture-history')).toBe(notes)
    expect(window.history.state).toEqual(historyEntry)
    const { invokeTauriCommand } = await import('./lib/tauri')
    expect(await invokeTauriCommand('get_assistant_settings')).toMatchObject({ speech_language: preferences['speech_language'], speech_tts_provider: preferences['speech_tts_provider'], speech_tts_voice: preferences['speech_tts_voice'], web_enabled: preferences['web_enabled'], speech_network_fallback: preferences['speech_network_fallback'] })
    await act(async () => { close.click(); await vi.advanceTimersByTimeAsync(0) })
    expect(container.querySelector('.capture-history')).toBe(notes)
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="Chat"]')!.click() })
    expect(container.textContent).not.toContain(`Synthetic ${kind} rejection two`)
    window.history.replaceState(null, '')
  })
  it('does not overlap slow Voice notes status reads or accept their retired result after chat owns newer status', async () => {
    vi.useFakeTimers(); vi.stubEnv('VITE_VOXGOLEM_RUNTIME', 'android'); vi.resetModules(); window.history.replaceState(null, '')
    vi.spyOn(window.history, 'back').mockImplementation(() => { window.history.replaceState(null, ''); window.dispatchEvent(new PopStateEvent('popstate', { state: null })) })
    let preferences: Record<string, unknown> = { ...(wireSnapshot() as { preferences: Record<string, unknown> }).preferences, speech_language: 'en-AU', speech_tts_provider: 'synthetic.tts.a', speech_tts_voice: '' }
    const retiredStatus = speechStatus(preferences)
    let calls = 0
    let activeReads = 0
    let peakReads = 0
    let retire: (value: unknown) => void = () => undefined
    window.__TAURI_INTERNALS__ = { invoke: async (command, args) => {
      if (command === 'get_app_snapshot') return { ...wireSnapshot(), preferences }
      if (command === 'assistant_voice_status') {
        peakReads = Math.max(peakReads, ++activeReads)
        if (++calls === 2) return new Promise((resolve) => { retire = (value) => { activeReads -= 1; resolve(value) } })
        activeReads -= 1
        return speechStatus(preferences, true)
      }
      if (command === 'set_preferences') { preferences = (args as { preferences: Record<string, unknown> }).preferences; return preferences }
      if (command === 'plugin:capture|snapshot') return { available: true, notesFolderUri: null, captures: [], error: null }
      return null
    } }
    const { default: App } = await import('./App')
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="Voice notes"]')!.click() })
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="Settings"]')!.click() })
    await act(async () => { await vi.advanceTimersByTimeAsync(2000) })
    expect(calls).toBe(2)
    await act(async () => { const engine = container!.querySelector<HTMLSelectElement>('[aria-label="TTS engine"]')!; engine.value = 'synthetic.tts.b'; engine.dispatchEvent(new Event('change', { bubbles: true })) })
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="Close settings"]')!.click(); await vi.advanceTimersByTimeAsync(0) })
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="Chat"]')!.click() })
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="Settings"]')!.click() })
    // The same adapter retains the in-flight read across the view handoff.
    expect(calls).toBe(2)
    await act(async () => { retire(retiredStatus) })
    expect(calls).toBe(3)
    expect(peakReads).toBe(1)
    expect(container.querySelector('[aria-label="Last native-reported requested configuration"]')?.textContent).toContain('synthetic.tts.b')
    expect(container.querySelector<HTMLSelectElement>('[aria-label="TTS voice"]')!.querySelector('option[value="synthetic-b-local"]')).not.toBeNull()
    window.history.replaceState(null, '')
  })
  it('refreshes speech inventory in Voice notes Settings with one exclusive owner and stops that polling on close', async () => {
    vi.useFakeTimers(); vi.stubEnv('VITE_VOXGOLEM_RUNTIME', 'android'); vi.resetModules(); window.history.replaceState(null, '')
    vi.spyOn(window.history, 'back').mockImplementation(() => { window.history.replaceState(null, ''); window.dispatchEvent(new PopStateEvent('popstate', { state: null })) })
    let preferences: Record<string, unknown> = { ...(wireSnapshot() as { preferences: Record<string, unknown> }).preferences, speech_language: 'en-AU', speech_tts_provider: 'synthetic.tts.a', speech_tts_voice: 'synthetic-a-local' }
    let preparedB = false
    let statusCalls = 0
    window.__TAURI_INTERNALS__ = { invoke: async (command, args) => {
      if (command === 'get_app_snapshot') return { ...wireSnapshot(), preferences }
      if (command === 'assistant_voice_status') { statusCalls += 1; return speechStatus(preferences, preparedB) }
      if (command === 'set_preferences') { preferences = (args as { preferences: Record<string, unknown> }).preferences; return preferences }
      if (command === 'plugin:capture|snapshot') return { available: true, notesFolderUri: null, captures: [], error: null }
      return null
    } }
    const { default: App } = await import('./App')
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    const chatCalls = statusCalls
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="Voice notes"]')!.click() })
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="Settings"]')!.click() })
    expect(statusCalls).toBe(chatCalls + 1)
    await act(async () => { const select = container!.querySelector<HTMLSelectElement>('[aria-label="TTS engine"]')!; select.value = 'synthetic.tts.b'; select.dispatchEvent(new Event('change', { bubbles: true })) })
    preparedB = true
    await act(async () => { await vi.advanceTimersByTimeAsync(1000) })
    expect(statusCalls).toBe(chatCalls + 2)
    expect(container.querySelector<HTMLSelectElement>('[aria-label="TTS voice"]')!.querySelector('option[value="synthetic-b-local"]')).not.toBeNull()
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="Close settings"]')!.click(); await vi.advanceTimersByTimeAsync(0) })
    const closedCalls = statusCalls
    await act(async () => { await vi.advanceTimersByTimeAsync(3000) })
    expect(statusCalls).toBe(closedCalls)
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="Chat"]')!.click() })
    expect(statusCalls).toBe(closedCalls + 1)
    await act(async () => { await vi.advanceTimersByTimeAsync(1000) })
    expect(statusCalls).toBe(closedCalls + 2)
    window.history.replaceState(null, '')
  })
  it('persists requested speech selections, explicitly clears voice on engine change, and waits for the matching reported inventory', async () => {
    vi.useFakeTimers(); vi.stubEnv('VITE_VOXGOLEM_RUNTIME', 'android'); vi.resetModules(); window.history.replaceState(null, '')
    let preferences: Record<string, unknown> = { ...(wireSnapshot() as { preferences: Record<string, unknown> }).preferences, speech_language: 'en-AU', speech_tts_provider: 'synthetic.tts.a', speech_tts_voice: 'synthetic-a-local' }
    let preparedB = false
    const proposals: Record<string, unknown>[] = []
    const invoke = vi.fn(async (command: string, args?: unknown) => {
      if (command === 'get_app_snapshot') return { ...wireSnapshot(), preferences }
      if (command === 'assistant_voice_status') return speechStatus(preferences, preparedB)
      if (command === 'set_preferences') { preferences = (args as { preferences: Record<string, unknown> }).preferences; proposals.push(preferences); return preferences }
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke }
    const { default: App } = await import('./App')
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="Settings"]')!.click() })
    const choose = async (label: string, value: string): Promise<void> => {
      await act(async () => { const select = container!.querySelector<HTMLSelectElement>(`[aria-label="${label}"]`)!; select.value = value; select.dispatchEvent(new Event('change', { bubbles: true })) })
    }
    expect(container.querySelector<HTMLSelectElement>('[aria-label="TTS voice"]')!.value).toBe('synthetic-a-local')
    expect(container.textContent).toContain('Changing the TTS engine resets the requested voice to Automatic')
    await choose('TTS engine', 'synthetic.tts.b')
    expect(proposals[0]).toMatchObject({ speech_language: 'en-AU', speech_tts_provider: 'synthetic.tts.b', speech_tts_voice: '', ui_theme: 'dark' })
    expect(container.querySelector<HTMLSelectElement>('[aria-label="TTS voice"]')!.value).toBe('')
    expect(Array.from(container.querySelector<HTMLSelectElement>('[aria-label="TTS voice"]')!.options).map((option) => option.value)).toEqual([''])
    expect(container.querySelector('[aria-label="Last native-reported requested configuration"]')?.textContent).toContain('synthetic-a-local')
    preparedB = true
    await act(async () => { await vi.advanceTimersByTimeAsync(1000) })
    expect(Array.from(container.querySelector<HTMLSelectElement>('[aria-label="TTS voice"]')!.options).map((option) => option.value)).toEqual(['', 'synthetic-b-local'])
    await choose('Speech language', 'en-GB')
    await choose('TTS voice', 'synthetic-b-local')
    expect(preferences).toMatchObject({ speech_language: 'en-GB', speech_tts_provider: 'synthetic.tts.b', speech_tts_voice: 'synthetic-b-local' })
    const { invokeTauriCommand } = await import('./lib/tauri')
    expect(await invokeTauriCommand('get_assistant_settings')).toMatchObject({ speech_language: 'en-GB', speech_tts_provider: 'synthetic.tts.b', speech_tts_voice: 'synthetic-b-local' })
    expect(invoke.mock.calls.filter(([command]) => command === 'set_preferences')).toHaveLength(3)
    expect(invoke.mock.calls.some(([command]) => command === 'set_assistant_settings')).toBe(false)
    await act(async () => { root?.unmount() }); container.remove()
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    expect(container.querySelector<HTMLSelectElement>('[aria-label="Speech language"]')!.value).toBe('en-GB')
    expect(container.querySelector<HTMLSelectElement>('[aria-label="TTS engine"]')!.value).toBe('synthetic.tts.b')
    expect(container.querySelector<HTMLSelectElement>('[aria-label="TTS voice"]')!.value).toBe('synthetic-b-local')
    window.history.replaceState(null, '')
  })

  it('rolls back rejected engine/voice selection atomically and preserves the cached baseline for a subsequent accepted choice', async () => {
    vi.stubEnv('VITE_VOXGOLEM_RUNTIME', 'android'); vi.resetModules(); window.history.replaceState(null, '')
    let preferences: Record<string, unknown> = { ...(wireSnapshot() as { preferences: Record<string, unknown> }).preferences, speech_language: 'en-AU', speech_tts_provider: 'synthetic.tts.a', speech_tts_voice: 'synthetic-a-local' }
    let reject: (reason: unknown) => void = () => undefined
    let writes = 0
    window.__TAURI_INTERNALS__ = { invoke: async (command, args) => {
      if (command === 'get_app_snapshot') return { ...wireSnapshot(), preferences }
      if (command === 'assistant_voice_status') return speechStatus(preferences)
      if (command === 'set_preferences') { if (++writes === 1) return new Promise((_resolve, fail) => { reject = fail }); preferences = (args as { preferences: Record<string, unknown> }).preferences; return preferences }
      return null
    } }
    const { default: App } = await import('./App')
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="Settings"]')!.click() })
    const engine = container.querySelector<HTMLSelectElement>('[aria-label="TTS engine"]')!
    const voice = container.querySelector<HTMLSelectElement>('[aria-label="TTS voice"]')!
    await act(async () => { engine.value = 'synthetic.tts.b'; engine.dispatchEvent(new Event('change', { bubbles: true })) })
    expect(engine.value).toBe('synthetic.tts.b'); expect(voice.value).toBe('')
    expect(engine.disabled).toBe(true); expect(voice.disabled).toBe(true)
    await act(async () => { reject({ code: 'invalid_input', message: 'Synthetic requested engine unavailable' }) })
    expect(engine.value).toBe('synthetic.tts.a'); expect(voice.value).toBe('synthetic-a-local')
    expect(engine.disabled).toBe(false)
    expect(container.textContent).toContain('Synthetic requested engine unavailable')
    await act(async () => { voice.value = 'synthetic-a-other'; voice.dispatchEvent(new Event('change', { bubbles: true })) })
    expect(preferences).toMatchObject({ speech_language: 'en-AU', speech_tts_provider: 'synthetic.tts.a', speech_tts_voice: 'synthetic-a-other' })
    const { invokeTauriCommand } = await import('./lib/tauri')
    expect(await invokeTauriCommand('get_assistant_settings')).toMatchObject({ speech_tts_provider: 'synthetic.tts.a', speech_tts_voice: 'synthetic-a-other' })
    window.history.replaceState(null, '')
  })

  it('can re-enable persisted fallback through the native adapter after local-only support disappears', async () => {
    vi.stubEnv('VITE_VOXGOLEM_RUNTIME', 'android'); vi.resetModules(); window.history.replaceState(null, '')
    let preferences: Record<string, unknown> = { ...(wireSnapshot() as { preferences: Record<string, unknown> }).preferences, speech_network_fallback: false }
    window.__TAURI_INTERNALS__ = { invoke: async (command, args) => {
      if (command === 'get_app_snapshot') return { ...wireSnapshot(), preferences }
      if (command === 'assistant_voice_status') return speechStatus(preferences, false, false)
      if (command === 'set_preferences') { preferences = (args as { preferences: Record<string, unknown> }).preferences; return preferences }
      return null
    } }
    const { default: App } = await import('./App')
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="Settings"]')!.click() })
    const fallback = container.querySelector<HTMLInputElement>('[aria-label="Allow speech network fallback"]')!
    expect(fallback.checked).toBe(false); expect(fallback.disabled).toBe(false)
    await act(async () => { fallback.click() })
    expect(preferences['speech_network_fallback']).toBe(true)
    expect(container.querySelector('[aria-label="Allow speech network fallback"]')).toBeNull()
    expect(container.textContent).toContain('Disabling speech network fallback is unavailable')
    window.history.replaceState(null, '')
  })
  it('persists web and supported speech fallback policies through the real native preference adapter', async () => {
    vi.stubEnv('VITE_VOXGOLEM_RUNTIME', 'android')
    vi.resetModules()
    window.history.replaceState(null, '')
    let preferences = (wireSnapshot() as { preferences: Record<string, unknown> }).preferences
    const invoke = vi.fn(async (command: string, args?: unknown) => {
      if (command === 'get_app_snapshot') return { ...wireSnapshot(), preferences }
      if (command === 'assistant_voice_status') return { kind: 'voice_status', available: true, reason: 'Synthetic speech provider', mode: 'dictation', dictation: 'idle', loop: 'idle', ttsEnabled: false, ttsAvailable: true, ttsPhase: 'idle', error: null, localOnlyRecognitionAvailable: true }
      if (command === 'set_preferences') { preferences = (args as { preferences: Record<string, unknown> }).preferences; return preferences }
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke }
    const { default: App } = await import('./App')
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="Settings"]')!.click() })
    const web = container.querySelector<HTMLInputElement>('[aria-label="Allow web access"]')!
    const speech = container.querySelector<HTMLInputElement>('[aria-label="Allow speech network fallback"]')!
    expect(web.checked).toBe(true)
    expect(speech.checked).toBe(true)
    await act(async () => { web.click() })
    await act(async () => { speech.click() })
    expect(preferences).toMatchObject({ web_enabled: false, speech_network_fallback: false, ui_theme: 'dark', instant: 'custom-sol-high' })
    expect(invoke.mock.calls.filter(([command]) => command === 'set_preferences')).toHaveLength(2)
    expect(invoke.mock.calls.some(([command]) => command === 'set_assistant_settings')).toBe(false)
    expect(container.querySelector<HTMLSelectElement>('[aria-label="Search mode"]')!.disabled).toBe(true)
    await act(async () => { root?.unmount() }); container.remove()
    container = document.createElement('div'); document.body.append(container); root = createRoot(container)
    await act(async () => { root?.render(<App />) })
    expect(container.querySelector<HTMLInputElement>('[aria-label="Allow web access"]')!.checked).toBe(false)
    expect(container.querySelector<HTMLInputElement>('[aria-label="Allow speech network fallback"]')!.checked).toBe(false)
    window.history.replaceState(null, '')
  })
  it('recovers hydration through the real cache-only adapter under StrictMode', async () => {
    vi.stubEnv('VITE_VOXGOLEM_RUNTIME', 'android')
    vi.resetModules()
    let snapshotCalls = 0
    let startupCalls = 0
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          if (snapshotCalls === 1) throw new Error('transient initial snapshot failure')
          return wireSnapshot()
        }
        if (command === 'get_startup_state') {
          startupCalls += 1
          throw new Error('Android startup must use the accepted cache')
        }
        return null
      },
    }
    const { default: App } = await import('./App')

    container = document.createElement('div')
    document.body.append(container)
    root = createRoot(container)
    await act(async () => {
      root?.render(<StrictMode><App /></StrictMode>)
      await Promise.resolve()
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(snapshotCalls).toBeGreaterThanOrEqual(2)
    expect(startupCalls).toBe(0)
    const composer = container.querySelector<HTMLTextAreaElement>('#promptComposer')
    if (composer === null) throw new Error('Missing composer')
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set
      setter?.call(composer, 'Ready after recovery')
      composer.dispatchEvent(new Event('input', { bubbles: true }))
    })
    expect(container.querySelector<HTMLButtonElement>('button[type="submit"]')?.disabled).toBe(false)
  })

  it('adopts a current snapshot after reset exhausts while initial hydration is stale', async () => {
    vi.useFakeTimers()
    vi.stubEnv('VITE_VOXGOLEM_RUNTIME', 'android')
    vi.resetModules()
    let snapshotCalls = 0
    let startupCalls = 0
    const initialResolvers: Array<(value: unknown) => void> = []
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_app_snapshot') {
          snapshotCalls += 1
          if (snapshotCalls <= 2) {
            return new Promise((resolve) => { initialResolvers.push(resolve) })
          }
          if (snapshotCalls <= 5) throw new Error('reset snapshot unavailable')
          return wireSnapshot()
        }
        if (command === 'reset_session') return wireSnapshot()
        if (command === 'get_startup_state') {
          startupCalls += 1
          throw new Error('Android startup must use the accepted cache')
        }
        return null
      },
    }
    const { default: App } = await import('./App')

    container = document.createElement('div')
    document.body.append(container)
    root = createRoot(container)
    await act(async () => {
      root?.render(<StrictMode><App /></StrictMode>)
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(snapshotCalls).toBe(2)
    await act(async () => {
      container?.querySelector<HTMLButtonElement>('[aria-label="New Chat"]')?.click()
      await Promise.resolve()
      initialResolvers[1]?.(wireSnapshot())
      await Promise.resolve()
      await vi.advanceTimersByTimeAsync(2_000)
    })

    expect(snapshotCalls).toBeGreaterThanOrEqual(6)
    expect(startupCalls).toBe(0)
    const composer = container.querySelector<HTMLTextAreaElement>('#promptComposer')
    if (composer === null) throw new Error('Missing composer')
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set
      setter?.call(composer, 'Ready after reset recovery')
      composer.dispatchEvent(new Event('input', { bubbles: true }))
    })
    expect(container.querySelector<HTMLButtonElement>('button[type="submit"]')?.disabled).toBe(false)
  })
})

function speechStatus(preferences: Record<string, unknown>, preparedB = false, localOnlyRecognitionAvailable = true) {
  return { kind: 'voice_status', available: true, reason: 'Synthetic speech provider', mode: 'dictation', dictation: 'idle', loop: 'idle', ttsEnabled: false, ttsAvailable: true, ttsPhase: 'idle', error: null,
    speechLanguage: preferences['speech_language'] ?? '', speechTtsProvider: preferences['speech_tts_provider'] ?? '', speechTtsVoice: preferences['speech_tts_voice'] ?? '', localOnlyRecognitionAvailable,
    speechOptions: { ttsProviders: ['synthetic.tts.a', 'synthetic.tts.b'], ttsVoices: [
      { provider: 'synthetic.tts.a', name: 'synthetic-a-local', language: 'en-AU', networkRequired: false },
      { provider: 'synthetic.tts.a', name: 'synthetic-a-other', language: 'en-AU', networkRequired: false },
      ...(preparedB ? [{ provider: 'synthetic.tts.b', name: 'synthetic-b-local', language: 'en-GB', networkRequired: false }] : []),
    ], recognitionInstalledLanguages: ['en-AU'], recognitionSupportedLanguages: ['en-AU', 'en-GB'], localOnlyRecognitionAvailable, modelSelectionAvailable: false, modelIdentifiers: null } }
}

function desktopStartupState() {
  return { kind: 'ready', cue_asset_paths: { start_listening: 'synthetic-start.wav', stop_listening: 'synthetic-stop.wav' }, runtime_phase: 'sleeping', voice_input_available: false, voice_input_error: 'Synthetic voice unavailable', silence_timeout_ms: 1500, selected_response_profile: 'fast', supported_response_profiles: [], prompt_cancellation_available: true, tts_enabled: false, tts_output_gain_db: 3, capabilities: wireSnapshot()['capabilities'] }
}

function wireSnapshot(): Record<string, unknown> {
  const capabilityIds = ['custom_provider', 'opencode', 'local_fast', 'local_quality', 'qwen_prediction', 'wake_word', 'vad', 'parakeet', 'tts', 'deep', 'review']
  return {
    session_id: '1',
    runtime_phase: 'sleeping',
    transcript: [],
    active_request: null,
    capabilities: capabilityIds.map((id) => ({
      id,
      state: id === 'custom_provider' ? 'available' : 'not_configured',
      reason: id === 'custom_provider' ? 'Ready' : 'Unavailable on Android',
      actual_provider: null,
    })),
    model_options: [
      { id: 'custom-sol-high', label: 'Sol High', available: true, unavailable_reason: null },
      { id: 'custom-luna-low', label: 'Luna Low', available: true, unavailable_reason: null },
    ],
    preferences: {
      instant: 'custom-sol-high', deep: 'custom-sol-high', review: 'custom-sol-high',
      deep_enabled: false, review_enabled: false, prefetch: false, completion: false,
      response_profile: 'fast', tts_enabled: false, ui_theme: 'dark', ui_text_size: 'medium',
    },
  }
}

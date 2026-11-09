import { execFileSync } from 'node:child_process'
import { resolve } from 'node:path'
import { act } from 'react'
import { createRoot } from 'react-dom/client'
import type { Root } from 'react-dom/client'
import { afterEach, describe, expect, it, vi } from 'vitest'
import App from './App'
import { FrontendSession } from './FrontendSession'
import { sessionAuthority } from './SessionAuthority'
import { appSnapshot, canonicalTurn, completeCapabilities, readyStartupState, durableId } from './test/sessionFixtures'

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

async function mount(initial: Availability = 'available', session = new FrontendSession(), durable = false) {
  vi.useFakeTimers()
  let availability = initial
  let sessionId = durable ? durableId(1) : '1'
  let transcript: Record<string, unknown>[] = []
  let snapshotResponse: (() => Promise<unknown>) | null = null
  let startupResponse: (() => Promise<unknown>) | null = null
  const unexpected: string[] = []
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
    if (command === 'get_app_snapshot') return snapshotResponse === null ? snapshot() : snapshotResponse()
    if (command === 'get_startup_state') return startupResponse === null ? startup() : startupResponse()
    if (command === 'submit_prompt') {
      const request = args as { requestId: string; prompt: string }
      const turn = canonicalTurn('1', '2', request.prompt, 'Synthetic admitted answer', request.requestId, 'text', 'instant', sessionId)
      transcript = [turn]
      return { kind: 'completed', save_disposition: 'saved', turn }
    }
    if (command === 'reset_session') { sessionId = '3'; transcript = []; return snapshot() }
    if (command === 'get_update_snapshot') return { revision: 1, phase: 'up_to_date', operation: null, current_version: 'synthetic-version', version: null, notes: null, progress_phase: null, downloaded_bytes: 0, total_bytes: null, error: null, reason: null, auto_download_enabled: false, auto_install_enabled: false }
    if (['record_frontend_runtime_diagnostic', 'check_for_update', 'clear_completion'].includes(command)) return null
    unexpected.push(command)
    throw new Error(`Unexpected desktop controls fixture command: ${command}`)
  })
  window.__TAURI_INTERNALS__ = { invoke, createChannel: <T,>(send: (value: T) => void) => ({ send }) }
  container = document.createElement('div'); document.body.append(container); root = createRoot(container)
  await act(async () => { root!.render(<App session={session} />) })
  return {
    session, invoke, snapshot, startup, unexpected,
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

function benchmarkScripts() {
  return JSON.parse(execFileSync('python3', ['-c',
    'import json,runpy,sys; s=runpy.run_path(sys.argv[1]); print(json.dumps({k:s[k] for k in ("BEGIN_OWNERSHIP_JS","SETTINGS_NAVIGATION_JS","RELEASE_OWNERSHIP_JS")}))',
    resolve('../../tools/benchmark/benchmark.py'),
  ], { encoding: 'utf8', timeout: 5000 })) as {
    BEGIN_OWNERSHIP_JS: string; SETTINGS_NAVIGATION_JS: string; RELEASE_OWNERSHIP_JS: string
  }
}

// Execute benchmark JavaScript against real App/ActionMenu/settings DOM.
// Geometry, history traversal and native calls are synthetic; no app is launched.
it('supports shared benchmark settings navigation and refuses foreign UI mutations', async () => {
  const state = await mount()
  // happy-dom does not traverse history; deliver the browser's popstate boundary.
  const historyStates: unknown[] = []
  const pushState = window.history.pushState.bind(window.history)
  vi.spyOn(window.history, 'pushState').mockImplementation((data, unused, url) => {
    historyStates.push(window.history.state)
    pushState(data, unused, url)
  })
  vi.spyOn(window.history, 'back').mockImplementation(() => {
    const state = historyStates.pop() ?? null
    window.history.replaceState(state, '')
    window.dispatchEvent(new PopStateEvent('popstate', { state }))
  })
  const originalInvoke = state.invoke.getMockImplementation()!
  state.invoke.mockImplementation(async (command, args) => {
    if (command === 'list_audio_input_devices') return []
    if (command === 'set_assistant_settings') return (args as { settings: unknown }).settings
    if (command === 'set_tts_enabled') return { enabled: (args as { enabled: boolean }).enabled }
    return originalInvoke(command, args)
  })
  vi.spyOn(HTMLElement.prototype, 'getClientRects').mockReturnValue([{}] as unknown as DOMRectList)
  const scripts = benchmarkScripts()
  const script = scripts.SETTINGS_NAVIGATION_JS
  const releaseScript = scripts.RELEASE_OWNERSHIP_JS
  const release = () => window.eval(releaseScript.replace('ARGS', JSON.stringify('synthetic-owner-token'))) as boolean
  const owner = { token: 'synthetic-owner-token', session: '1', request: null, execution: null, baseline: '[]' }
  Object.assign(window, { __voxBenchmarkOwner: owner })
  let enabled = true
  const run = async (action: string, page = 'web', label = 'Allow web access') => {
    let result: unknown
    await act(async () => {
      result = await window.eval(script.replace('ARGS', JSON.stringify({ action, token: 'synthetic-token', page, label, enabled })))
      await vi.advanceTimersByTimeAsync(100)
    })
    return result
  }
  const mutate = async (action: string, page = 'web', label = 'Allow web access') => {
    Object.assign(window, { __voxBenchmarkOwner: { ...owner, session: 'synthetic-foreign' } })
    expect(await run(action, page, label), `${action}: foreign owner`).toBe(false)
    Object.assign(window, { __voxBenchmarkOwner: owner })
    return run(action, page, label)
  }
  try {
    const menu = container!.querySelector<HTMLDetailsElement>('details.ui-action-menu')!
    expect(menu.open).toBe(false)
    const composer = container!.querySelector<HTMLTextAreaElement>('#promptComposer')!
    composer.value = 'Synthetic draft'
    expect(await run('begin')).toBe(false)
    expect(menu.open).toBe(false)
    expect(composer.value).toBe('Synthetic draft')
    composer.value = ''
    menu.open = true
    expect(await run('begin')).toBe(false)
    expect(menu.open).toBe(true)
    menu.open = false
    for (const [page, label] of [['web', 'Allow web access'], ['voice', 'Speak replies']]) {
      for (const action of ['begin', 'open', 'mounted', 'page', 'ready']) {
        expect(await (['begin', 'open', 'page', 'back', 'close', 'finish'].includes(action) ? mutate : run)(action, page, label), `${page}: ${action}`).toBe(true)
      }
      const input = container!.querySelector<HTMLInputElement>(`input[aria-label="${label}"]`)!
      enabled = !input.checked
      Object.assign(window, { __voxBenchmarkOwner: { ...owner, session: 'synthetic-foreign' } })
      expect(await run('toggle', page, label)).toBe(false)
      expect(input.checked).toBe(!enabled)
      Object.assign(window, { __voxBenchmarkOwner: owner })
      expect(await run('toggle', page, label)).toBe(true)
      expect(input.checked).toBe(enabled)
      for (const action of ['back', 'overview', 'close', 'closed', 'finish']) {
        expect(await (['begin', 'open', 'page', 'back', 'close', 'finish'].includes(action) ? mutate : run)(action, page, label), `${page}: ${action}`).toBe(true)
      }
      expect(menu.open).toBe(false)
      expect(container!.querySelector('[role="dialog"]')).toBeNull()
    }
    // A mount-observation failure must still close the dialog this operation opened.
    for (const action of ['begin', 'open', 'back', 'overview', 'close', 'closed', 'finish']) {
      expect(await run(action), `mount failure cleanup: ${action}`).toBe(true)
    }
    // A page that never mounts must also unwind from the remaining overview.
    expect(await run('begin')).toBe(true)
    expect(await run('open')).toBe(true)
    expect(await run('mounted')).toBe(true)
    const link = container!.querySelector<HTMLButtonElement>('[aria-label="Web research"]')!
    const blockedClick = vi.spyOn(link, 'click').mockImplementation(() => {})
    expect(await run('page')).toBe(true)
    expect(await run('ready')).toBe(false)
    blockedClick.mockRestore()
    for (const action of ['back', 'overview', 'close', 'closed', 'finish']) {
      expect(await run(action), `page failure cleanup: ${action}`).toBe(true)
    }
    const trigger = menu.querySelector<HTMLElement>('[aria-label="More actions"]')!
    const failedBegin = vi.spyOn(trigger, 'click').mockImplementation(() => { throw new Error('Synthetic begin failure') })
    await expect(run('begin')).rejects.toThrow('Synthetic begin failure')
    failedBegin.mockRestore()
    expect(await run('finalize')).toBe(false)
    expect(Reflect.get(window, '__voxBenchmarkNavigation')).toBeUndefined()
    expect(menu.open).toBe(false)
    expect(await run('begin')).toBe(true)
    const refusedFinish = vi.spyOn(trigger, 'click').mockImplementation(() => {})
    expect(await run('finish')).toBe(false)
    refusedFinish.mockRestore()
    expect(Reflect.get(window, '__voxBenchmarkNavigation')).toBeDefined()
    expect(await run('finalize')).toBe(false)
    expect(Reflect.get(window, '__voxBenchmarkNavigation')).toBeUndefined()
    expect(menu.open).toBe(true)
    menu.open = false // Synthetic user closes their remaining menu.
    // A draft or canonical ownership change forbids UI cleanup, but must not
    // strand this operation's internal token or block a later clean admission.
    for (const interruption of ['draft', 'canonical', 'owner']) {
      expect(await run('begin')).toBe(true)
      expect(await run('open')).toBe(true)
      if (interruption === 'draft') composer.value = 'Synthetic interrupted draft'
      if (interruption === 'canonical') state.setSnapshotResponse(async () => ({ ...state.snapshot(), session_id: 'synthetic-successor' }))
      if (interruption === 'owner') Object.assign(window, { __voxBenchmarkOwner: { ...owner } })
      const dialog = container!.querySelector('[role="dialog"]')
      expect(await run('back')).toBe(false)
      expect(await run('finalize')).toBe(false)
      expect(Reflect.get(window, '__voxBenchmarkNavigation')).toBeUndefined()
      expect(container!.querySelector('[role="dialog"]')).toBe(dialog)
      expect(menu.open).toBe(true)
      if (interruption === 'draft') expect(composer.value).toBe('Synthetic interrupted draft')
      // Only the synthetic user returns their own UI/draft to baseline.
      composer.value = ''
      state.setSnapshotResponse(null)
      Object.assign(window, { __voxBenchmarkOwner: owner })
      await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="Close settings"]')!.click() })
      menu.open = false
      for (const action of ['begin', 'back', 'overview', 'close', 'closed', 'finish', 'finalize']) {
        expect(await run(action), `subsequent admission: ${action}`).toBe(true)
      }
    }
    // Returning preferences/UI to baseline cannot erase an earlier failed cleanup.
    expect(Reflect.get(window, '__voxBenchmarkNavigation')).toBeUndefined()
    expect(release()).toBe(false)
    expect(Reflect.get(window, '__voxBenchmarkOwner')).toBeUndefined()
    Reflect.deleteProperty(owner, 'navigationFailed')
    Object.assign(window, { __voxBenchmarkOwner: owner })
    expect(await run('begin')).toBe(true)
    const foreignNavigation = Reflect.get(window, '__voxBenchmarkNavigation') as { token: string }
    foreignNavigation.token = 'synthetic-foreign-token'
    expect(await run('finalize')).toBe(false)
    expect(Reflect.get(window, '__voxBenchmarkNavigation')).toBe(foreignNavigation)
    foreignNavigation.token = 'synthetic-token'
    expect(await run('finish')).toBe(true)
    expect(await run('finalize')).toBe(true)
    // Release only retires this suite's bookkeeping, even when UI is unsafe.
    expect(await run('begin')).toBe(true)
    expect(await run('open')).toBe(true)
    composer.value = 'Synthetic release draft'
    const ownedDialog = container!.querySelector('[role="dialog"]')
    expect(release()).toBe(false)
    expect(Reflect.get(window, '__voxBenchmarkNavigation')).toBeUndefined()
    expect(Reflect.get(window, '__voxBenchmarkOwner')).toBeUndefined()
    expect(container!.querySelector('[role="dialog"]')).toBe(ownedDialog)
    expect(menu.open).toBe(true)
    expect(composer.value).toBe('Synthetic release draft')
    composer.value = ''
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="Close settings"]')!.click() })
    menu.open = false
    const nextOwner = { ...owner, navigationFailed: false }
    Object.assign(window, { __voxBenchmarkOwner: nextOwner })
    expect(await run('begin')).toBe(true)
    const ownedNavigation = Reflect.get(window, '__voxBenchmarkNavigation')
    const successor = { ...owner, token: 'synthetic-successor-token' }
    Object.assign(window, { __voxBenchmarkOwner: successor, __voxBenchmarkExecutions: ['synthetic-successor'] })
    expect(release()).toBe(false)
    expect(Reflect.get(window, '__voxBenchmarkNavigation')).toBeUndefined()
    expect(Reflect.get(window, '__voxBenchmarkOwner')).toBe(successor)
    expect(Reflect.get(window, '__voxBenchmarkExecutions')).toEqual(['synthetic-successor'])
    expect(menu.open).toBe(true)
    menu.open = false
    Object.assign(window, { __voxBenchmarkOwner: nextOwner })
    // A foreign navigation token/owner must survive our release as well.
    const foreignOwnedNavigation = { ...(ownedNavigation as object), owner: successor }
    Object.assign(window, { __voxBenchmarkNavigation: foreignOwnedNavigation })
    expect(release()).toBe(false)
    expect(Reflect.get(window, '__voxBenchmarkNavigation')).toBe(foreignOwnedNavigation)
    Reflect.deleteProperty(window, '__voxBenchmarkNavigation')
    Object.assign(window, { __voxBenchmarkOwner: nextOwner })
    expect(release()).toBe(true)
    Object.assign(window, { __voxBenchmarkOwner: owner })
    const foreignDialog = document.createElement('section')
    foreignDialog.setAttribute('role', 'dialog')
    container!.append(foreignDialog)
    expect(await run('begin')).toBe(false)
    expect(foreignDialog.isConnected).toBe(true)
    expect(menu.open).toBe(false)
    foreignDialog.remove()
    expect(state.invoke.mock.calls.filter(([command]) => command === 'set_assistant_settings')).toHaveLength(1)
    expect(state.invoke.mock.calls.filter(([command]) => command === 'set_tts_enabled')).toHaveLength(1)
    expect(state.unexpected).toEqual([])
  } finally {
    Reflect.deleteProperty(window, '__voxBenchmarkOwner')
    Reflect.deleteProperty(window, '__voxBenchmarkNavigation')
    Reflect.deleteProperty(window, '__voxBenchmarkExecutions')
  }
})

it('revokes timed-out acquisition before delayed native snapshots can install ownership', async () => {
  const state = await mount()
  const scripts = benchmarkScripts()
  const acquire = (token: string) => window.eval(scripts.BEGIN_OWNERSHIP_JS.replace('OWNER_TOKEN', JSON.stringify(token))) as Promise<boolean>
  const release = (token: string) => window.eval(scripts.RELEASE_OWNERSHIP_JS.replace('ARGS', JSON.stringify(token))) as boolean
  const delayed = deferred<unknown>()
  const originalInvoke = state.invoke.getMockImplementation()!
  state.invoke.mockImplementation(async (command, args) => command === 'get_app_snapshot' ? delayed.promise : originalInvoke(command, args))
  try {
    // A CDP timeout abandons its result, not the JavaScript/native promise.
    const pending = acquire('synthetic-expired-owner')
    const released = release('synthetic-expired-owner')
    delayed.resolve(state.snapshot())
    expect(await pending).toBe(false)
    expect(released).toBe(true)
    expect(Reflect.get(window, '__voxBenchmarkOwner')).toBeUndefined()
    expect(Reflect.get(window, '__voxBenchmarkAcquisition')).toBeUndefined()
    state.invoke.mockImplementation(originalInvoke)
    expect(await acquire('synthetic-next-owner')).toBe(true)
    expect(release('synthetic-expired-owner')).toBe(false)
    expect(Reflect.get(window, '__voxBenchmarkOwner')).toMatchObject({ token: 'synthetic-next-owner' })
    expect(release('synthetic-next-owner')).toBe(true)
    expect(Reflect.get(window, '__voxBenchmarkOwner')).toBeUndefined()

    const expiredSnapshot = deferred<unknown>()
    const successorSnapshot = deferred<unknown>()
    let snapshots = 0
    state.invoke.mockImplementation(async (command, args) => command === 'get_app_snapshot'
      ? (++snapshots === 1 ? expiredSnapshot.promise : successorSnapshot.promise)
      : originalInvoke(command, args))
    const expired = acquire('synthetic-expired-overlap')
    const acquired = Reflect.get(window, '__voxBenchmarkAcquisition')
    expect(await acquire('synthetic-competing-owner')).toBe(false)
    expect(release('synthetic-foreign-release')).toBe(false)
    expect(Reflect.get(window, '__voxBenchmarkAcquisition')).toBe(acquired)
    expect(release('synthetic-expired-overlap')).toBe(true)
    const successor = acquire('synthetic-successor-overlap')
    const successorIntent = Reflect.get(window, '__voxBenchmarkAcquisition')
    expiredSnapshot.resolve(state.snapshot())
    expect(await expired).toBe(false)
    expect(Reflect.get(window, '__voxBenchmarkAcquisition')).toBe(successorIntent)
    expect(release('synthetic-expired-overlap')).toBe(false)
    successorSnapshot.resolve(state.snapshot())
    expect(await successor).toBe(true)
    expect(release('synthetic-successor-overlap')).toBe(true)

    state.invoke.mockRejectedValueOnce(new Error('Synthetic native snapshot failure'))
    await expect(acquire('synthetic-rejected-owner')).rejects.toThrow('Synthetic native snapshot failure')
    expect(Reflect.get(window, '__voxBenchmarkAcquisition')).toBeUndefined()
    expect(release('synthetic-rejected-owner')).toBe(true)
  } finally {
    Reflect.deleteProperty(window, '__voxBenchmarkOwner')
    Reflect.deleteProperty(window, '__voxBenchmarkAcquisition')
  }
})

describe('desktop change-triggered canonical model controls', () => {
  it('applies held control updates without allowing their pre-settlement null disposition to erase an unsaved answer', async () => {
    const session = new FrontendSession()
    const state = await mount('available', session, true)
    const authority = sessionAuthority(session)
    const pending = deferred<unknown>()
    const held = deferred<unknown>()
    const original = state.invoke.getMockImplementation()!
    let requestId = ''
    let discarded = false
    state.invoke.mockImplementation(async (command, args) => {
      if (command === 'submit_prompt') { requestId = (args as { requestId: string }).requestId; return pending.promise }
      if (command === 'discard_unsaved_and_new_chat') {
        if (JSON.stringify(args) !== JSON.stringify({ executionId: { session_id: durableId(1), generation: durableId(20) } })) throw new Error('Synthetic stale discard target')
        discarded = true; return null
      }
      if (command === 'get_app_snapshot' && discarded) return appSnapshot([], {}, { session_id: '2' })
      return original(command, args)
    })
    await typeQuestion()
    await act(async () => { sendButton().click() })
    const before = state.snapshots()
    const controls = { ...state.snapshot(), capabilities: completeCapabilities({ deep: 'failed' }), preferences: { ...(state.snapshot()['preferences'] as Record<string, unknown>), completion: true, ui_theme: 'light' } }
    state.setSnapshotResponse(() => held.promise)
    state.setStartupResponse(async () => ({ ...state.startup(), capabilities: completeCapabilities({ deep: 'failed' }) }))
    await poll()
    expect(state.snapshots()).toBeGreaterThan(before)
    const unsaved = { request_id: requestId, execution_id: { session_id: durableId(1), generation: durableId(20) }, prompt: 'Synthetic controls question', answer: { kind: 'text', text: 'Synthetic retained unsaved answer' }, sources: [], final_stage: 'instant', reason: 'storage_error' }
    await act(async () => { pending.resolve({ kind: 'unsaved', save_disposition: 'unsaved', unsaved_result: unsaved }) })
    expect(container!.querySelector('[aria-label="Unsaved answer"]')).not.toBeNull()
    await act(async () => { held.resolve(controls) })
    expect(container!.querySelector('[aria-label="Unsaved answer"]')?.textContent).toContain('Synthetic retained unsaved answer')
    expect(authority.fieldRef('uiTheme').current).toBe('light')
    expect(authority.fieldRef('assistantSettings').current.completion).toBe(true)
    expect(authority.fieldRef('startupState').current).toMatchObject({ capabilities: expect.arrayContaining([expect.objectContaining({ id: 'deep', state: 'failed' })]) })
    const writeText = vi.fn().mockResolvedValue(undefined)
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } })
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="Copy unsaved response"]')!.click() })
    expect(writeText).toHaveBeenCalledWith('Synthetic retained unsaved answer')
    await typeQuestion()
    expect(sendButton().disabled).toBe(true)
    expect(state.invoke.mock.calls.some(([command]) => command === 'request_completion')).toBe(false)
    await act(async () => { container!.querySelector<HTMLButtonElement>('[aria-label="New Chat"]')!.click() })
    expect(container!.querySelector('[aria-label="Unsaved answer"]')).toBeNull()
    expect(state.invoke.mock.calls.filter(([command]) => command === 'discard_unsaved_and_new_chat')).toEqual([['discard_unsaved_and_new_chat', { executionId: { session_id: durableId(1), generation: durableId(20) } }]])
  })
  it('does not suppress autonomous native audio on hide or renderer detach, and admits no typed-control cues', async () => {
    const state = await mount()
    expect(state.invoke.mock.calls.filter(([command]) => command === 'set_native_audio_active')).toEqual([])
    try {
      Object.defineProperty(document, 'hidden', { configurable: true, value: true })
      await act(async () => { document.dispatchEvent(new Event('visibilitychange')) })
      Object.defineProperty(document, 'hidden', { configurable: true, value: false })
      await act(async () => { document.dispatchEvent(new Event('visibilitychange')) })
      expect(state.invoke.mock.calls.filter(([command]) => command === 'set_native_audio_active')).toEqual([])
      await act(async () => { root!.unmount(); root = null })
      expect(state.invoke.mock.calls.filter(([command]) => command === 'set_native_audio_active')).toEqual([])
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
    expect(container!.querySelector('.composer__utility-actions [aria-label="New Chat"] [data-icon-kind="plus"]')).not.toBeNull()
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

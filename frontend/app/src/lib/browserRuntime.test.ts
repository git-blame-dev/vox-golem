import { afterEach, describe, expect, it, vi } from 'vitest'
import { parseAppSnapshot, parseValidatedPreferences } from './appSnapshot'
import {
  configureBrowserPreviewFixture,
  getBrowserPreviewInternals,
  resetBrowserPreviewRuntime,
  teardownBrowserPreviewRuntime,
} from './browserRuntime'

afterEach(() => {
  resetBrowserPreviewRuntime()
  vi.useRealTimers()
})

describe('browser preview runtime', () => {
  it('starts with a canonical, explicitly labelled preview snapshot', async () => {
    const runtime = getBrowserPreviewInternals()

    const snapshot = parseAppSnapshot(await runtime.invoke('get_app_snapshot'))

    expect(snapshot).toMatchObject({
      sessionId: '1',
      runtimePhase: 'sleeping',
      transcript: [],
      activeRequest: null,
      preferences: { instant: 'local-fast', completion: false },
    })
    expect(snapshot.capabilities.find(({ id }) => id === 'local_fast')).toEqual({
      id: 'local_fast',
      state: 'available',
      reason: 'Deterministic browser preview',
      actualProvider: 'browser_preview',
    })
  })

  it('streams delayed updates and commits one canonical turn', async () => {
    vi.useFakeTimers()
    configureBrowserPreviewFixture({
      delayMs: 25,
      outcome: 'completed',
      chunks: ['Browser preview: ', 'deterministic response.'],
    })
    const runtime = getBrowserPreviewInternals()
    const updates: unknown[] = []
    const channel = runtime.createChannel!((update) => updates.push(update))

    const result = runtime.invoke('submit_prompt', {
      requestId: 'request-1',
      prompt: 'Draft release notes',
      source: 'typed',
      updates: channel,
    })
    expect(parseAppSnapshot(await runtime.invoke('get_app_snapshot'))).toMatchObject({
      runtimePhase: 'executing',
      activeRequest: { requestId: 'request-1', prompt: 'Draft release notes' },
    })

    await vi.advanceTimersByTimeAsync(24)
    expect(updates).toEqual([])
    await vi.advanceTimersByTimeAsync(1)
    expect(updates).toEqual([{ kind: 'delta', text: 'Browser preview: ' }])
    await vi.advanceTimersByTimeAsync(25)
    expect(updates).toHaveLength(2)
    await vi.advanceTimersByTimeAsync(25)

    await expect(result).resolves.toMatchObject({
      kind: 'completed',
      turn: {
        request_id: 'request-1',
        execution_id: { session_id: '1', generation: '1' },
      },
    })
    const snapshot = parseAppSnapshot(await runtime.invoke('get_app_snapshot'))
    expect(snapshot.activeRequest).toBeNull()
    expect(snapshot.transcript).toHaveLength(1)
    expect(snapshot.transcript[0]).toMatchObject({
      requestId: 'request-1',
      executionId: { sessionId: '1', generation: '1' },
      user: { content: { kind: 'text', text: 'Draft release notes' } },
      answer: { content: { kind: 'text', text: 'Browser preview: deterministic response.' } },
      finalStage: 'instant',
    })
  })

  it('publishes all stable Instant choices through the production snapshot parser without enabling native-only models', async () => {
    const snapshot = parseAppSnapshot(await getBrowserPreviewInternals().invoke('get_app_snapshot'))
    expect(snapshot.modelOptions.map((model) => model.id)).toEqual([
      'local-fast', 'local-quality', 'custom-sol-high', 'custom-sol-none',
      'custom-luna-none', 'custom-luna-low', 'opencode-sol-high', 'opencode-luna-low',
    ])
    expect(snapshot.modelOptions.filter((model) => model.available).map((model) => model.id)).toEqual(['local-fast'])
    expect(snapshot.modelOptions.find((model) => model.id === 'custom-sol-none')).toEqual({
      id: 'custom-sol-none', label: 'Custom: GPT-6.1 Sol No Reasoning', available: false,
      unavailableReason: 'Unavailable in browser preview',
    })
    expect(snapshot.modelOptions.find((model) => model.id === 'opencode-sol-high')?.label).toBe('OpenCode: GPT-6.1 Sol High')
  })

  it('rejects deterministic failures without committing partial output', async () => {
    vi.useFakeTimers()
    configureBrowserPreviewFixture({
      delayMs: 10,
      outcome: 'error',
      chunks: ['Partial preview'],
      errorMessage: 'Deterministic preview failure',
    })
    const runtime = getBrowserPreviewInternals()
    const updates: unknown[] = []
    const result = runtime.invoke('submit_prompt', {
      requestId: 'request-error',
      prompt: 'Fail this preview',
      updates: runtime.createChannel!((update) => updates.push(update)),
    })
    const rejection = expect(result).rejects.toMatchObject({
      code: 'unavailable',
      message: 'Deterministic preview failure',
    })

    await vi.runAllTimersAsync()
    await rejection
    expect(updates).toEqual([{ kind: 'delta', text: 'Partial preview' }])
    expect(parseAppSnapshot(await runtime.invoke('get_app_snapshot'))).toMatchObject({
      runtimePhase: 'sleeping',
      transcript: [],
      activeRequest: null,
    })
  })

  it('commits deterministic refusals with canonical refusal content', async () => {
    configureBrowserPreviewFixture({
      outcome: 'refused',
      chunks: ['Browser preview refusal'],
    })
    const runtime = getBrowserPreviewInternals()

    await expect(runtime.invoke('submit_prompt', {
      requestId: 'request-refused',
      prompt: 'Refuse this preview',
      updates: runtime.createChannel!(() => undefined),
    })).resolves.toMatchObject({
      kind: 'refused',
      turn: { answer: { content: { kind: 'refusal', text: 'Browser preview refusal' } } },
    })

    expect(parseAppSnapshot(await runtime.invoke('get_app_snapshot')).transcript[0]).toMatchObject({
      requestId: 'request-refused',
      answer: { content: { kind: 'refusal', text: 'Browser preview refusal' } },
    })
  })

  it('cancels matching work and prevents delayed callbacks from committing', async () => {
    vi.useFakeTimers()
    configureBrowserPreviewFixture({ delayMs: 20, chunks: ['Late ', 'output'] })
    const runtime = getBrowserPreviewInternals()
    const updates: unknown[] = []
    const result = runtime.invoke('submit_prompt', {
      requestId: 'request-cancel',
      prompt: 'Cancel this preview',
      updates: runtime.createChannel!((update) => updates.push(update)),
    })

    await vi.advanceTimersByTimeAsync(20)
    await expect(runtime.invoke('cancel_prompt', { requestId: 'request-cancel' })).resolves.toBe(true)
    await expect(result).resolves.toEqual({ kind: 'cancelled' })
    await vi.runAllTimersAsync()

    expect(updates).toEqual([{ kind: 'delta', text: 'Late ' }])
    expect(parseAppSnapshot(await runtime.invoke('get_app_snapshot'))).toMatchObject({
      transcript: [],
      activeRequest: null,
    })
    await expect(runtime.invoke('cancel_prompt', { requestId: 'request-cancel' })).resolves.toBe(false)
    await expect(runtime.invoke('cancel_prompt', { requestId: '' })).rejects.toMatchObject({ code: 'invalid_input' })
  })

  it('cancels when the command-scoped update receiver closes', async () => {
    configureBrowserPreviewFixture({ chunks: ['Receiver closed'] })
    const runtime = getBrowserPreviewInternals()

    await expect(runtime.invoke('submit_prompt', {
      requestId: 'request-closed',
      prompt: 'Close the receiver',
      updates: runtime.createChannel!(() => { throw new Error('receiver closed') }),
    })).resolves.toEqual({ kind: 'cancelled' })
    expect(parseAppSnapshot(await runtime.invoke('get_app_snapshot'))).toMatchObject({
      transcript: [],
      activeRequest: null,
    })
  })

  it('rejects concurrent admission while one preview execution is active', async () => {
    vi.useFakeTimers()
    configureBrowserPreviewFixture({ delayMs: 20, chunks: ['First response'] })
    const runtime = getBrowserPreviewInternals()
    const first = runtime.invoke('submit_prompt', {
      requestId: 'request-first',
      prompt: 'First prompt',
      updates: runtime.createChannel!(() => undefined),
    })

    await expect(runtime.invoke('submit_prompt', {
      requestId: 'request-second',
      prompt: 'Second prompt',
      updates: runtime.createChannel!(() => undefined),
    })).rejects.toMatchObject({ code: 'busy' })
    await runtime.invoke('cancel_prompt', { requestId: 'request-first' })
    await expect(first).resolves.toEqual({ kind: 'cancelled' })
  })

  it('rotates sessions on reset and ignores the retired execution', async () => {
    vi.useFakeTimers()
    configureBrowserPreviewFixture({ delayMs: 20, chunks: ['Retired response'] })
    const runtime = getBrowserPreviewInternals()
    const result = runtime.invoke('submit_prompt', {
      requestId: 'request-reset',
      prompt: 'Reset this preview',
      updates: runtime.createChannel!(() => undefined),
    })

    await expect(runtime.invoke('reset_session')).resolves.toMatchObject({ runtime_phase: 'sleeping' })
    await expect(result).resolves.toEqual({ kind: 'cancelled' })
    await vi.runAllTimersAsync()

    expect(parseAppSnapshot(await runtime.invoke('get_app_snapshot'))).toMatchObject({
      sessionId: '2',
      runtimePhase: 'sleeping',
      transcript: [],
      activeRequest: null,
    })
  })

  it('publishes complete validated preferences and rejects partial proposals atomically', async () => {
    const runtime = getBrowserPreviewInternals()
    const proposal = {
      instant: 'local-fast',
      deep: 'custom-sol-high',
      review: 'custom-luna-low',
      deep_enabled: false,
      review_enabled: false,
      prefetch: true,
      completion: false,
      response_profile: 'fast',
      tts_enabled: false,
      ui_theme: 'light',
      ui_text_size: 'large',
    }

    const returned = await runtime.invoke('set_preferences', { preferences: proposal }) as Record<string, unknown>
    expect(parseValidatedPreferences(returned)).toEqual({
      instant: 'local-fast',
      deep: 'custom-sol-high',
      review: 'custom-luna-low',
      deepEnabled: false,
      reviewEnabled: false,
      prefetch: true,
      completion: false,
      responseProfile: 'fast',
      ttsEnabled: false,
      uiTheme: 'light',
      uiTextSize: 'large',
      webEnabled: true,
      speechNetworkFallback: true,
      speechLanguage: '', speechTtsProvider: '', speechTtsVoice: '',
    })
    proposal.ui_theme = 'dark'
    returned['ui_theme'] = 'dark'
    await expect(runtime.invoke('set_preferences', { preferences: { ui_theme: 'dark' } })).rejects.toMatchObject({ code: 'invalid_input' })
    expect(parseAppSnapshot(await runtime.invoke('get_app_snapshot')).preferences).toMatchObject({
      uiTheme: 'light',
      uiTextSize: 'large',
    })
  })

  it('round-trips speech strings through preview snapshots and legacy settings without adding speech capability', async () => {
    const runtime = getBrowserPreviewInternals()
    const wire = await runtime.invoke('get_app_snapshot') as { preferences: Record<string, unknown> }
    const proposed = { ...wire.preferences, speech_language: 'en-AU', speech_tts_provider: 'synthetic.tts.engine', speech_tts_voice: 'é'.repeat(256) }
    expect(await runtime.invoke('set_preferences', { preferences: proposed })).toEqual(proposed)
    const settings = await runtime.invoke('get_assistant_settings') as Record<string, unknown>
    expect(settings).toMatchObject({ speech_language: 'en-AU', speech_tts_provider: 'synthetic.tts.engine', speech_tts_voice: 'é'.repeat(256) })
    const oldSettings = { ...settings }; delete oldSettings['speech_language']; delete oldSettings['speech_tts_provider']; delete oldSettings['speech_tts_voice']
    expect(await runtime.invoke('set_assistant_settings', { settings: oldSettings })).toMatchObject({ speech_language: 'en-AU', speech_tts_provider: 'synthetic.tts.engine', speech_tts_voice: 'é'.repeat(256) })
    for (const field of ['speech_language', 'speech_tts_provider', 'speech_tts_voice']) {
      for (const value of [null, true, 'é'.repeat(257)]) await expect(runtime.invoke('set_preferences', { preferences: { ...proposed, [field]: value } })).rejects.toMatchObject({ code: 'invalid_input' })
    }
    const snapshot = parseAppSnapshot(await runtime.invoke('get_app_snapshot'))
    expect(snapshot.preferences).toMatchObject({ speechLanguage: 'en-AU', speechTtsProvider: 'synthetic.tts.engine', speechTtsVoice: 'é'.repeat(256) })
    expect(snapshot.capabilities.find(({ id }) => id === 'tts')?.state).toBe('unavailable')
  })

  it('accepts old full preferences, persists new policies and still rejects unknown or malformed fields atomically', async () => {
    const runtime = getBrowserPreviewInternals()
    const wire = await runtime.invoke('get_app_snapshot') as { preferences: Record<string, unknown> }
    const proposal = { ...wire.preferences, web_enabled: false, speech_network_fallback: false }
    expect(parseValidatedPreferences(await runtime.invoke('set_preferences', { preferences: proposal }))).toMatchObject({ webEnabled: false, speechNetworkFallback: false })
    for (const bad of [{ ...proposal, web_enabled: 'false' }, { ...proposal, speech_network_fallback: null }, { ...proposal, unsupported: true }]) {
      await expect(runtime.invoke('set_preferences', { preferences: bad })).rejects.toMatchObject({ code: 'invalid_input' })
    }
    expect(parseAppSnapshot(await runtime.invoke('get_app_snapshot')).preferences).toMatchObject({ webEnabled: false, speechNetworkFallback: false })
  })

  it('rejects sparse fixture chunks before admitting work', async () => {
    const sparseChunks = Array<string>(1)

    expect(() => configureBrowserPreviewFixture({ chunks: sparseChunks })).toThrow('Preview chunks must contain bounded text')
    expect(parseAppSnapshot(await getBrowserPreviewInternals().invoke('get_app_snapshot'))).toMatchObject({
      runtimePhase: 'sleeping',
      transcript: [],
      activeRequest: null,
    })
  })

  it('tears down active work without allowing stale timers to publish', async () => {
    vi.useFakeTimers()
    configureBrowserPreviewFixture({ delayMs: 20, chunks: ['Never visible'] })
    const runtime = getBrowserPreviewInternals()
    const updates: unknown[] = []
    const result = runtime.invoke('submit_prompt', {
      requestId: 'request-teardown',
      prompt: 'Unmount preview',
      updates: runtime.createChannel!((update) => updates.push(update)),
    })

    teardownBrowserPreviewRuntime()
    await expect(result).resolves.toEqual({ kind: 'cancelled' })
    await vi.runAllTimersAsync()

    expect(updates).toEqual([])
    expect(parseAppSnapshot(await runtime.invoke('get_app_snapshot')).transcript).toEqual([])
  })

  it('rejects fixture delays that exceed the browser timer range', () => {
    expect(() => configureBrowserPreviewFixture({ delayMs: 1_073_741_824, chunks: ['one'] })).toThrow(
      'Preview delay exceeds the supported timer range',
    )
    expect(() => configureBrowserPreviewFixture({ delayMs: 1_073_741_823, chunks: ['one'] })).not.toThrow()
  })
})

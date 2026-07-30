import { afterEach, describe, expect, it, vi } from 'vitest'
import { TauriCommandError, androidPreferenceProposal, buildAndroidStartupState, cacheAndroidSnapshot, getAndroidPreferencePublicationRevision, invokeAndroidCommand, normalizeTauriError, setCachedAndroidPreferences, withoutAndroidEventListener } from './tauri'
import { missingPromptStreamingSupportError, parseAndroidPromptOutcome, parseAndroidPromptOutcomeForExecution, parseAndroidPromptUpdate } from './promptExecution'
import type { AppSnapshot, ValidatedPreferences } from './appSnapshot'
import { appendCanonicalTurn, canonicalExecutionKey, isExpectedCanonicalTurn, parseAppSnapshot, parseCanonicalTurn } from './appSnapshot'
import { initialUpdateState } from './useAppUpdates'

const preferences: ValidatedPreferences = {
  instant: 'local-fast', deep: 'opencode-sol-high', review: 'opencode-luna-low', deepEnabled: false, reviewEnabled: true,
  prefetch: true, completion: false, responseProfile: 'quality', ttsEnabled: true, uiTheme: 'dark', uiTextSize: 'large',
}
const snapshot = { runtimePhase: 'sleeping', preferences, capabilities: [], activeRequest: null } as unknown as AppSnapshot

afterEach(() => {
  delete window.__TAURI_INTERNALS__
  vi.unstubAllEnvs()
})

describe('Android runtime contract helpers', () => {
  it('projects zero-asset startup truthfully across every legacy capability', () => {
    const startup = buildAndroidStartupState(snapshot)
    expect(startup['supported_response_profiles']).toEqual([])
    expect(startup['voice_input_available']).toBe(false)
    expect(startup['prompt_cancellation_available']).toBe(true)
    expect(startup['capabilities']).toHaveLength(11)
    expect((startup['capabilities'] as Record<string, unknown>[]).every((item) => item['state'] === 'not_configured' && item['actual_provider'] === null)).toBe(true)
  })

  it('preserves all fields while mapping snake_case assistant settings', () => {
    expect(androidPreferenceProposal('set_assistant_settings', { settings: { instant: 'local-quality', deep_enabled: true, review_enabled: false, prefetch: false, completion: true, deep: 'custom-sol-high', review: 'custom-luna-low' } }, preferences)).toEqual({
      instant: 'local-quality', deep: 'custom-sol-high', review: 'custom-luna-low', deep_enabled: true, review_enabled: false, prefetch: false, completion: true,
      response_profile: 'quality', tts_enabled: true, ui_theme: 'dark', ui_text_size: 'large',
    })
  })

  it('strictly maps canonical prompt updates and outcomes', () => {
    expect(parseAndroidPromptUpdate({ kind: 'delta', text: 'hello' }, 'r')).toEqual({ requestId: 'r', kind: 'text', text: 'hello' })
    expect(parseAndroidPromptUpdate({ kind: 'stage', stage: 'deep', status: 'running', detail: null }, 'r')).toEqual({ requestId: 'r', kind: 'stage', stage: 'deep', status: 'running' })
    expect(parseAndroidPromptUpdate({ kind: 'correction', stage: 'review', text: 'new', correction: 'old was corrected' }, 'r')).toMatchObject({ kind: 'correction', correction: 'old was corrected' })
    expect(parseAndroidPromptUpdate({ kind: 'status', message: 'waiting' }, 'r')).toMatchObject({ kind: 'status', message: 'waiting' })
    const turn = { request_id: 'r', execution_id: { session_id: '1', generation: '1' }, user: { id: '1', role: 'user', content: { kind: 'text', text: 'question' } }, answer: { id: '2', role: 'assistant', content: { kind: 'refusal', text: 'cannot help' } }, final_stage: 'instant' }
    expect(parseAndroidPromptOutcome({ kind: 'refused', turn }, 'r')).toMatchObject({ outcome: 'completed', runtimePhase: 'sleeping', canonicalOutcome: 'refused' })
    const textTurn = { ...turn, answer: { ...turn.answer, content: { kind: 'text', text: 'answer' } } }
    expect(parseAndroidPromptOutcome({ kind: 'completed', turn: textTurn }, 'r')).toMatchObject({ outcome: 'completed', canonicalOutcome: 'completed' })
    expect(parseAndroidPromptOutcome({ kind: 'cancelled' }, 'r')).toMatchObject({ outcome: 'cancelled' })
    expect(() => parseAndroidPromptUpdate({ kind: 'delta', delta: 'wrong' }, 'r')).toThrow()
    expect(() => parseAndroidPromptOutcome({ kind: 'completed', turn: {} }, 'r')).toThrow()
    expect(() => parseAndroidPromptOutcome({ kind: 'completed', turn: { ...turn, request_id: 'other' } }, 'r')).toThrow()
    expect(() => parseAndroidPromptOutcome({ kind: 'completed', turn }, 'r')).toThrow()
    expect(() => parseAndroidPromptOutcome({ kind: 'refused', turn: { ...turn, answer: { ...turn.answer, content: { kind: 'text', text: 'answer' } } } }, 'r')).toThrow()
    expect(() => parseAndroidPromptOutcome({ kind: 'cancelled', turn }, 'r')).toThrow()
    expect(() => parseAndroidPromptOutcomeForExecution({ kind: 'completed', turn: {} }, 'r')).toThrow(TauriCommandError)
    expect(missingPromptStreamingSupportError(true)).toMatchObject({ code: 'internal', message: 'Prompt execution requires Channel support' })
    const parsedTurn = parseCanonicalTurn(turn)
    expect(appendCanonicalTurn([], parsedTurn)).toEqual([parsedTurn])
    expect(appendCanonicalTurn([parsedTurn], parsedTurn)).toEqual([parsedTurn])
    expect(isExpectedCanonicalTurn(parsedTurn, 'r', '1', new Set())).toBe(true)
    expect(isExpectedCanonicalTurn(parsedTurn, 'r', '2', new Set())).toBe(false)
    expect(isExpectedCanonicalTurn(parsedTurn, 'r', '1', new Set([canonicalExecutionKey(parsedTurn)]))).toBe(false)
  })

  it('maps legacy settings through only the canonical preference command', async () => {
    cacheAndroidSnapshot(snapshot)
    const invoke = vi.fn(async (command: string, args?: unknown) => {
      expect(command).toBe('set_preferences')
      const proposal = (args as { preferences: Record<string, unknown> }).preferences
      return { ...proposal, ui_theme: 'light' }
    })
    window.__TAURI_INTERNALS__ = { invoke }

    await expect(invokeAndroidCommand('set_ui_theme', { theme: 'light' })).resolves.toBe('light')
    expect(invoke).toHaveBeenCalledTimes(1)
    expect(invoke.mock.calls[0]?.[1]).toMatchObject({ preferences: { ui_theme: 'light', response_profile: 'quality' } })

    await expect(invokeAndroidCommand('unknown_desktop_command')).rejects.toMatchObject({ code: 'unavailable', message: 'Command is unavailable on Android' })
    expect(invoke).toHaveBeenCalledTimes(1)
  })

  it('serializes disjoint legacy preference changes against the latest cache', async () => {
    cacheAndroidSnapshot(snapshot)
    let resolveFirst: (value: unknown) => void = () => undefined
    const proposals: Record<string, unknown>[] = []
    window.__TAURI_INTERNALS__ = {
      invoke: vi.fn(async (_command, args) => {
        const proposal = (args as { preferences: Record<string, unknown> }).preferences
        proposals.push(proposal)
        if (proposals.length === 1) {
          return new Promise((resolve) => { resolveFirst = resolve })
        }
        return proposal
      }),
    }

    const theme = invokeAndroidCommand('set_ui_theme', { theme: 'light' })
    const textSize = invokeAndroidCommand('set_ui_text_size', { textSize: 'small' })
    await Promise.resolve()
    expect(proposals).toHaveLength(1)
    resolveFirst({ ...proposals[0], ui_theme: 'light' })
    await expect(theme).resolves.toBe('light')
    await expect(textSize).resolves.toBe('small')
    expect(proposals).toHaveLength(2)
    expect(proposals[1]).toMatchObject({ ui_theme: 'light', ui_text_size: 'small' })
  })

  it('publishes only snapshots accepted by the App ordering gate', async () => {
    const capability = (state: 'available' | 'unavailable') => ({
      id: 'custom_provider',
      state,
      reason: state === 'available' ? 'Ready' : 'Rejected',
      actual_provider: null,
    })
    const accepted = parseAppSnapshot({ ...wireSnapshot(), capabilities: [capability('available')] })
    const stale = { ...wireSnapshot(), capabilities: [capability('unavailable')] }
    cacheAndroidSnapshot(accepted)
    window.__TAURI_INTERNALS__ = { invoke: async () => stale }

    await invokeAndroidCommand('get_app_snapshot')
    const startup = await invokeAndroidCommand('get_startup_state') as { capabilities: Array<Record<string, unknown>> }
    expect(startup.capabilities.find((item) => item['id'] === 'custom_provider')?.['state']).toBe('available')

    cacheAndroidSnapshot(parseAppSnapshot(stale))
    const updated = await invokeAndroidCommand('get_startup_state') as { capabilities: Array<Record<string, unknown>> }
    expect(updated.capabilities.find((item) => item['id'] === 'custom_provider')?.['state']).toBe('unavailable')
  })

  it('preserves preferences changed after a snapshot request started', () => {
    cacheAndroidSnapshot(snapshot)
    const snapshotPreferenceRevision = getAndroidPreferencePublicationRevision()
    const updatedPreferences = { ...preferences, uiTheme: 'light' as const }
    setCachedAndroidPreferences(updatedPreferences)

    const published = cacheAndroidSnapshot(
      { ...snapshot, preferences: { ...preferences, uiTheme: 'dark' } },
      snapshotPreferenceRevision,
    )
    expect(published.preferences).toEqual(updatedPreferences)
    expect(androidPreferenceProposal('set_ui_text_size', { textSize: 'small' }, published.preferences))
      .toMatchObject({ ui_theme: 'light', ui_text_size: 'small' })
  })

  it('parses a native snapshot exactly once through the actual Android branch', async () => {
    vi.stubEnv('VITE_VOXGOLEM_RUNTIME', 'android')
    vi.resetModules()
    window.__TAURI_INTERNALS__ = { invoke: async () => wireSnapshot() }
    const [{ loadAppSnapshot }, { isAndroidRuntime }] = await Promise.all([
      import('./appSnapshot'),
      import('./tauri'),
    ])
    expect(isAndroidRuntime).toBe(true)
    await expect(loadAppSnapshot()).resolves.toMatchObject({ sessionId: '1', modelOptions: [] })
    vi.unstubAllEnvs()
  })

  it('removes event listening and preserves every typed command error code', () => {
    const listen = vi.fn()
    expect(withoutAndroidEventListener({ invoke: vi.fn(), listen }, true).listen).toBeUndefined()
    expect(withoutAndroidEventListener({ invoke: vi.fn(), listen }, false).listen).toBe(listen)
    for (const code of ['invalid_input', 'not_found', 'busy', 'unavailable', 'internal'] as const) {
      const error = normalizeTauriError({ code, message: 'safe message' })
      expect(error).toBeInstanceOf(TauriCommandError)
      expect(error).toMatchObject({ code, message: 'safe message' })
    }
    for (const malformed of [
      { code: 'unknown', message: 'safe' },
      { code: 'busy', message: '' },
      { code: 'busy', message: 'x'.repeat(513) },
      { code: 'busy' },
      null,
    ]) {
      expect(normalizeTauriError(malformed)).toMatchObject({ message: 'Native command failed' })
    }
    expect(normalizeTauriError(new Error('x'.repeat(1_000)))).toMatchObject({ code: 'internal', message: 'Native command failed' })
    expect(normalizeTauriError(new TauriCommandError('busy', 'x'.repeat(1_000)))).toMatchObject({ code: 'busy', message: 'Native command failed' })
    expect(new TauriCommandError('internal', '')).toMatchObject({ code: 'internal', message: 'Native command failed' })
    expect(() => parseCanonicalTurn({ ...turnFixture(), request_id: 'é'.repeat(65) })).toThrow()
    expect(() => parseCanonicalTurn({ ...turnFixture(), user: { id: '1', role: 'user', content: { kind: 'refusal', text: 'no' } } })).toThrow()
  })

  it('trims reconstructed canonical history by pair count and UTF-8 bytes', () => {
    let transcript = Array.from({ length: 20 }, (_, index) => parseCanonicalTurn(turnFixture(String(index + 1), 'x')))
    const newest = parseCanonicalTurn(turnFixture('21', 'new'))
    transcript = [...appendCanonicalTurn(transcript, newest)]
    expect(transcript).toHaveLength(20)
    expect(transcript[0]?.requestId).toBe('2')

    const large = 'é'.repeat(16_000)
    let byteBounded = [parseCanonicalTurn(turnFixture('1', large))]
    for (let index = 2; index <= 9; index += 1) byteBounded = [...appendCanonicalTurn(byteBounded, parseCanonicalTurn(turnFixture(String(index), large)))]
    expect(byteBounded[0]?.requestId).not.toBe('1')
    expect(() => parseAppSnapshot({ ...wireSnapshot(), transcript: Array.from({ length: 21 }, (_, index) => turnFixture(String(index + 1), 'x')) })).toThrow()
  })

  it('models Android updates as unsupported without a native retry', () => {
    expect(initialUpdateState(true, true)).toEqual({ kind: 'unsupported', reason: 'Desktop updates are not available on Android' })
  })
})

function turnFixture(requestId = 'r', text = 'question'): Record<string, unknown> {
  return { request_id: requestId, execution_id: { session_id: '1', generation: requestId === 'r' ? '1' : requestId }, user: { id: '1', role: 'user', content: { kind: 'text', text } }, answer: { id: '2', role: 'assistant', content: { kind: 'text', text: 'answer' } }, final_stage: 'instant' }
}

function wireSnapshot(): Record<string, unknown> {
  return { session_id: '1', runtime_phase: 'sleeping', transcript: [], active_request: null, capabilities: [], model_options: [], preferences: { instant: 'local-fast', deep: 'opencode-sol-high', review: 'opencode-sol-high', deep_enabled: false, review_enabled: false, prefetch: false, completion: false, response_profile: 'fast', tts_enabled: false, ui_theme: 'dark', ui_text_size: 'medium' } }
}

import { afterEach, describe, expect, it, vi } from 'vitest'
import { loadAppSnapshot, parseAppSnapshot } from './appSnapshot'

function validSnapshot(): Record<string, unknown> {
  return {
    session_id: '18446744073709551615',
    runtime_phase: 'executing',
    transcript: [{
      request_id: 'request-1',
      execution_id: { session_id: '4', generation: '9' },
      user: { id: '1', role: 'user', content: { kind: 'text', text: 'question' } },
      answer: { id: '2', role: 'assistant', content: { kind: 'refusal', text: 'cannot answer' } },
      final_stage: 'review',
    }],
    active_request: {
      execution_id: { session_id: '18446744073709551615', generation: '10' },
      request_id: 'request-2',
      prompt: 'next question',
    },
    capabilities: [{ id: 'custom_provider', state: 'available', reason: 'ready', actual_provider: 'cpu' }],
    model_options: [{ id: 'custom-sol-high', label: 'Custom: GPT-5.6 Sol High', available: true, unavailable_reason: null }],
    preferences: {
      instant: 'custom-sol-high',
      deep: 'opencode-sol-high',
      review: 'custom-luna-low',
      deep_enabled: true,
      review_enabled: false,
      prefetch: true,
      completion: false,
      response_profile: 'quality',
      tts_enabled: true,
      ui_theme: 'light',
      ui_text_size: 'extra_large',
    },
  }
}

afterEach(() => {
  Reflect.deleteProperty(window, '__TAURI_INTERNALS__')
})

describe('app snapshot contract', () => {
  it.each(['custom-sol-high', 'custom-sol-none', 'custom-luna-none', 'custom-luna-low'] as const)(
    'accepts the current Custom Instant selection %s in canonical preferences',
    (instant) => {
      const wire = validSnapshot()
      wire['preferences'] = { ...(wire['preferences'] as Record<string, unknown>), instant }
      expect(parseAppSnapshot(wire).preferences.instant).toBe(instant)
    },
  )
  it('parses the complete Rust wire shape without numeric ID coercion', () => {
    const snapshot = parseAppSnapshot(validSnapshot())
    expect(snapshot.sessionId).toBe('18446744073709551615')
    expect(snapshot.transcript[0]).toMatchObject({
      requestId: 'request-1',
      executionId: { sessionId: '4', generation: '9' },
      answer: { role: 'assistant', content: { kind: 'refusal', text: 'cannot answer' } },
      finalStage: 'review',
    })
    expect(snapshot.activeRequest?.executionId.generation).toBe('10')
    expect(snapshot.capabilities[0]).toMatchObject({ actualProvider: 'cpu' })
    expect(snapshot.modelOptions[0]).toMatchObject({ id: 'custom-sol-high', available: true })
    expect(snapshot.preferences).toEqual({
      instant: 'custom-sol-high', deep: 'opencode-sol-high', review: 'custom-luna-low',
      deepEnabled: true, reviewEnabled: false, prefetch: true, completion: false,
      responseProfile: 'quality', ttsEnabled: true, uiTheme: 'light', uiTextSize: 'extra_large',
    })
  })

  it.each([
    4,
    '01',
    '-1',
    '18446744073709551616',
  ])('rejects non-canonical wire ID %j', (sessionId) => {
    expect(() => parseAppSnapshot({ ...validSnapshot(), session_id: sessionId })).toThrow(/session_id/)
  })

  it('rejects malformed nested turns and active requests', () => {
    const wrongRole = validSnapshot()
    const turn = (wrongRole['transcript'] as Record<string, unknown>[])[0]!
    turn['user'] = { id: '1', role: 'assistant', content: { kind: 'text', text: 'question' } }
    expect(() => parseAppSnapshot(wrongRole)).toThrow(/user role/)

    const badActive = validSnapshot()
    badActive['active_request'] = { execution_id: { session_id: '1', generation: 2 }, request_id: 'r', prompt: 'p' }
    expect(() => parseAppSnapshot(badActive)).toThrow(/generation/)
  })

  it('rejects unsupported enums, invalid nullability, duplicates, and UTF-8 byte overflow', () => {
    const badPhase = validSnapshot(); badPhase['runtime_phase'] = 'ready'
    expect(() => parseAppSnapshot(badPhase)).toThrow(/runtime_phase/)

    const badProvider = validSnapshot()
    ;(badProvider['capabilities'] as Record<string, unknown>[])[0]!['actual_provider'] = undefined
    expect(() => parseAppSnapshot(badProvider)).toThrow(/actual_provider/)

    const duplicate = validSnapshot()
    duplicate['model_options'] = [
      { id: 'same', label: 'One', available: true, unavailable_reason: null },
      { id: 'same', label: 'Two', available: false, unavailable_reason: 'no' },
    ]
    expect(() => parseAppSnapshot(duplicate)).toThrow(/duplicate model option/)

    const overflow = validSnapshot()
    ;(overflow['capabilities'] as Record<string, unknown>[])[0]!['reason'] = '界'.repeat(171)
    expect(() => parseAppSnapshot(overflow)).toThrow(/reason/)
  })

  it.each(['transcript', 'capabilities', 'model_options'] as const)('rejects a sparse %s array', (field) => {
    const sparse = validSnapshot()
    sparse[field] = Array(1)
    expect(() => parseAppSnapshot(sparse)).toThrow()
  })

  it('loads and parses get_app_snapshot through the shared Tauri adapter', async () => {
    const invoke = vi.fn().mockResolvedValue(validSnapshot())
    window.__TAURI_INTERNALS__ = { invoke }
    const snapshot = await loadAppSnapshot()
    expect(invoke).toHaveBeenCalledWith('get_app_snapshot', undefined)
    expect(snapshot.runtimePhase).toBe('executing')
  })
})

import { describe, expect, it } from 'vitest'
import { appSnapshot, canonicalTurn } from '../test/sessionFixtures'
import { parseAppSnapshot } from './appSnapshot'
import { parseCanonicalPromptOutcome } from './promptExecution'

const replica = '00000000-0000-4000-8000-000000000001'
const unsaved = () => ({ request_id: 'synthetic-request', execution_id: { session_id: `${replica}:1`, generation: `${replica}:2` }, prompt: 'Synthetic question', answer: { kind: 'text', text: 'Synthetic answer' }, sources: [], final_stage: 'instant', reason: 'storage_error' })

describe('current durable outcome contract', () => {
  it.each(['\uFEFF', ' \uFEFF\t'])('preserves opaque correlation accepted by Rust whitespace validation: %j', (requestId) => {
    const result = { ...unsaved(), request_id: requestId }
    expect(parseCanonicalPromptOutcome({ kind: 'unsaved', save_disposition: 'unsaved', unsaved_result: result }, requestId).unsavedResult?.requestId).toBe(requestId)
  })
  it.each(['\u0085', ' \u0085\n'])('rejects correlation containing only Unicode White_Space: %j', (requestId) => {
    expect(() => parseAppSnapshot(appSnapshot([], {}, { unsaved_result: { ...unsaved(), request_id: requestId } }))).toThrow()
  })
  it('rejects numeric unsaved execution identifiers and blank renderer correlation', () => {
    for (const mutation of [{ request_id: ' \n' }, { execution_id: { session_id: '1', generation: `${replica}:2` } }, { execution_id: { session_id: `${replica}:1`, generation: '2' } }]) {
      expect(() => parseAppSnapshot(appSnapshot([], {}, { unsaved_result: { ...unsaved(), ...mutation } }))).toThrow()
    }
  })
  it('accepts only a bounded content-free explicit error terminal', () => {
    const error = { kind: 'error', request_id: 'synthetic-request', runtime_phase: 'sleeping', error_message: 'Synthetic provider failure' }
    expect(parseCanonicalPromptOutcome(error, 'synthetic-request')).toEqual({ requestId: 'synthetic-request', runtimePhase: 'sleeping', outcome: 'error', errorMessage: 'Synthetic provider failure' })
    for (const mutation of [{ request_id: 'synthetic-other' }, { error_message: '' }, { error_message: 'é'.repeat(2049) }, { runtime_phase: 'unknown' }, { turn: null }, { unsaved_result: null }, { save_disposition: 'saved' }]) {
      expect(() => parseCanonicalPromptOutcome({ ...error, ...mutation }, 'synthetic-request')).toThrow()
    }
  })
  it('requires an explicit snapshot unsaved projection', () => {
    const wire = appSnapshot()
    delete wire['unsaved_result']
    expect(() => parseAppSnapshot(wire)).toThrow(/unsaved/)
  })
  it('accepts distributed identities on desktop without an assistant discriminator', () => {
    const wire = appSnapshot([], {}, { session_id: `${replica}:1`, unsaved_result: unsaved() })
    expect(parseAppSnapshot(wire).sessionId).toBe(`${replica}:1`)
  })
  it('rejects saved receipts without saved disposition', () => {
    expect(() => parseCanonicalPromptOutcome({ kind: 'completed', turn: canonicalTurn('1', '2', 'Synthetic question', 'Synthetic answer', 'synthetic-request') }, 'synthetic-request')).toThrow(/disposition/)
  })
  it('rejects content or disposition on cancellation', () => {
    for (const extra of [{ save_disposition: 'saved' }, { unsaved_result: unsaved() }, { turn: null }]) {
      expect(() => parseCanonicalPromptOutcome({ kind: 'cancelled', ...extra }, 'synthetic-request')).toThrow()
    }
  })
  it('accepts saved text and refusal, without allowing optional projection failure to revoke saving', () => {
    for (const kind of ['completed', 'refused'] as const) {
      const turn = canonicalTurn('1', '2', 'Synthetic question', 'Synthetic answer', 'synthetic-request', kind === 'refused' ? 'refusal' : 'text')
      expect(parseCanonicalPromptOutcome({ kind, save_disposition: 'saved', turn, app_snapshot: { invalid: true } }, 'synthetic-request')).toMatchObject({ saveDisposition: 'saved', canonicalOutcome: kind, canonicalTurn: { requestId: 'synthetic-request' } })
      expect(() => parseCanonicalPromptOutcome({ kind, save_disposition: 'saved', turn, unsaved_result: null }, 'synthetic-request')).toThrow()
    }
  })
  it('accepts unsaved text/refusal projections separately from canonical history', () => {
    for (const kind of ['text', 'refusal'] as const) {
      const result = { ...unsaved(), answer: { kind, text: 'Synthetic result' } }
      const parsed = parseCanonicalPromptOutcome({ kind: 'unsaved', save_disposition: 'unsaved', unsaved_result: result }, 'synthetic-request')
      expect(parsed.unsavedResult?.answer.kind).toBe(kind)
      expect(parsed.canonicalTurn).toBeUndefined()
      const snapshot = parseAppSnapshot(appSnapshot([], {}, { unsaved_result: result }))
      expect(snapshot.transcript).toHaveLength(0)
      expect(snapshot.unsavedResult?.requestId).toBe('synthetic-request')
    }
  })
  it('accepts the exact shared UTF-8 correlation and completed-content boundaries without truncation', () => {
    const result = { ...unsaved(), request_id: 'é'.repeat(64), prompt: 'p'.repeat(32 * 1024), answer: { kind: 'text', text: 'a'.repeat(224 * 1024) }, reason: 'context_unavailable' }
    const parsed = parseCanonicalPromptOutcome({ kind: 'unsaved', save_disposition: 'unsaved', unsaved_result: result }, result.request_id)
    expect(parsed.unsavedResult?.answer.text).toBe(result.answer.text)
    expect(parsed.unsavedResult?.reason).toBe('context_unavailable')
    expect(() => parseAppSnapshot(appSnapshot([], {}, { unsaved_result: { ...result, answer: { ...result.answer, text: `${result.answer.text}a` } } }))).toThrow(/bounds/)
  })
  it('rejects malformed, oversized, source-invalid and identity-invalid unsaved projections', () => {
    const mutations = [
      { request_id: 'é'.repeat(65) }, { request_id: '' }, { prompt: 'x'.repeat(32 * 1024 + 1) }, { prompt: ' ' },
      { answer: { kind: 'text', text: 'x'.repeat(256 * 1024) } }, { answer: { kind: 'text', text: ' ' } },
      { answer: { kind: 'refusal', text: 'Synthetic refusal' }, sources: [{ url: 'https://example.test/source', title: 'Synthetic title' }] },
      { sources: [{ url: 'javascript:alert(1)', title: 'Synthetic invalid source' }] },
      { reason: 'retry' }, { operation_id: '1' }, { execution_id: { session_id: `${replica}:01`, generation: '2' } },
    ]
    for (const mutation of mutations) {
      expect(() => parseAppSnapshot(appSnapshot([], {}, { unsaved_result: { ...unsaved(), ...mutation } }))).toThrow()
    }
    for (const extra of [{ turn: {} }, { save_disposition: 'saved' }, { unsaved_result: null }]) {
      expect(() => parseCanonicalPromptOutcome({ kind: 'unsaved', save_disposition: 'unsaved', unsaved_result: unsaved(), ...extra }, 'synthetic-request')).toThrow()
    }
    expect(() => parseCanonicalPromptOutcome({ kind: 'unsaved', save_disposition: 'unsaved', unsaved_result: unsaved() }, 'synthetic-other')).toThrow()
  })
})

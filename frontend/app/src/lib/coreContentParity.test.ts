import { describe, expect, it } from 'vitest'
import { parseAppSnapshot, parseCanonicalTurn, parseUnsavedResult } from './appSnapshot'
import { parseDesktopRuntimeRead } from './desktopRuntime'
import { parseSourceLinks } from './answerSources'
import { appSnapshot, canonicalTurn, durableId } from '../test/sessionFixtures'

const projection = () => ({ request_id: 'synthetic-request', execution_id: { session_id: durableId(1), generation: durableId(2) }, prompt: 'Synthetic prompt', answer: { kind: 'text', text: 'Synthetic answer' }, sources: [], final_stage: 'instant', reason: 'storage_error' })
describe('Rust completed-content whitespace parity', () => {
  it.each(['text', 'refusal'] as const)('preserves native-valid BOM prompt and %s answer bytes', (kind) => {
    const wire = { ...projection(), prompt: '\uFEFF', answer: { kind, text: '\uFEFF' } }
    expect(parseUnsavedResult(wire)).toMatchObject({ prompt: '\uFEFF', answer: { kind, text: '\uFEFF' } })
    expect(parseAppSnapshot(appSnapshot([], {}, { unsaved_result: wire })).unsavedResult?.answer.text).toBe('\uFEFF')
  })
  it.each(['prompt', 'text', 'refusal'] as const)('rejects native-whitespace-only NEL in unsaved %s', (field) => {
    const wire = field === 'prompt' ? { ...projection(), prompt: '\u0085' } : { ...projection(), answer: { kind: field, text: '\u0085' } }
    expect(() => parseUnsavedResult(wire)).toThrow(/empty/)
  })
  it.each([['BOM', '\uFEFF', true], ['NEL', '\u0085', false]] as const)('matches native %s prompts across saved, active and observer projections', (_name, prompt, valid) => {
    const turn = canonicalTurn('1', '2', prompt, 'Synthetic answer', 'synthetic-request')
    const active = { request_id: 'synthetic-request', execution_id: { session_id: durableId(1), generation: durableId(2) }, prompt }
    const observer = { revision: 1, snapshot: { revision: 1, voice: { auto_stop_on_silence: true, auto_start_available: false, capture_id: null, state: 'stopped', runtime_phase: 'sleeping', wake_confidence: null, partial_transcript: null, error: null }, speech: { playback_id: null, pending: false, playing: false }, prompt: { ...active, updates: [], result: null } }, events: [] }
    const checks = [() => parseCanonicalTurn(turn).user.content.text, () => parseAppSnapshot(appSnapshot([], {}, { active_request: active })).activeRequest?.prompt, () => parseDesktopRuntimeRead(observer).snapshot?.prompt?.prompt]
    for (const check of checks) { if (valid) expect(check()).toBe(prompt); else expect(check).toThrow() }
  })
  it('preserves native-valid BOM source title and URL path without normalization', () => {
    const sources = [{ title: '\uFEFF', url: 'https://example.test/\uFEFF' }]
    expect(parseSourceLinks(sources)).toEqual(sources)
    expect(parseUnsavedResult({ ...projection(), sources }).sources).toEqual(sources)
  })
  it.each(['\u0085', '\u0000', '\u0001', '\u007f', '\u009f'])('rejects native-forbidden URL whitespace/control %j', (character) => {
    expect(() => parseSourceLinks([{ title: 'Synthetic source', url: `https://example.test/${character}` }])).toThrow()
  })
  it('rejects native-whitespace-only NEL source titles', () => {
    expect(() => parseSourceLinks([{ title: '\u0085', url: 'https://example.test/source' }])).toThrow()
  })
})

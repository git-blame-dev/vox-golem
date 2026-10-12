import { describe, expect, it } from 'vitest'
import { canonicalMessageId, projectCommittedTranscript, replaceCommittedTranscript, settleCanonicalAnswerStages } from './committedTranscript'
import { parseCanonicalTurn } from './lib/appSnapshot'

const id = (sequence: number): string => `11111111-1111-4111-8111-111111111111:${sequence}`
function encoderTurn(sequence: number) {
  return parseCanonicalTurn({ request_id: `synthetic-request-${sequence}`, execution_id: { session_id: id(1), generation: id(sequence - 1) },
    user: { id: id(sequence), role: 'user', content: { kind: 'text', text: `Synthetic prompt ${sequence}` } },
    answer: { id: id(sequence), role: 'assistant', content: { kind: 'text', text: `Synthetic answer ${sequence}` } }, final_stage: 'instant' }, true)
}

describe('actual Android encoder-shaped transcript projection', () => {
  it('settles transient stages against the canonical final stage without replacing recorded terminal details', () => {
    expect(settleCanonicalAnswerStages([{ stage: 'instant', status: 'running' }, { stage: 'deep', status: 'queued' }, { stage: 'review', status: 'queued' }], 'deep')).toEqual([
      { stage: 'instant', status: 'completed' }, { stage: 'deep', status: 'completed' }, { stage: 'review', status: 'stale', detail: 'Not required for this answer' },
    ])
    expect(settleCanonicalAnswerStages([{ stage: 'instant', status: 'completed' }], 'review')).toEqual([{ stage: 'instant', status: 'completed' }, { stage: 'review', status: 'completed' }])
  })
  it('retains settled stage failures and correction details on the same canonical answer across snapshots', () => {
    const turn = { ...encoderTurn(3), finalStage: 'deep' as const }
    const stages = [{ stage: 'instant' as const, status: 'completed' as const }, { stage: 'deep' as const, status: 'corrected' as const, detail: 'Correction: synthetic correction' }, { stage: 'review' as const, status: 'failed' as const, detail: 'Synthetic review provider failed' }]
    const projected = projectCommittedTranscript([turn]).map((message) => message.role === 'assistant' ? { ...message, answerStage: { stages, priorVersions: [] } } : message)
    const repeated = replaceCommittedTranscript(replaceCommittedTranscript(projected, [turn]), [turn])
    expect(repeated[1]?.answerStage?.stages).toEqual(stages)
    expect(repeated[1]?.id).toBe(canonicalMessageId(turn.answer))
    expect(repeated[1]?.content).toBe(turn.answer.content.text)
  })
  it('does not carry presentation metadata onto a changed canonical answer with the same ID', () => {
    const turn = encoderTurn(3)
    const projected = projectCommittedTranscript([turn]).map((message) => message.role === 'assistant' ? { ...message, answerStage: { stages: [{ stage: 'review' as const, status: 'failed' as const, detail: 'Synthetic old failure' }], sources: [{ url: 'https://example.test/old', title: 'Synthetic old source' }], priorVersions: [{ id: 'synthetic-old', text: 'Synthetic old version' }] } } : message)
    const changed = { ...turn, answer: { ...turn.answer, content: { kind: 'refusal' as const, text: 'Synthetic new refusal' } } }
    const reconciled = replaceCommittedTranscript(projected, [changed])
    expect(reconciled[1]?.contentKind).toBe('refusal')
    expect(reconciled[1]?.answerStage?.priorVersions).toEqual([])
    expect(reconciled[1]?.answerStage?.sources).toBeUndefined()
    expect(reconciled[1]?.answerStage?.stages).toEqual([{ stage: 'instant', status: 'completed' }])
  })
  it('leaves desktop decimal message identities unchanged', () => {
    expect(canonicalMessageId({ id: '18446744073709551615', role: 'assistant' })).toBe('18446744073709551615')
  })
  it('keeps domain IDs unchanged while giving each rendered role a unique identity', () => {
    const turn = encoderTurn(3)
    const messages = projectCommittedTranscript([turn])
    expect(turn.user.id).toBe(turn.answer.id)
    expect(new Set(messages.map((message) => message.id)).size).toBe(2)
  })
  it('preserves metadata on the matching answer through reconciliation and conversation switching', () => {
    const first = encoderTurn(3)
    const second = encoderTurn(5)
    const initial = projectCommittedTranscript([first, second]).map((message) => message.role !== 'assistant' ? message : {
      ...message, customDirectTimings: { firstTextMs: message.content.endsWith('3') ? 13 : 25, completedMs: 50 },
      answerStage: { ...message.answerStage!, priorVersions: [{ id: `synthetic-prior-${message.id}`, stage: 'instant' as const, text: `Prior ${message.content}`, correction: 'Synthetic correction' }] },
    })
    const reconciled = replaceCommittedTranscript(initial, [first, second])
    const switched = replaceCommittedTranscript(reconciled, [second])
    const repeated = replaceCommittedTranscript(switched, [second])
    expect(repeated[0]?.role).toBe('user')
    expect(repeated[0]?.customDirectTimings).toBeUndefined()
    expect(repeated[1]?.customDirectTimings?.firstTextMs).toBe(25)
    expect(repeated[1]?.answerStage?.priorVersions[0]?.text).toBe('Prior Synthetic answer 5')
    expect(new Set(repeated.map((message) => message.id)).size).toBe(2)
  })
})

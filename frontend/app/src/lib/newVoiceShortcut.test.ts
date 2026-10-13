import { describe, expect, it } from 'vitest'
import { isConfirmedNewVoiceContext, parseNewVoicePinStatus, parsePendingNewVoiceAction } from './newVoiceShortcut'
import type { AppSnapshot } from './appSnapshot'

describe('one-time native voice shortcut boundary', () => {
  it('does not call a requested pin confirmed before launcher completion', () => {
    expect(parseNewVoicePinStatus({ supported: true, pinned: false, requested: true }).pinned).toBe(false)
    expect(parseNewVoicePinStatus({ supported: true, pinned: true, requested: false }).pinned).toBe(true)
  })
  it('accepts only bounded native action identities and no incoming prompt payload', () => {
    expect(parsePendingNewVoiceAction({ token: '11111111-1111-4111-8111-111111111111', error: null, prompt: 'Ignored synthetic foreign prompt' })).toEqual({ token: '11111111-1111-4111-8111-111111111111', error: null, mode: 'ask' })
    for (const token of ['synthetic-prompt', '', '11111111-1111-4111-8111-111111111111/prompt']) expect(() => parsePendingNewVoiceAction({ token, error: null })).toThrow()
    expect(() => parsePendingNewVoiceAction({ token: null, error: 'é'.repeat(257) })).toThrow()
  })
  it('delivers only Ask or Talk policies, defaulting legacy pinned shortcuts to Ask', () => {
    const token = '22222222-2222-4222-8222-222222222222'
    expect(parsePendingNewVoiceAction({ token, error: null, mode: 'talk' })).toEqual({ token, error: null, mode: 'talk' })
    expect(parsePendingNewVoiceAction({ token, error: null, mode: 'ask' }).mode).toBe('ask')
    for (const mode of ['note', 'dictation', '', null, true, {}]) expect(() => parsePendingNewVoiceAction({ token, error: null, mode })).toThrow()
  })
  it('requires a distinct durable, empty, branch-free and error-free canonical context', () => {
    const oldSession = '11111111-1111-4111-8111-111111111111:1'
    const snapshot = {
      sessionId: '11111111-1111-4111-8111-111111111111:2', transcript: [], activeRequest: null,
      assistant: { revision: '1', conversations: [], conversationCount: 0, conversationOffset: 0, selectedHead: null, branchChoices: [], branchCount: 0, branchOffset: 0, draft: '', interrupted: false, error: null },
    } satisfies Pick<AppSnapshot, 'sessionId' | 'transcript' | 'activeRequest' | 'assistant'>
    expect(isConfirmedNewVoiceContext(snapshot, oldSession)).toBe(true)
    for (const sessionId of ['0', '2', oldSession]) expect(isConfirmedNewVoiceContext({ ...snapshot, sessionId }, oldSession)).toBe(false)
    expect(isConfirmedNewVoiceContext({ sessionId: snapshot.sessionId, transcript: [], activeRequest: null }, oldSession)).toBe(false)
    expect(isConfirmedNewVoiceContext({ ...snapshot, assistant: { ...snapshot.assistant, branchCount: 1 } }, oldSession)).toBe(false)
    expect(isConfirmedNewVoiceContext({ ...snapshot, assistant: { ...snapshot.assistant, error: 'storage_error' } }, oldSession)).toBe(false)
  })
})

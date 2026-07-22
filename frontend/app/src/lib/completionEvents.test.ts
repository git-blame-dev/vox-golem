import { describe, expect, it } from 'vitest'
import { parseCompletionEvent } from './completionEvents'

describe('parseCompletionEvent', () => {
  it('parses typed completion events', () => {
    expect(parseCompletionEvent({ source: 'typed', revision: 3, voice_session_id: null, suffix: ' draft' })).toEqual({
      source: 'typed', revision: 3, voiceSessionId: null, suffix: ' draft',
    })
  })

  it('parses voice completion events', () => {
    expect(parseCompletionEvent({ source: 'voice', revision: 4, voice_session_id: 9, suffix: null })).toEqual({
      source: 'voice', revision: 4, voiceSessionId: 9, suffix: null,
    })
  })

  it.each([
    { source: 'unknown', revision: 0, voice_session_id: null, suffix: null },
    { source: 'typed', revision: -1, voice_session_id: null, suffix: null },
    { source: 'typed', revision: Number.MAX_SAFE_INTEGER + 1, voice_session_id: null, suffix: null },
    { source: 'typed', revision: 0, voice_session_id: 1, suffix: null },
    { source: 'voice', revision: 0, voice_session_id: null, suffix: null },
    { source: 'voice', revision: 0, voice_session_id: Number.MAX_SAFE_INTEGER + 1, suffix: null },
    { source: 'typed', revision: 0, voice_session_id: null, suffix: 42 },
  ])('rejects invalid payload %#', (payload) => {
    expect(() => parseCompletionEvent(payload)).toThrow()
  })
})

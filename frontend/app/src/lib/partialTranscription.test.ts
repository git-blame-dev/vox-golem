import { describe, expect, it } from 'vitest'
import { acceptsPartialTranscriptionEvent, parsePartialTranscriptionEvent } from './partialTranscription'

describe('partial transcription events', () => {
  it('parses the wire payload and rejects malformed values', () => {
    expect(parsePartialTranscriptionEvent({ session_id: 2, revision: 3, text: 'hel' })).toEqual({ sessionId: 2, revision: 3, text: 'hel' })
    expect(() => parsePartialTranscriptionEvent({ session_id: 2, revision: '3', text: 'hel' })).toThrow()
    expect(() => parsePartialTranscriptionEvent({ session_id: -1, revision: 3, text: 'hel' })).toThrow()
    expect(() => parsePartialTranscriptionEvent({ session_id: 2, revision: Number.MAX_SAFE_INTEGER + 1, text: 'hel' })).toThrow()
  })

  it('accepts the first session and newer revisions only within that session', () => {
    expect(acceptsPartialTranscriptionEvent(null, { sessionId: 2, revision: 1, text: '' })).toBe(true)
    const current = { sessionId: 2, revision: 3, active: true }
    expect(acceptsPartialTranscriptionEvent(current, { sessionId: 1, revision: 9, text: '' })).toBe(false)
    expect(acceptsPartialTranscriptionEvent(current, { sessionId: 2, revision: 2, text: '' })).toBe(false)
    expect(acceptsPartialTranscriptionEvent(current, { sessionId: 2, revision: 3, text: '' })).toBe(false)
    expect(acceptsPartialTranscriptionEvent(current, { sessionId: 2, revision: 4, text: '' })).toBe(true)
    expect(acceptsPartialTranscriptionEvent(current, { sessionId: 3, revision: 0, text: '' })).toBe(false)
  })

  it('requires the caller to open a new session after finalization', () => {
    const current = { sessionId: 2, revision: 3, active: false }
    expect(acceptsPartialTranscriptionEvent(current, { sessionId: 2, revision: 4, text: '' })).toBe(false)
    expect(acceptsPartialTranscriptionEvent(current, { sessionId: 3, revision: 1, text: '' })).toBe(false)
    expect(acceptsPartialTranscriptionEvent(null, { sessionId: 3, revision: 1, text: '' })).toBe(true)
  })
})

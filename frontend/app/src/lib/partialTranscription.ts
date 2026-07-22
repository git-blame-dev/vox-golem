export interface PartialTranscriptionEvent {
  readonly sessionId: number
  readonly revision: number
  readonly text: string
}

export function parsePartialTranscriptionEvent(payload: unknown): PartialTranscriptionEvent {
  if (typeof payload !== 'object' || payload === null) {
    throw new Error('Partial transcription event must be an object')
  }
  const record = payload as Record<string, unknown>
  if (
    typeof record['session_id'] !== 'number' ||
    !Number.isSafeInteger(record['session_id']) ||
    record['session_id'] < 0 ||
    typeof record['revision'] !== 'number' ||
    !Number.isSafeInteger(record['revision']) ||
    record['revision'] < 0 ||
    typeof record['text'] !== 'string'
  ) {
    throw new Error('Partial transcription event has an invalid payload')
  }
  return {
    sessionId: record['session_id'],
    revision: record['revision'],
    text: record['text'],
  }
}

export function acceptsPartialTranscriptionEvent(
  current: { readonly sessionId: number; readonly revision: number; readonly active: boolean } | null,
  event: PartialTranscriptionEvent,
): boolean {
  if (current === null) return true
  return current.active && event.sessionId === current.sessionId && event.revision > current.revision
}

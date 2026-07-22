export type CompletionSource = 'typed' | 'voice'
export interface CompletionEvent { readonly source: CompletionSource; readonly revision: number; readonly voiceSessionId: number | null; readonly suffix: string | null }
export function parseCompletionEvent(payload: unknown): CompletionEvent {
  if (typeof payload !== 'object' || payload === null) throw new Error('Invalid completion event')
  const value = payload as Record<string, unknown>
  if (value['source'] !== 'typed' && value['source'] !== 'voice') throw new Error('Invalid completion source')
  if (typeof value['revision'] !== 'number' || !Number.isSafeInteger(value['revision']) || value['revision'] < 0) throw new Error('Invalid completion revision')
  if (value['voice_session_id'] !== null && (typeof value['voice_session_id'] !== 'number' || !Number.isSafeInteger(value['voice_session_id']) || value['voice_session_id'] < 0)) throw new Error('Invalid voice session id')
  if (value['suffix'] !== null && typeof value['suffix'] !== 'string') throw new Error('Invalid completion suffix')
  if (value['source'] === 'typed' && value['voice_session_id'] !== null) throw new Error('Typed completion cannot have a voice session')
  if (value['source'] === 'voice' && value['voice_session_id'] === null) throw new Error('Voice completion requires a voice session')
  return { source: value['source'], revision: value['revision'], voiceSessionId: value['voice_session_id'], suffix: value['suffix'] }
}

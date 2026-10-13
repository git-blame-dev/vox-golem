import { isRecord, utf8ByteLength } from './wire'
import type { AppSnapshot } from './appSnapshot'

export const NEW_VOICE_SHORTCUT_EVENT = 'voxgolem-new-voice-chat'
export const NEW_VOICE_SHORTCUT_COMMANDS = ['new_voice_shortcut_status', 'pin_new_voice_shortcut', 'consume_new_voice_shortcut', 'stop_send_new_voice_shortcut', 'release_new_voice_shortcut', 'prepare_new_voice_shortcut'] as const
export function pendingNewVoiceToken(value: unknown): string | null {
  return isRecord(value) && typeof value['token'] === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(value['token']) ? value['token'] : null
}
export function isConfirmedNewVoiceContext(snapshot: Pick<AppSnapshot, 'sessionId' | 'transcript' | 'activeRequest' | 'assistant'>, oldSession: string | null): boolean {
  return snapshot.sessionId.includes(':') && snapshot.sessionId !== oldSession && snapshot.transcript.length === 0 && snapshot.activeRequest === null && snapshot.assistant !== undefined && snapshot.assistant.selectedHead === null && snapshot.assistant.branchCount === 0 && snapshot.assistant.error === null
}
export interface NewVoicePinStatus { readonly supported: boolean; readonly pinned: boolean; readonly requested: boolean }
export function parseNewVoicePinStatus(value: unknown): NewVoicePinStatus {
  if (!isRecord(value) || typeof value['supported'] !== 'boolean' || typeof value['pinned'] !== 'boolean' || typeof value['requested'] !== 'boolean') throw new Error('Native shortcut status is invalid')
  return { supported: value['supported'], pinned: value['pinned'], requested: value['requested'] }
}
export function parsePendingNewVoiceAction(value: unknown): { readonly token: string | null; readonly error: string | null; readonly mode: 'ask' | 'talk' } {
  if (!isRecord(value) || (value['token'] !== null && pendingNewVoiceToken(value) === null) || (value['error'] !== null && (typeof value['error'] !== 'string' || utf8ByteLength(value['error']) > 512))) throw new Error('Native shortcut launch is invalid')
  const mode = value['mode'] === undefined ? 'ask' : value['mode']
  if (mode !== 'ask' && mode !== 'talk') throw new Error('Native voice entry mode is invalid')
  return { token: value['token'] as string | null, error: value['error'] as string | null, mode }
}

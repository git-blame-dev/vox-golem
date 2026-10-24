import { TauriCommandError, getTauriInternals, isAndroidRuntime, isBrowserPreviewRuntime, normalizeTauriError } from './tauri'
import { parseAppSnapshot, parseCanonicalTurn, parseUnsavedResult } from './appSnapshot'
import { isRecord, parseRuntimePhase, utf8ByteLength } from './wire'
import { MAX_ANSWER_SOURCES, isSourceLink } from './answerSources'
import type {
  PromptExecutionEvent,
  PromptExecutionResult,
} from '../types/chat'
import type { AnswerSource } from '../components/AnswerStage'

function parseCustomDirectTimings(payload: Record<string, unknown>): import('../types/chat').CustomDirectTimings {
  const firstTextMs = payload['first_text_ms']
  const completedMs = payload['completed_ms']
  if (
    !Number.isSafeInteger(firstTextMs) ||
    !Number.isSafeInteger(completedMs) ||
    (firstTextMs as number) < 0 ||
    (completedMs as number) < (firstTextMs as number)
  ) {
    throw new Error('Custom direct timings must include ordered nonnegative milliseconds')
  }
  return { firstTextMs: firstTextMs as number, completedMs: completedMs as number }
}

function parseOptionalAnswerSources(sources: unknown): readonly AnswerSource[] {
  if (!Array.isArray(sources) || sources.length > MAX_ANSWER_SOURCES || !Array.from(sources).every(isSourceLink)) {
    throw new Error('Sources event must include bounded URL/title pairs')
  }
  return sources as AnswerSource[]
}

export function parseAndroidPromptUpdate(payload: unknown, requestId: string): PromptExecutionEvent {
  if (!isRecord(payload) || typeof payload['kind'] !== 'string') throw new Error('Android prompt update must include kind')
  const kind = payload['kind']
  if (kind === 'delta') {
    if (typeof payload['text'] !== 'string' || utf8ByteLength(payload['text']) > 256 * 1024) throw new Error('Android delta must include bounded text')
    return { requestId, kind: 'text', text: payload['text'] }
  }
  if (kind === 'custom_direct_timings') {
    return { requestId, kind: 'custom_direct_timings', timings: parseCustomDirectTimings(payload) }
  }
  if (kind === 'stage') {
    if (!isStage(payload['stage']) || !isStageStatus(payload['status']) || (payload['detail'] !== undefined && payload['detail'] !== null && (typeof payload['detail'] !== 'string' || utf8ByteLength(payload['detail']) > 512))) throw new Error('Android stage update is invalid')
    return { requestId, kind: 'stage', stage: payload['stage'], status: payload['status'], ...(typeof payload['detail'] === 'string' ? { detail: payload['detail'] } : {}) }
  }
  if (kind === 'correction') {
    if (!isStage(payload['stage']) || typeof payload['text'] !== 'string' || utf8ByteLength(payload['text']) > 256 * 1024 || typeof payload['correction'] !== 'string' || payload['correction'].trim() === '' || utf8ByteLength(payload['correction']) > 512) throw new Error('Android correction update is invalid')
    return { requestId, kind: 'correction', stage: payload['stage'], text: payload['text'], correction: payload['correction'] }
  }
  if (kind === 'status') {
    if (typeof payload['message'] !== 'string' || utf8ByteLength(payload['message']) > 512) throw new Error('Android status update is invalid')
    return { requestId, kind: 'status', message: payload['message'] }
  }
  throw new Error('Android prompt update kind is unsupported')
}

export function parseAndroidPromptOutcome(payload: unknown, requestId: string): PromptExecutionResult {
  if (requestId.length === 0 || utf8ByteLength(requestId) > 128) throw new Error('Prompt request ID must be bounded')
  if (!isRecord(payload) || !['completed', 'refused', 'cancelled', 'unsaved', 'error'].includes(payload['kind'] as string)) throw new Error('Canonical prompt outcome is invalid')
  if (payload['kind'] === 'error') {
    if (['turn', 'save_disposition', 'unsaved_result'].some((key) => Object.hasOwn(payload, key))) throw new Error('Error prompt outcome must be content-free')
    if (payload['request_id'] !== requestId) throw new Error('Error prompt outcome request ID did not match the active request')
    const message = payload['error_message']
    if (typeof message !== 'string' || message.trim().length === 0 || utf8ByteLength(message) > 4096) throw new Error('Error prompt outcome must contain a bounded error message')
    return { requestId, runtimePhase: parseRuntimePhase(payload['runtime_phase'], 'Prompt runtime_phase'), outcome: 'error', errorMessage: message }
  }
  if (payload['kind'] === 'cancelled') {
    if (['turn', 'save_disposition', 'unsaved_result'].some((key) => Object.hasOwn(payload, key))) throw new Error('Cancelled prompt outcome must be content-free')
    return { requestId, runtimePhase: 'sleeping', outcome: 'cancelled', errorMessage: null, canonicalOutcome: 'cancelled' }
  }
  if (payload['kind'] === 'unsaved') {
    if (payload['save_disposition'] !== 'unsaved' || Object.hasOwn(payload, 'turn')) throw new Error('Unsaved outcome must have unsaved disposition and no turn')
    const unsavedResult = parseUnsavedResult(payload['unsaved_result'])
    if (unsavedResult.requestId !== requestId) throw new Error('Unsaved outcome request ID did not match the active request')
    return { requestId, runtimePhase: 'sleeping', outcome: 'completed', errorMessage: null, canonicalOutcome: 'unsaved', saveDisposition: 'unsaved', unsavedResult }
  }
  if (payload['save_disposition'] !== 'saved' || Object.hasOwn(payload, 'unsaved_result')) throw new Error('Saved outcome must have saved disposition and no unsaved result')
  const turn = parseCanonicalTurn(payload['turn'])
  if (turn.requestId !== requestId) throw new Error('Android prompt outcome request ID did not match the active request')
  if (payload['kind'] === 'completed' && turn.answer.content.kind !== 'text') throw new Error('Completed Android prompt outcome must contain a text answer')
  if (payload['kind'] === 'refused' && turn.answer.content.kind !== 'refusal') throw new Error('Refused Android prompt outcome must contain a refusal answer')
  const firstTextMs = payload['first_text_ms']
  const completedMs = payload['completed_ms']
  let customDirectTimings: import('../types/chat').CustomDirectTimings | undefined
  if (
    Number.isSafeInteger(firstTextMs) &&
    Number.isSafeInteger(completedMs) &&
    (firstTextMs as number) >= 0 &&
    (completedMs as number) >= (firstTextMs as number)
  ) {
    customDirectTimings = { firstTextMs: firstTextMs as number, completedMs: completedMs as number }
  }
  let terminalSnapshot: import('./appSnapshot').AppSnapshot | undefined
  if (payload['app_snapshot'] !== undefined && payload['app_snapshot'] !== null) {
    try { terminalSnapshot = parseAppSnapshot(payload['app_snapshot']) } catch {
      // Optional projection evidence cannot invalidate an otherwise committed turn.
    }
  }
  return { requestId, runtimePhase: 'sleeping', outcome: 'completed', errorMessage: null, canonicalOutcome: payload['kind'] as 'completed' | 'refused', saveDisposition: 'saved', canonicalTurn: turn, ...(terminalSnapshot === undefined ? {} : { terminalSnapshot }), ...(customDirectTimings === undefined ? {} : { customDirectTimings }) }
}

export const parseCanonicalPromptOutcome = parseAndroidPromptOutcome

export function parseAndroidPromptOutcomeForExecution(payload: unknown, requestId: string): PromptExecutionResult {
  try { return parseAndroidPromptOutcome(payload, requestId) } catch (error) { throw normalizeTauriError(error) }
}

export function missingPromptStreamingSupportError(android: boolean): Error {
  return android
    ? new TauriCommandError('internal', 'Prompt execution requires Channel support')
    : new Error('Prompt execution requires Channel support')
}

export function parsePromptExecutionEvent(payload: unknown): PromptExecutionEvent {
  if (!isRecord(payload)) {
    throw new Error('Prompt execution event must be an object')
  }

  const requestId = payload['request_id']
  const kind = payload['kind']
  if (typeof requestId !== 'string' || requestId.length === 0 || utf8ByteLength(requestId) > 128 || typeof kind !== 'string') {
    throw new Error('Prompt execution event must include request_id and kind')
  }
  for (const key of ['text', 'correction', 'message', 'detail', 'tool']) {
    const value = payload[key]
    if (typeof value === 'string' && utf8ByteLength(value) > (key === 'text' ? 256 * 1024 : 4096)) {
      throw new Error('Prompt execution event exceeds presentation budget')
    }
  }

  if (kind === 'text' || kind === 'reasoning') {
    const text = payload['text']
    if (typeof text !== 'string') {
      throw new Error(`${kind} event must include text`)
    }
    return { requestId, kind, text }
  }

  if (kind === 'custom_direct_timings') {
    return { requestId, kind, timings: parseCustomDirectTimings(payload) }
  }

  if (kind === 'correction') {
    const text = payload['text']
    const correction = payload['correction']
    const stage = payload['stage']
    if (
      !isStage(stage) ||
      typeof text !== 'string' ||
      text.trim().length === 0 ||
      typeof correction !== 'string' ||
      !correction.startsWith('Correction: ') ||
      correction.slice('Correction: '.length).trim().length === 0
    ) {
      throw new Error('Correction event must include stage, text, and correction')
    }
    return { requestId, kind, stage, text, correction }
  }

  if (kind === 'stage') {
    const stage = payload['stage']
    const status = payload['status']
    const detail = payload['detail']
    if (!isStage(stage) || !isStageStatus(status) || (detail !== undefined && detail !== null && typeof detail !== 'string')) throw new Error('Stage event must include typed stage and status')
    return { requestId, kind, stage, status, ...(typeof detail === 'string' ? { detail } : {}) }
  }
  if (kind === 'sources') {
    return { requestId, kind, sources: parseOptionalAnswerSources(payload['sources']) }
  }

  if (kind === 'status') {
    const message = payload['message']
    if (typeof message !== 'string') {
      throw new Error('Status event must include message')
    }
    return { requestId, kind, message }
  }

  if (kind === 'tool') {
    const tool = payload['tool']
    const status = payload['status']
    const detail = payload['detail']
    if (typeof tool !== 'string' || !isToolStatus(status) || typeof detail !== 'string') {
      throw new Error('Tool event must include tool, status, and detail')
    }
    return { requestId, kind, tool, status, detail }
  }

  if (kind === 'error') {
    const message = payload['message']
    if (typeof message !== 'string') {
      throw new Error('Error event must include message')
    }
    return { requestId, kind, message }
  }

  if (kind === 'completed' || kind === 'cancelled') {
    return {
      requestId,
      kind,
      runtimePhase: parseRuntimePhase(payload['runtime_phase'], 'Prompt runtime_phase'),
    }
  }

  throw new Error('Prompt execution event contains an unsupported kind')
}

export async function executePrompt(
  requestId: string,
  prompt: string,
  onEvent: (event: PromptExecutionEvent) => void,
  source: 'typed' | 'voice' = 'typed',
): Promise<PromptExecutionResult> {
  const tauri = typeof window === 'undefined' ? null : getTauriInternals()
  if (tauri === null) throw new Error('Prompt execution requires a runtime adapter')
  const canonicalRuntime = isAndroidRuntime || isBrowserPreviewRuntime()

  if (tauri.createChannel !== undefined) {
    const updates = tauri.createChannel<unknown>((payload) => {
      try {
        if (canonicalRuntime) {
          onEvent(parseAndroidPromptUpdate(payload, requestId))
          return
        }
        if (!isRecord(payload) || 'request_id' in payload) return
        const parsed = parsePromptExecutionEvent({ ...payload, request_id: requestId })
        if (parsed.kind !== 'completed' && parsed.kind !== 'cancelled') onEvent(parsed)
      } catch {
        // Ignore malformed channel updates; the correlated command result remains authoritative.
      }
    })

    let payload: unknown
    try {
      payload = await tauri.invoke('submit_prompt', isAndroidRuntime ? { requestId, prompt, updates } : { requestId, prompt, source, updates })
    } catch (error) {
      if (isAndroidRuntime) throw normalizeTauriError(error)
      throw error
    }
    let result: PromptExecutionResult
    if (canonicalRuntime) {
      result = parseAndroidPromptOutcomeForExecution(payload, requestId)
    } else {
      result = parseCanonicalPromptOutcome(payload, requestId)
    }
    if (result.requestId !== requestId) {
      throw new Error('Prompt response request ID did not match the active request')
    }
    return result
  }

  throw missingPromptStreamingSupportError(isAndroidRuntime)
}

function isToolStatus(value: unknown): value is 'pending' | 'running' | 'completed' | 'error' {
  return value === 'pending' || value === 'running' || value === 'completed' || value === 'error'
}
function isStage(value: unknown): value is 'instant' | 'deep' | 'review' { return value === 'instant' || value === 'deep' || value === 'review' }
function isStageStatus(value: unknown): value is import('../components/AnswerStage').AnswerStageStatus { return ['queued', 'running', 'completed', 'kept', 'corrected', 'cancelled', 'failed', 'stale'].includes(value as string) }

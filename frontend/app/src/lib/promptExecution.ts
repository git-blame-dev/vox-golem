import { TauriCommandError, getTauriInternals, isAndroidRuntime, isBrowserPreviewRuntime, normalizeTauriError } from './tauri'
import { parseCanonicalTurn } from './appSnapshot'
import { isRecord, parseRuntimePhase, utf8ByteLength } from './wire'
import type {
  PromptExecutionEvent,
  PromptExecutionResult,
} from '../types/chat'
import type { AnswerSource } from '../components/AnswerStage'
import type { SearchMode } from './searchMode'

export function parseAndroidPromptUpdate(payload: unknown, requestId: string): PromptExecutionEvent {
  if (!isRecord(payload) || typeof payload['kind'] !== 'string') throw new Error('Android prompt update must include kind')
  const kind = payload['kind']
  if (kind === 'delta') {
    if (typeof payload['text'] !== 'string' || utf8ByteLength(payload['text']) > 256 * 1024) throw new Error('Android delta must include bounded text')
    return { requestId, kind: 'text', text: payload['text'] }
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
  if (!isRecord(payload) || (payload['kind'] !== 'completed' && payload['kind'] !== 'refused' && payload['kind'] !== 'cancelled')) throw new Error('Android prompt outcome is invalid')
  if (payload['kind'] === 'cancelled') {
    if (Object.prototype.hasOwnProperty.call(payload, 'turn')) throw new Error('Cancelled Android prompt outcome must not include a turn')
    return { requestId, runtimePhase: 'sleeping', outcome: 'cancelled', errorMessage: null, canonicalOutcome: 'cancelled' }
  }
  const turn = parseCanonicalTurn(payload['turn'], true)
  if (turn.requestId !== requestId) throw new Error('Android prompt outcome request ID did not match the active request')
  if (payload['kind'] === 'completed' && turn.answer.content.kind !== 'text') throw new Error('Completed Android prompt outcome must contain a text answer')
  if (payload['kind'] === 'refused' && turn.answer.content.kind !== 'refusal') throw new Error('Refused Android prompt outcome must contain a refusal answer')
  return { requestId, runtimePhase: 'sleeping', outcome: 'completed', errorMessage: null, canonicalOutcome: payload['kind'], canonicalTurn: turn }
}

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
  if (typeof requestId !== 'string' || typeof kind !== 'string') {
    throw new Error('Prompt execution event must include request_id and kind')
  }

  if (kind === 'text' || kind === 'reasoning') {
    const text = payload['text']
    if (typeof text !== 'string') {
      throw new Error(`${kind} event must include text`)
    }
    return { requestId, kind, text }
  }

  if (kind === 'custom_direct_timings') {
    const firstTextMs = payload['first_text_ms']
    const completedMs = payload['completed_ms']
    if (!Number.isSafeInteger(firstTextMs) || !Number.isSafeInteger(completedMs) ||
        (firstTextMs as number) < 0 || (completedMs as number) < (firstTextMs as number)) {
      throw new Error('Custom direct timings must include ordered nonnegative milliseconds')
    }
    return { requestId, kind, timings: { firstTextMs: firstTextMs as number, completedMs: completedMs as number } }
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
    const sources = payload['sources']
    if (!Array.isArray(sources) || sources.length > 32 || !sources.every(isSource)) throw new Error('Sources event must include bounded URL/title pairs')
    return { requestId, kind, sources: sources as AnswerSource[] }
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

export function parsePromptExecutionResult(payload: unknown): PromptExecutionResult {
  if (!isRecord(payload)) {
    throw new Error('Prompt execution result must be an object')
  }
  const requestId = payload['request_id']
  const outcome = payload['outcome']
  const errorMessage = payload['error_message']
  if (
    typeof requestId !== 'string' ||
    (outcome !== 'completed' && outcome !== 'cancelled' && outcome !== 'error') ||
    (errorMessage !== undefined && errorMessage !== null && typeof errorMessage !== 'string')
  ) {
    throw new Error('Prompt execution result must include request_id and outcome')
  }
  return {
    requestId,
    runtimePhase: parseRuntimePhase(payload['runtime_phase'], 'Prompt runtime_phase'),
    outcome,
    errorMessage: typeof errorMessage === 'string' ? errorMessage : null,
  }
}

export async function executePrompt(
  requestId: string,
  prompt: string,
  onEvent: (event: PromptExecutionEvent) => void,
  source: 'typed' | 'voice' = 'typed',
  searchMode: SearchMode = 'quick',
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
      payload = await tauri.invoke('submit_prompt', isAndroidRuntime ? { requestId, prompt, searchMode, updates } : { requestId, prompt, source, searchMode, updates })
    } catch (error) {
      if (isAndroidRuntime) throw normalizeTauriError(error)
      throw error
    }
    let result: PromptExecutionResult
    if (canonicalRuntime) {
      result = parseAndroidPromptOutcomeForExecution(payload, requestId)
    } else {
      result = parsePromptExecutionResult(payload)
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
function isSource(value: unknown): value is AnswerSource {
  if (!isRecord(value) || typeof value['url'] !== 'string' || typeof value['title'] !== 'string' || value['title'].trim() === '' || utf8ByteLength(value['title']) > 512) return false
  try { const url = new URL(value['url']); return (url.protocol === 'http:' || url.protocol === 'https:') && value['url'].length <= 2048 } catch { return false }
}

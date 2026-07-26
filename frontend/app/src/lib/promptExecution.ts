import { getTauriInternals } from './tauri'
import type {
  BackendRuntimePhase,
  PromptExecutionEvent,
  PromptExecutionResult,
} from '../types/chat'
import type { AnswerSource } from '../components/AnswerStage'

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
    if (!isStage(stage) || !isStageStatus(status) || (detail !== undefined && typeof detail !== 'string')) throw new Error('Stage event must include typed stage and status')
    return { requestId, kind, stage, status, ...(detail === undefined ? {} : { detail }) }
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
      runtimePhase: parseRuntimePhase(payload['runtime_phase']),
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
    runtimePhase: parseRuntimePhase(payload['runtime_phase']),
    outcome,
    errorMessage: typeof errorMessage === 'string' ? errorMessage : null,
  }
}

export async function executePrompt(
  requestId: string,
  prompt: string,
  onEvent: (event: PromptExecutionEvent) => void,
  source: 'typed' | 'voice' = 'typed',
): Promise<PromptExecutionResult> {
  const tauri = typeof window === 'undefined' ? null : getTauriInternals()
  if (tauri === null) {
    onEvent({
      requestId,
      kind: 'text',
      text: `Browser preview only — no backend response was generated. Prompt: ${prompt}`,
    })
    return {
      requestId,
      runtimePhase: 'sleeping',
      outcome: 'completed',
      errorMessage: null,
    }
  }

  if (tauri.listen === undefined) {
    throw new Error('Prompt execution requires streaming listener support')
  }

  const unlisten = await tauri.listen('prompt-execution-event', (event) => {
    try {
      const parsed = parsePromptExecutionEvent(event.payload)
      if (parsed.requestId === requestId) {
        onEvent(parsed)
      }
    } catch {
      // Ignore malformed external events; the correlated command result remains authoritative.
    }
  })

  try {
    const payload = await tauri.invoke('submit_prompt', { requestId, prompt, source })
    const result = parsePromptExecutionResult(payload)
    if (result.requestId !== requestId) {
      throw new Error('Prompt response request ID did not match the active request')
    }
    return result
  } finally {
    unlisten()
  }
}

function isToolStatus(value: unknown): value is 'pending' | 'running' | 'completed' | 'error' {
  return value === 'pending' || value === 'running' || value === 'completed' || value === 'error'
}
function isStage(value: unknown): value is 'instant' | 'deep' | 'review' { return value === 'instant' || value === 'deep' || value === 'review' }
function isStageStatus(value: unknown): value is import('../components/AnswerStage').AnswerStageStatus { return ['queued', 'running', 'completed', 'kept', 'corrected', 'cancelled', 'failed', 'stale'].includes(value as string) }
function isSource(value: unknown): value is AnswerSource {
  if (!isRecord(value) || typeof value['url'] !== 'string' || typeof value['title'] !== 'string' || value['title'].trim() === '' || new TextEncoder().encode(value['title']).length > 512) return false
  try { const url = new URL(value['url']); return (url.protocol === 'http:' || url.protocol === 'https:') && value['url'].length <= 2048 } catch { return false }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null
}

function parseRuntimePhase(payload: unknown): BackendRuntimePhase {
  if (
    payload === 'initializing' ||
    payload === 'sleeping' ||
    payload === 'listening' ||
    payload === 'processing' ||
    payload === 'executing' ||
    payload === 'error'
  ) {
    return payload
  }
  throw new Error('Prompt payload must include a supported runtime phase')
}

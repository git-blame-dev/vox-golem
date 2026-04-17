import { getTauriInternals } from './tauri'
import type { PromptExecutionEvent, PromptExecutionResult } from '../types/chat'

export function parsePromptExecutionResult(payload: unknown): PromptExecutionResult {
  if (!isRecord(payload)) {
    throw new Error('Prompt execution payload must be an object')
  }

  const events = payload['events']
  const stderr = payload['stderr']
  const exitCode = payload['exit_code']

  if (!Array.isArray(events)) {
    throw new Error('Prompt execution payload must include events')
  }

  if (typeof stderr !== 'string') {
    throw new Error('Prompt execution payload must include stderr')
  }

  if (typeof exitCode !== 'number' && exitCode !== null) {
    throw new Error('Prompt execution payload must include a numeric or null exit code')
  }

  return {
    events: events.map(parsePromptExecutionEvent),
    stderr,
    exitCode,
  }
}

export async function executePrompt(prompt: string): Promise<PromptExecutionResult> {
  if (typeof window === 'undefined') {
    return createFallbackResult(prompt)
  }

  const tauriInternals = getTauriInternals()

  if (tauriInternals === null) {
    return createFallbackResult(prompt)
  }

  const payload = await tauriInternals.invoke('submit_prompt', { prompt })
  return parsePromptExecutionResult(payload)
}

function createFallbackResult(prompt: string): PromptExecutionResult {
  return {
    events: [
      {
        kind: 'text',
        text: `Placeholder response for: ${prompt}`,
      },
    ],
    stderr: '',
    exitCode: 0,
  }
}

function parsePromptExecutionEvent(payload: unknown): PromptExecutionEvent {
  if (!isRecord(payload)) {
    throw new Error('Prompt execution event must be an object')
  }

  if (payload['kind'] === 'text') {
    const text = payload['text']

    if (typeof text !== 'string') {
      throw new Error('Text event must include text')
    }

    return {
      kind: 'text',
      text,
    }
  }

  if (payload['kind'] === 'error') {
    const name = payload['name']
    const message = payload['message']

    if (typeof name !== 'string' || typeof message !== 'string') {
      throw new Error('Error event must include name and message')
    }

    return {
      kind: 'error',
      name,
      message,
    }
  }

  if (payload['kind'] === 'tool_use') {
    const tool = payload['tool']
    const status = payload['status']
    const detail = payload['detail']

    if (
      typeof tool !== 'string' ||
      (status !== 'completed' && status !== 'error') ||
      typeof detail !== 'string'
    ) {
      throw new Error('Tool-use event must include tool, status, and detail')
    }

    return {
      kind: 'tool_use',
      tool,
      status,
      detail,
    }
  }

  throw new Error('Prompt execution event contains an unsupported kind')
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null
}

import type { PromptExecutionResult } from '../types/chat'

interface TauriInternals {
  readonly invoke: (command: string, args?: unknown) => Promise<unknown>
}

declare global {
  interface Window {
    __TAURI_INTERNALS__?: TauriInternals
  }
}

export function parsePromptExecutionResult(payload: unknown): PromptExecutionResult {
  if (!isRecord(payload)) {
    throw new Error('Prompt execution payload must be an object')
  }

  const stdout = payload['stdout']
  const stderr = payload['stderr']
  const exitCode = payload['exit_code']

  if (typeof stdout !== 'string') {
    throw new Error('Prompt execution payload must include stdout')
  }

  if (typeof stderr !== 'string') {
    throw new Error('Prompt execution payload must include stderr')
  }

  if (typeof exitCode !== 'number' && exitCode !== null) {
    throw new Error('Prompt execution payload must include a numeric or null exit code')
  }

  return {
    stdout,
    stderr,
    exitCode,
  }
}

export async function executePrompt(prompt: string): Promise<PromptExecutionResult> {
  if (typeof window === 'undefined') {
    return createFallbackResult(prompt)
  }

  const tauriInternals = window.__TAURI_INTERNALS__

  if (!tauriInternals || typeof tauriInternals.invoke !== 'function') {
    return createFallbackResult(prompt)
  }

  const payload = await tauriInternals.invoke('submit_prompt', { prompt })
  return parsePromptExecutionResult(payload)
}

function createFallbackResult(prompt: string): PromptExecutionResult {
  return {
    stdout: `Placeholder response for: ${prompt}`,
    stderr: '',
    exitCode: 0,
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null
}

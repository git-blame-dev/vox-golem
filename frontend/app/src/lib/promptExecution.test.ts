import { afterEach, describe, expect, it } from 'vitest'
import {
  executePrompt,
  parsePromptExecutionEvent,
  parsePromptExecutionResult,
} from './promptExecution'
import type { PromptExecutionEvent } from '../types/chat'
import { getTauriInternals } from './tauri'

afterEach(() => {
  Reflect.deleteProperty(window, '__TAURI_INTERNALS__')
})

describe('prompt payload parsing', () => {
  it('parses a valid final result', () => {
    expect(
      parsePromptExecutionResult({
        request_id: 'request-1',
        runtime_phase: 'sleeping',
        outcome: 'completed',
      }),
    ).toEqual({
      requestId: 'request-1',
      runtimePhase: 'sleeping',
      outcome: 'completed',
      errorMessage: null,
    })
  })

  it('parses an authoritative final error message', () => {
    expect(
      parsePromptExecutionResult({
        request_id: 'request-1',
        runtime_phase: 'error',
        outcome: 'error',
        error_message: 'OpenCode provider authentication failed',
      }),
    ).toEqual({
      requestId: 'request-1',
      runtimePhase: 'error',
      outcome: 'error',
      errorMessage: 'OpenCode provider authentication failed',
    })
  })

  it('parses text, tool, and terminal events', () => {
    expect(
      parsePromptExecutionEvent({
        request_id: 'request-1',
        kind: 'text',
        text: 'Hello',
      }),
    ).toEqual({ requestId: 'request-1', kind: 'text', text: 'Hello' })
    expect(
      parsePromptExecutionEvent({
        request_id: 'request-1',
        kind: 'tool',
        tool: 'bash',
        status: 'running',
        detail: 'Checking status',
      }),
    ).toEqual({
      requestId: 'request-1',
      kind: 'tool',
      tool: 'bash',
      status: 'running',
      detail: 'Checking status',
    })
    expect(
      parsePromptExecutionEvent({
        request_id: 'request-1',
        kind: 'cancelled',
        runtime_phase: 'sleeping',
      }),
    ).toEqual({
      requestId: 'request-1',
      kind: 'cancelled',
      runtimePhase: 'sleeping',
    })
  })

  it('rejects invalid result and event payloads', () => {
    expect(() => parsePromptExecutionResult({ outcome: 'completed' })).toThrow()
    expect(() =>
      parsePromptExecutionEvent({
        request_id: 'request-1',
        kind: 'tool',
        tool: 'bash',
        status: 'unknown',
        detail: '',
      }),
    ).toThrow()
  })
})

describe('executePrompt', () => {
  it('uses API wrappers for native-shaped Tauri internals', () => {
    window.__TAURI_INTERNALS__ = Object.assign(
      { invoke: async () => null },
      { transformCallback: () => 1 },
    )

    expect(getTauriInternals()?.listen).toBeTypeOf('function')
  })

  it('emits browser fallback text when Tauri is unavailable', async () => {
    const events: PromptExecutionEvent[] = []

    await expect(
      executePrompt('request-1', 'Draft release notes', (event) => events.push(event)),
    ).resolves.toEqual({
      requestId: 'request-1',
      runtimePhase: 'sleeping',
      outcome: 'completed',
      errorMessage: null,
    })
    expect(events).toEqual([
      {
        requestId: 'request-1',
        kind: 'text',
        text: 'Browser preview only — no backend response was generated. Prompt: Draft release notes',
      },
    ])
  })

  it('subscribes before invoking and forwards only correlated events', async () => {
    const order: string[] = []
    const events: PromptExecutionEvent[] = []
    let eventHandler: ((event: { payload: unknown }) => void) | undefined
    let unlistened = false
    window.__TAURI_INTERNALS__ = {
      listen: async (event, handler) => {
        expect(event).toBe('prompt-execution-event')
        order.push('listen')
        eventHandler = handler
        return () => {
          unlistened = true
        }
      },
      invoke: async (command, args) => {
        order.push('invoke')
        expect(command).toBe('submit_prompt')
        expect(args).toEqual({ requestId: 'request-1', prompt: 'Draft release notes' })
        eventHandler?.({
          payload: { request_id: 'stale', kind: 'text', text: 'Ignore me' },
        })
        eventHandler?.({
          payload: { request_id: 'request-1', kind: 'text', text: 'OpenCode response' },
        })
        return {
          request_id: 'request-1',
          runtime_phase: 'sleeping',
          outcome: 'completed',
        }
      },
    }

    await expect(
      executePrompt('request-1', 'Draft release notes', (event) => events.push(event)),
    ).resolves.toEqual({
      requestId: 'request-1',
      runtimePhase: 'sleeping',
      outcome: 'completed',
      errorMessage: null,
    })
    expect(order).toEqual(['listen', 'invoke'])
    expect(events).toEqual([
      { requestId: 'request-1', kind: 'text', text: 'OpenCode response' },
    ])
    expect(unlistened).toBe(true)
  })

  it('requires streaming listener support', async () => {
    window.__TAURI_INTERNALS__ = { invoke: async () => null }

    await expect(executePrompt('request-1', 'Draft release notes', () => undefined)).rejects.toThrow(
      'Prompt execution requires streaming listener support',
    )
  })

  it('rejects a final result correlated to another request', async () => {
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async () => ({
        request_id: 'request-other',
        runtime_phase: 'sleeping',
        outcome: 'completed',
        error_message: null,
      }),
    }

    await expect(executePrompt('request-1', 'Hello', () => undefined)).rejects.toThrow(
      'Prompt response request ID did not match the active request',
    )
  })
})

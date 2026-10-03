import { afterEach, describe, expect, it } from 'vitest'
import {
  executePrompt,
  parsePromptExecutionEvent,
  parsePromptExecutionResult,
} from './promptExecution'
import type { PromptExecutionEvent } from '../types/chat'
import { getTauriInternals } from './tauri'
import { resetBrowserPreviewRuntime } from './browserRuntime'

afterEach(() => {
  Reflect.deleteProperty(window, '__TAURI_INTERNALS__')
  resetBrowserPreviewRuntime()
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

  it('accepts measured Custom direct timings and rejects fabricated or reversed values', () => {
    expect(parsePromptExecutionEvent({
      request_id: 'request-1', kind: 'custom_direct_timings', first_text_ms: 405, completed_ms: 1875,
    })).toEqual({
      requestId: 'request-1', kind: 'custom_direct_timings', timings: { firstTextMs: 405, completedMs: 1875 },
    })
    for (const [first_text_ms, completed_ms] of [[-1, 20], [30, 20], [1.5, 20], [0, Number.MAX_SAFE_INTEGER + 1], [undefined, 20]]) {
      expect(() => parsePromptExecutionEvent({ request_id: 'request-1', kind: 'custom_direct_timings', first_text_ms, completed_ms })).toThrow('Custom direct timings')
    }
  })

  it('parses strict correction events and rejects malformed corrections', () => {
    expect(parsePromptExecutionEvent({
      request_id: 'request-1',
      kind: 'correction',
      stage: 'deep',
      text: 'Corrected answer',
      correction: 'Correction: Use the verified value.',
    })).toEqual({
      requestId: 'request-1',
      kind: 'correction',
      stage: 'deep',
      text: 'Corrected answer',
      correction: 'Correction: Use the verified value.',
    })
    expect(() => parsePromptExecutionEvent({
      request_id: 'request-1',
      kind: 'correction',
      stage: 'other',
      text: '',
      correction: 'not a correction',
    })).toThrow('Correction event must include stage')
    expect(() => parsePromptExecutionEvent({
      request_id: 'request-1',
      kind: 'correction',
      text: 'Corrected answer',
      correction: 'Correction: Use the verified value.',
    })).toThrow('Correction event must include stage')
    expect(() => parsePromptExecutionEvent({
      request_id: 'request-1',
      kind: 'correction',
      text: 'Corrected answer',
      correction: 'Correction: ',
    })).toThrow('Correction event must include stage')
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

  it('validates source title byte boundaries and complete source batches', () => {
    const title = (value: string) => ({ url: `https://example.com/${value.length}`, title: value })
    const ascii = (length: number) => 'a'.repeat(length)
    const unicode = '界'.repeat(170)
    const unicode512 = '😀'.repeat(128)

    for (const length of [256, 257]) {
      expect(parsePromptExecutionEvent({ request_id: 'request-1', kind: 'sources', sources: [title(ascii(length))] })).toMatchObject({ kind: 'sources' })
    }
    expect(parsePromptExecutionEvent({ request_id: 'request-1', kind: 'sources', sources: [title(ascii(512))] })).toMatchObject({ kind: 'sources' })
    expect(parsePromptExecutionEvent({ request_id: 'request-1', kind: 'sources', sources: [title(unicode)] })).toMatchObject({ kind: 'sources' })
    expect(parsePromptExecutionEvent({ request_id: 'request-1', kind: 'sources', sources: [title(unicode512)] })).toMatchObject({ kind: 'sources' })
    expect(() => parsePromptExecutionEvent({ request_id: 'request-1', kind: 'sources', sources: [title('界'.repeat(171))] })).toThrow('Sources event')
    expect(parsePromptExecutionEvent({ request_id: 'request-1', kind: 'sources', sources: [title(ascii(256)), title(ascii(512))] })).toMatchObject({ kind: 'sources' })
    expect(() => parsePromptExecutionEvent({ request_id: 'request-1', kind: 'sources', sources: [title(ascii(256)), title('界'.repeat(171))] })).toThrow('Sources event')
  })
})

describe('executePrompt', () => {
  it('uses API wrappers for native-shaped Tauri internals', () => {
    window.__TAURI_INTERNALS__ = Object.assign(
      { invoke: async () => null },
      { transformCallback: () => 1 },
    )

    expect(getTauriInternals()?.listen).toBeTypeOf('function')
    expect(getTauriInternals()?.createChannel).toBeTypeOf('function')
  })

  it('streams a deterministic response through the browser preview adapter', async () => {
    const events: PromptExecutionEvent[] = []

    await expect(
      executePrompt('request-1', 'Draft release notes', (event) => events.push(event)),
    ).resolves.toMatchObject({
      requestId: 'request-1',
      runtimePhase: 'sleeping',
      outcome: 'completed',
      errorMessage: null,
      canonicalOutcome: 'completed',
      canonicalTurn: {
        requestId: 'request-1',
        executionId: { sessionId: '1', generation: '1' },
        answer: { content: { kind: 'text' } },
      },
    })
    expect(events).toEqual([
      {
        requestId: 'request-1',
        kind: 'text',
        text: 'Browser preview only: no backend response was generated. Prompt: Draft release notes',
      },
    ])
  })

  it('creates isolated Channels before invoking and rejects request-tagged global updates', async () => {
    const order: string[] = []
    const events: PromptExecutionEvent[] = []
    let send: ((payload: unknown) => void) | undefined
    window.__TAURI_INTERNALS__ = {
      createChannel: <T,>(handler: (payload: T) => void) => {
        order.push('channel')
        send = handler as (payload: unknown) => void
        return 'channel'
      },
      listen: async () => { throw new Error('Global prompt subscriptions are obsolete') },
      invoke: async (command, args) => {
        order.push('invoke')
        expect(command).toBe('submit_prompt')
        expect(args).toEqual({ requestId: 'request-1', prompt: 'Draft release notes', source: 'typed', updates: 'channel' })
        send?.({ request_id: 'stale', kind: 'text', text: 'Ignore me' })
        send?.({ kind: 'text', text: 'OpenCode response' })
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
    expect(order).toEqual(['channel', 'invoke'])
    expect(events).toEqual([
      { requestId: 'request-1', kind: 'text', text: 'OpenCode response' },
    ])
  })

  it('creates a Channel before invoking and adapts updates without request IDs', async () => {
    const order: string[] = []
    const events: PromptExecutionEvent[] = []
    let update: ((payload: unknown) => void) | undefined
    window.__TAURI_INTERNALS__ = {
      createChannel: <T>(handler: (message: T) => void) => {
        order.push('channel')
        update = handler as (payload: unknown) => void
        return 'channel'
      },
      listen: async () => { throw new Error('global listener must not be used') },
      invoke: async (command, args) => {
        order.push('invoke')
        expect(command).toBe('submit_prompt')
        expect(args).toMatchObject({ requestId: 'request-1', prompt: 'Draft release notes', source: 'typed', updates: 'channel' })
        update?.({ kind: 'text', text: 'OpenCode response' })
        update?.({ kind: 'text' })
        return { request_id: 'request-1', runtime_phase: 'sleeping', outcome: 'completed' }
      },
    }

    await expect(executePrompt('request-1', 'Draft release notes', (event) => events.push(event))).resolves.toMatchObject({ requestId: 'request-1' })
    expect(order).toEqual(['channel', 'invoke'])
    expect(events).toEqual([{ requestId: 'request-1', kind: 'text', text: 'OpenCode response' }])
  })

  it('preserves desktop Channel rejection details', async () => {
    window.__TAURI_INTERNALS__ = {
      createChannel: () => 'channel',
      invoke: async () => { throw 'provider unavailable' },
    }

    await expect(executePrompt('request-1', 'Draft release notes', () => undefined)).rejects.toBe('provider unavailable')
  })

  it('keeps concurrent Channel updates scoped to their own commands including nullable desktop stage details', async () => {
    const channels: Array<(payload: unknown) => void> = []
    const settle = new Map<string, (payload: unknown) => void>()
    const firstEvents: PromptExecutionEvent[] = []
    const secondEvents: PromptExecutionEvent[] = []
    window.__TAURI_INTERNALS__ = {
      createChannel: <T,>(handler: (payload: T) => void) => {
        channels.push(handler as (payload: unknown) => void)
        return channels.length
      },
      invoke: async (_command, args) => new Promise((resolve) => {
        settle.set((args as { requestId: string }).requestId, resolve)
      }),
    }
    const first = executePrompt('first', 'First question', (event) => firstEvents.push(event))
    const second = executePrompt('second', 'Second question', (event) => secondEvents.push(event))
    channels[1]?.({ kind: 'stage', stage: 'instant', status: 'running', detail: null })
    channels[0]?.({ kind: 'text', text: 'First answer' })
    channels[1]?.({ kind: 'text', text: 'Second answer' })
    settle.get('second')?.({ request_id: 'second', runtime_phase: 'sleeping', outcome: 'completed' })
    settle.get('first')?.({ request_id: 'first', runtime_phase: 'sleeping', outcome: 'completed' })
    await Promise.all([first, second])
    expect(firstEvents).toEqual([{ requestId: 'first', kind: 'text', text: 'First answer' }])
    expect(secondEvents).toEqual([
      { requestId: 'second', kind: 'stage', stage: 'instant', status: 'running' },
      { requestId: 'second', kind: 'text', text: 'Second answer' },
    ])
  })

  it('requires Channel support even when legacy global listeners exist', async () => {
    window.__TAURI_INTERNALS__ = { invoke: async () => null, listen: async () => { throw new Error('Do not subscribe') } }

    await expect(executePrompt('request-1', 'Draft release notes', () => undefined)).rejects.toThrow(
      'Prompt execution requires Channel support',
    )
  })

  it('rejects a final result correlated to another request', async () => {
    window.__TAURI_INTERNALS__ = {
      createChannel: () => 'channel',
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

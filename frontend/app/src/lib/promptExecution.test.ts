import { afterEach, describe, expect, it } from 'vitest'
import { executePrompt, parsePromptExecutionResult } from './promptExecution'

afterEach(() => {
  Reflect.deleteProperty(window, '__TAURI_INTERNALS__')
})

describe('parsePromptExecutionResult', () => {
  it('parses a valid execution payload', () => {
    expect(
      parsePromptExecutionResult({
        events: [
          { kind: 'text', text: 'done' },
          { kind: 'reasoning', text: 'Need to inspect the repo state first' },
          { kind: 'step_start' },
          { kind: 'step_finish', reason: 'stop' },
          { kind: 'tool_use', tool: 'bash', status: 'completed', detail: 'Shows status' },
        ],
        stderr: '',
        exit_code: 0,
        runtime_phase: 'sleeping',
      }),
    ).toEqual({
      events: [
        { kind: 'text', text: 'done' },
        { kind: 'reasoning', text: 'Need to inspect the repo state first' },
        { kind: 'step_start' },
        { kind: 'step_finish', reason: 'stop' },
        { kind: 'tool_use', tool: 'bash', status: 'completed', detail: 'Shows status' },
      ],
      stderr: '',
      exitCode: 0,
      runtimePhase: 'sleeping',
    })
  })

  it('parses a valid local-backend execution payload with null exit code', () => {
    expect(
      parsePromptExecutionResult({
        events: [{ kind: 'text', text: 'Local Gemma response' }],
        stderr: '',
        exit_code: null,
        runtime_phase: 'sleeping',
      }),
    ).toEqual({
      events: [{ kind: 'text', text: 'Local Gemma response' }],
      stderr: '',
      exitCode: null,
      runtimePhase: 'sleeping',
    })
  })

  it('throws for invalid payload shape', () => {
    expect(() => parsePromptExecutionResult({ stdout: 'done' })).toThrow()
  })
})

describe('executePrompt', () => {
  it('uses fallback output when tauri internals are unavailable', async () => {
    await expect(executePrompt('Draft release notes')).resolves.toEqual({
      events: [{ kind: 'text', text: 'Placeholder response for: Draft release notes' }],
      stderr: '',
      exitCode: 0,
      runtimePhase: 'sleeping',
    })
  })

  it('uses tauri prompt execution when available', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        expect(command).toBe('submit_prompt')
        expect(args).toEqual({ prompt: 'Draft release notes' })

        return {
          events: [{ kind: 'text', text: 'OpenCode response' }],
          stderr: '',
          exit_code: 0,
          runtime_phase: 'sleeping',
        }
      },
    }

    await expect(executePrompt('Draft release notes')).resolves.toEqual({
      events: [{ kind: 'text', text: 'OpenCode response' }],
      stderr: '',
      exitCode: 0,
      runtimePhase: 'sleeping',
    })
  })

  it('uses tauri prompt execution when the local backend returns null exit code', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        expect(command).toBe('submit_prompt')
        expect(args).toEqual({ prompt: 'Draft release notes' })

        return {
          events: [{ kind: 'text', text: 'Local Gemma response' }],
          stderr: '',
          exit_code: null,
          runtime_phase: 'sleeping',
        }
      },
    }

    await expect(executePrompt('Draft release notes')).resolves.toEqual({
      events: [{ kind: 'text', text: 'Local Gemma response' }],
      stderr: '',
      exitCode: null,
      runtimePhase: 'sleeping',
    })
  })
})

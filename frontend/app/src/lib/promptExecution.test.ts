import { afterEach, describe, expect, it } from 'vitest'
import { executePrompt, parsePromptExecutionResult } from './promptExecution'

afterEach(() => {
  Reflect.deleteProperty(window, '__TAURI_INTERNALS__')
})

describe('parsePromptExecutionResult', () => {
  it('parses a valid execution payload', () => {
    expect(
      parsePromptExecutionResult({
        stdout: 'done',
        stderr: '',
        exit_code: 0,
      }),
    ).toEqual({
      stdout: 'done',
      stderr: '',
      exitCode: 0,
    })
  })

  it('throws for invalid payload shape', () => {
    expect(() => parsePromptExecutionResult({ stdout: 'done' })).toThrow()
  })
})

describe('executePrompt', () => {
  it('uses fallback output when tauri internals are unavailable', async () => {
    await expect(executePrompt('Draft release notes')).resolves.toEqual({
      stdout: 'Placeholder response for: Draft release notes',
      stderr: '',
      exitCode: 0,
    })
  })

  it('uses tauri prompt execution when available', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        expect(command).toBe('submit_prompt')
        expect(args).toEqual({ prompt: 'Draft release notes' })

        return {
          stdout: 'OpenCode response',
          stderr: '',
          exit_code: 0,
        }
      },
    }

    await expect(executePrompt('Draft release notes')).resolves.toEqual({
      stdout: 'OpenCode response',
      stderr: '',
      exitCode: 0,
    })
  })
})

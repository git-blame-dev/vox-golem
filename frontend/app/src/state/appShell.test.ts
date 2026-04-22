import { describe, expect, it } from 'vitest'
import { createExecutionMessages } from './appShell'

describe('createExecutionMessages', () => {
  it('creates assistant and system messages for stdout and stderr output', () => {
    expect(
      createExecutionMessages({
        events: [
          { kind: 'step_start' },
          { kind: 'reasoning', text: 'Need to inspect the repo state first' },
          { kind: 'text', text: 'OpenCode response' },
          { kind: 'step_finish', reason: 'stop' },
        ],
        stderr: 'warning output',
        exitCode: 0,
        runtimePhase: 'sleeping',
      }).map((message) => message.content),
    ).toEqual([
      'step_start:\nOpenCode started a run step.',
      'reasoning:\nNeed to inspect the repo state first',
      'OpenCode response',
      'step_finish:\nstop',
      'stderr:\nwarning output',
    ])
  })

  it('includes a non-zero exit code label', () => {
    expect(
      createExecutionMessages({
        events: [],
        stderr: 'bad prompt',
        exitCode: 7,
        runtimePhase: 'error',
      }).map((message) => message.content),
    ).toEqual(['stderr:\nbad prompt', 'exit_code:\n7'])
  })

  it('creates a system message for structured opencode errors', () => {
    expect(
      createExecutionMessages({
        events: [
          {
            kind: 'error',
            name: 'APIError',
            message: 'Provider failed',
          },
        ],
        stderr: '',
        exitCode: 0,
        runtimePhase: 'error',
      }).map((message) => message.content),
    ).toEqual(['opencode_error:\nAPIError: Provider failed'])
  })

  it('creates a system message for structured tool-use events', () => {
    expect(
      createExecutionMessages({
        events: [
          {
            kind: 'tool_use',
            tool: 'bash',
            status: 'completed',
            detail: 'Shows working tree status',
          },
        ],
        stderr: '',
        exitCode: 0,
        runtimePhase: 'sleeping',
      }).map((message) => message.content),
    ).toEqual(['tool_use:\nbash (completed)\nShows working tree status'])
  })

  it('returns fallback message when no output is produced', () => {
    expect(
      createExecutionMessages({
        events: [],
        stderr: '',
        exitCode: 0,
        runtimePhase: 'sleeping',
      }).map((message) => message.content),
    ).toEqual(['OpenCode returned no output.'])
  })

  it('renders step finish fallback text when no reason is provided', () => {
    expect(
      createExecutionMessages({
        events: [{ kind: 'step_finish', reason: null }],
        stderr: '',
        exitCode: 0,
        runtimePhase: 'sleeping',
      }).map((message) => message.content),
    ).toEqual(['step_finish:\nOpenCode finished a run step.'])
  })
})

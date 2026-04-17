import { describe, expect, it } from 'vitest'
import { createExecutionMessages } from './appShell'

describe('createExecutionMessages', () => {
  it('creates assistant and system messages for stdout and stderr output', () => {
    expect(
      createExecutionMessages({
        stdout: 'OpenCode response',
        stderr: 'warning output',
        exitCode: 0,
      }).map((message) => message.content),
    ).toEqual(['stdout:\nOpenCode response', 'stderr:\nwarning output'])
  })

  it('includes a non-zero exit code label', () => {
    expect(
      createExecutionMessages({
        stdout: '',
        stderr: 'bad prompt',
        exitCode: 7,
      }).map((message) => message.content),
    ).toEqual(['stderr:\nbad prompt', 'exit_code:\n7'])
  })

  it('returns fallback message when no output is produced', () => {
    expect(
      createExecutionMessages({
        stdout: '',
        stderr: '',
        exitCode: 0,
      }).map((message) => message.content),
    ).toEqual(['OpenCode returned no output.'])
  })
})

import type { ChatMessage, PromptExecutionResult } from '../types/chat'

const INITIAL_MESSAGES: readonly ChatMessage[] = [
  {
    id: 'system-intro',
    role: 'system',
    content:
      'VoxGolem shell initialized. Voice pipeline wiring starts in the next phase.',
  },
]

export function getInitialMessages(): readonly ChatMessage[] {
  return INITIAL_MESSAGES
}

export function createExecutionMessages(
  result: PromptExecutionResult,
): readonly ChatMessage[] {
  const messages: ChatMessage[] = []

  const stderr = result.stderr.trim()

  for (const event of result.events) {
    if (event.kind === 'text') {
      messages.push({
        id: `assistant-${Date.now()}-${messages.length}`,
        role: 'assistant',
        content: event.text,
      })
      continue
    }

    if (event.kind === 'step_start') {
      messages.push({
        id: `system-step-start-${Date.now()}-${messages.length}`,
        role: 'system',
        content: 'step_start:\nOpenCode started a run step.',
      })
      continue
    }

    if (event.kind === 'step_finish') {
      messages.push({
        id: `system-step-finish-${Date.now()}-${messages.length}`,
        role: 'system',
        content:
          event.reason === null
            ? 'step_finish:\nOpenCode finished a run step.'
            : `step_finish:\n${event.reason}`,
      })
      continue
    }

    if (event.kind === 'tool_use') {
      messages.push({
        id: `system-tool-${Date.now()}-${messages.length}`,
        role: 'system',
        content: `tool_use:\n${event.tool} (${event.status})\n${event.detail}`,
      })
      continue
    }

    messages.push({
      id: `system-error-${Date.now()}-${messages.length}`,
      role: 'system',
      content: `opencode_error:\n${event.name}: ${event.message}`,
    })
  }

  if (stderr.length > 0) {
    messages.push({
      id: `system-${Date.now()}`,
      role: 'system',
      content: `stderr:\n${stderr}`,
    })
  }

  if (result.exitCode !== null && result.exitCode !== 0) {
    messages.push({
      id: `system-exit-${Date.now()}`,
      role: 'system',
      content: `exit_code:\n${result.exitCode}`,
    })
  }

  if (messages.length > 0) {
    return messages
  }

  return [
    {
      id: `assistant-${Date.now()}`,
      role: 'assistant',
      content: 'OpenCode returned no output.',
    },
  ]
}

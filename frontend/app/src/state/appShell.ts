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

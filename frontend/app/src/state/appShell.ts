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

  const stdout = result.stdout.trim()
  const stderr = result.stderr.trim()

  if (stdout.length > 0) {
    messages.push({
      id: `assistant-${Date.now()}`,
      role: 'assistant',
      content: `stdout:\n${stdout}`,
    })
  }

  if (stderr.length > 0) {
    messages.push({
      id: `system-${Date.now()}`,
      role: 'system',
      content: `stderr:\n${stderr}`,
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

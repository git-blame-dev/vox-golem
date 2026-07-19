import type { ChatMessage } from '../types/chat'

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

import type { JSX } from 'react'
import type { TranscriptMessage } from '../types/chat'

interface ChatBubbleProps {
  readonly message: TranscriptMessage
}

export function ChatBubble({ message }: ChatBubbleProps): JSX.Element {
  return (
    <article className={`message message--${message.role}`}>
      <p className="message__content">{message.content}</p>
    </article>
  )
}

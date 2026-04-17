import type { JSX } from 'react'
import type { ChatMessage } from '../types/chat'

interface ChatBubbleProps {
  readonly message: ChatMessage
}

export function ChatBubble({ message }: ChatBubbleProps): JSX.Element {
  return (
    <article className={`message message--${message.role}`}>
      <header className="message__role">{message.role}</header>
      <p className="message__content">{message.content}</p>
    </article>
  )
}

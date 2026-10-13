import type { JSX } from 'react'
import type { TranscriptMessage } from '../types/chat'
import { duration } from '../lib/duration'

interface ChatBubbleProps {
  readonly message: TranscriptMessage
}

export function ChatBubble({ message }: ChatBubbleProps): JSX.Element {
  const isAssistantWaiting = message.role === 'assistant' && message.content.length === 0
  return (
    <article className={`message message--${message.role}`}>
      <p className="message__content">
        {isAssistantWaiting ? <span className="message__thinking">Thinking…</span> : message.content}
      </p>
      {message.role === 'assistant' && message.customDirectTimings ? (
        <small className="conversation__timings">
          {duration(message.customDirectTimings.firstTextMs)}
        </small>
      ) : null}
    </article>
  )
}

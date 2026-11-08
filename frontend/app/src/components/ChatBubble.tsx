import { IconButton } from '../ui/controls'
import { useSyncExternalStore } from 'react'
import { useClipboardFeedback } from '../lib/useClipboardFeedback'
import type { JSX } from 'react'
import type { FrontendSession } from '../FrontendSession'
import type { TranscriptMessage } from '../types/chat'
import { isTranscriptMessage } from '../types/chat'
import { duration } from '../lib/duration'
import { turnObservationAttributes } from '../turnObservation'
import { ControlIcon } from './ControlIcon'
import { isCoreWhitespaceOnly } from '../lib/wire'

interface ChatBubbleProps {
  readonly message: TranscriptMessage
}

function formatTextWithSources(
  text: string,
  sources?: readonly { readonly url: string; readonly title: string }[],
  trimBody = false,
): string {
  const body = trimBody ? text.trim() : text
  if (!sources || sources.length === 0) return body
  const sourceLines = sources.map((source) => {
    const title = source.title.trim()
    return title.length > 0 && title !== source.url
      ? `- ${title} (${source.url})`
      : `- ${source.url}`
  })
  return `${body}\n\nSources:\n${sourceLines.join('\n')}`
}

export function CopyMessageButton({
  text,
  label,
  sources,
}: {
  readonly text: string
  readonly label: string
  readonly sources?: readonly { readonly url: string; readonly title: string }[]
}): JSX.Element | null {
  const { copied, copy } = useClipboardFeedback()
  if (isCoreWhitespaceOnly(text)) return null
  return (
    <IconButton
      className="message__copy-button"
      label={copied ? 'Copied' : label}
      title={copied ? 'Copied' : label}
      data-copied={copied ? 'true' : undefined}
      onClick={() => { void copy(formatTextWithSources(text, sources)) }}
    >
      <ControlIcon kind={copied ? 'check' : 'copy'} />
    </IconButton>
  )
}

export function MessageFooter({ role, text, canCopy, timingLabel, sources, copyLabel }: {
  readonly role: 'user' | 'assistant' | 'system'
  readonly text: string
  readonly canCopy: boolean
  readonly timingLabel: string | null
  readonly copyLabel?: string
  readonly sources?: readonly { readonly url: string; readonly title: string }[]
}): JSX.Element | null {
  if (timingLabel === null && !canCopy) return null
  return <footer className="message__footer" data-has-copy={canCopy || undefined}>
    {timingLabel !== null ? <small className="conversation__timings" data-custom-direct-timings="true">{timingLabel}</small> : null}
    {canCopy ? <CopyMessageButton text={text} label={copyLabel ?? (role === 'user' ? 'Copy prompt' : 'Copy response')} {...(sources === undefined ? {} : { sources })} /> : null}
  </footer>
}

export function CopyConversationButton({ session, disabled = false }: { readonly session: FrontendSession; readonly disabled?: boolean }): JSX.Element {
  const messages = useSyncExternalStore(session.subscribe, session.getMessagesSnapshot, session.getMessagesSnapshot)
  const { copied, copy } = useClipboardFeedback()
  const exportable = messages.filter(
    (m) => isTranscriptMessage(m) && (m.role === 'user' || m.role === 'assistant') && m.content.trim().length > 0,
  )
  const canExport = !disabled && exportable.length > 0
  const label = copied ? 'Copied conversation' : 'Copy conversation'
  return (
    <button
      type="button"
      className="shell__control shell__icon-control shell__export-conversation"
      aria-label={label}
      title={label}
      disabled={!canExport}
      data-copied={copied ? 'true' : undefined}
      onClick={() => {
        if (!canExport) return
        const formatted = exportable
          .map((m) => `${m.role === 'user' ? 'User' : 'Assistant'}:\n${formatTextWithSources(m.content, m.answerStage?.sources, true)}`)
          .join('\n\n')
        void copy(formatted)
      }}
    >
      <ControlIcon kind={copied ? 'check' : 'copy'} />
    </button>
  )
}

export function ChatBubble({ message }: ChatBubbleProps): JSX.Element {
  const isAssistantWaiting = message.role === 'assistant' && message.content.length === 0
  const timings = message.role === 'assistant' ? message.customDirectTimings : undefined
  const timingLabel = timings === undefined
    ? null
    : duration(timings.firstTextMs)
  const canCopy = (message.role === 'user' || message.role === 'assistant') && message.content.trim().length > 0
  return (
    <article className={`message message--${message.role}`} {...turnObservationAttributes(message)}>
      <p className="message__content">
        {isAssistantWaiting ? <span className="message__thinking message__status-pill">{message.thinkingStatus ?? 'Thinking…'}</span> : message.content}
      </p>
      <MessageFooter
        role={message.role}
        text={message.content}
        canCopy={canCopy}
        timingLabel={timingLabel}
        {...(message.answerStage?.sources === undefined ? {} : { sources: message.answerStage.sources })}
      />
    </article>
  )
}

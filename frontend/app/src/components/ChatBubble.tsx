import { useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore } from 'react'
import type { JSX } from 'react'
import type { FrontendSession } from '../FrontendSession'
import type { TranscriptMessage } from '../types/chat'
import { isTranscriptMessage } from '../types/chat'
import { duration } from '../lib/duration'
import { turnObservationAttributes } from '../turnObservation'
import { ControlIcon } from './ControlIcon'

interface ChatBubbleProps {
  readonly message: TranscriptMessage
}

async function copyTextToClipboard(text: string): Promise<boolean> {
  try {
    if (typeof navigator !== 'undefined' && navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text)
      return true
    }
  } catch {
    // Fall back to execCommand('copy') in restricted WebViews
  }
  try {
    const textarea = document.createElement('textarea')
    textarea.value = text
    textarea.setAttribute('readonly', '')
    textarea.style.position = 'fixed'
    textarea.style.opacity = '0'
    textarea.style.pointerEvents = 'none'
    document.body.appendChild(textarea)
    textarea.select()
    const copied = typeof document.execCommand === 'function' && document.execCommand('copy')
    document.body.removeChild(textarea)
    return copied
  } catch {
    return false
  }
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
  const [copied, setCopied] = useState(false)
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  useEffect(() => () => {
    if (timerRef.current !== null) clearTimeout(timerRef.current)
  }, [])
  if (text.trim().length === 0) return null
  return (
    <button
      type="button"
      className="message__copy-button"
      aria-label={copied ? 'Copied' : label}
      title={copied ? 'Copied' : label}
      data-copied={copied ? 'true' : undefined}
      onClick={() => {
        void copyTextToClipboard(formatTextWithSources(text, sources)).then((ok) => {
          if (!ok) return
          setCopied(true)
          if (timerRef.current !== null) clearTimeout(timerRef.current)
          timerRef.current = setTimeout(() => setCopied(false), 1500)
        })
      }}
    >
      <ControlIcon kind={copied ? 'check' : 'copy'} />
    </button>
  )
}

export function MessageFooter({
  role,
  text,
  canCopy,
  timingLabel,
  sources,
  canRaise = true,
}: {
  readonly role: 'user' | 'assistant' | 'system'
  readonly text: string
  readonly canCopy: boolean
  readonly timingLabel: string | null
  readonly sources?: readonly { readonly url: string; readonly title: string }[]
  readonly canRaise?: boolean
}): JSX.Element | null {
  const footerRef = useRef<HTMLElement | null>(null)
  const [raised, setRaised] = useState(false)

  const shouldEvaluateRaise = timingLabel === null && canRaise !== false && canCopy

  useLayoutEffect(() => {
    const footerEl = footerRef.current
    if (footerEl === null || !shouldEvaluateRaise) {
      setRaised(false)
      return
    }
    const paragraph = footerEl.parentElement?.querySelector('.message__content, .answer-stage__answer')
    if (!(paragraph instanceof HTMLElement)) {
      setRaised(false)
      return
    }

    const checkRaise = (): void => {
      const pRect = paragraph.getBoundingClientRect()
      if (pRect.width === 0 || pRect.height === 0) {
        setRaised(false)
        return
      }
      const range = document.createRange()
      range.selectNodeContents(paragraph)
      const rects = range.getClientRects()
      const lastRect = rects.length > 0 ? rects[rects.length - 1] ?? null : null
      const buttonClearance = 32

      let isRoomAvailable = false
      if (lastRect !== null) {
        if (role === 'assistant') {
          isRoomAvailable = pRect.right - lastRect.right >= buttonClearance
        } else if (role === 'user') {
          isRoomAvailable = lastRect.left - pRect.left >= buttonClearance
        }
      }
      setRaised(isRoomAvailable)
    }

    checkRaise()

    const observer = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(() => {
      checkRaise()
    })
    observer?.observe(paragraph)
    observer?.observe(footerEl)

    return () => {
      observer?.disconnect()
    }
  }, [role, text, shouldEvaluateRaise])

  if (timingLabel === null && !canCopy) return null

  return (
    <footer
      ref={footerRef as React.RefObject<HTMLElement>}
      className={`message__footer message__footer--${role}`}
      {...(raised ? { 'data-raised': 'true' } : {})}
    >
      {timingLabel !== null ? (
        <small className="conversation__timings" data-custom-direct-timings="true">
          {timingLabel}
        </small>
      ) : null}
      {canCopy ? (
        <CopyMessageButton
          text={text}
          label={role === 'user' ? 'Copy prompt' : 'Copy response'}
          {...(sources === undefined ? {} : { sources })}
        />
      ) : null}
    </footer>
  )
}

export function CopyConversationButton({ session, disabled = false }: { readonly session: FrontendSession; readonly disabled?: boolean }): JSX.Element {
  const messages = useSyncExternalStore(session.subscribe, session.getMessagesSnapshot, session.getMessagesSnapshot)
  const [copied, setCopied] = useState(false)
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  useEffect(() => () => {
    if (timerRef.current !== null) clearTimeout(timerRef.current)
  }, [])
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
        void copyTextToClipboard(formatted).then((ok) => {
          if (!ok) return
          setCopied(true)
          if (timerRef.current !== null) clearTimeout(timerRef.current)
          timerRef.current = setTimeout(() => setCopied(false), 1500)
        })
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

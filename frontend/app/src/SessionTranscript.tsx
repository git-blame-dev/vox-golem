import { memo, useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react'
import type { FrontendSession } from './FrontendSession'
import type { TranscriptMessage, UiTextSize } from './types/chat'
import { isTranscriptMessage } from './types/chat'
import { ChatBubble } from './components/ChatBubble'
import { AnswerStage } from './components/AnswerStage'
import { isAndroidRuntime } from './lib/tauri'
import { duration } from './lib/duration'

export function SessionTranscript({ session, staging, composerValue, textSize, noticeId }: {
  readonly session: FrontendSession
  readonly staging: boolean
  readonly composerValue: string
  readonly textSize: UiTextSize
  readonly noticeId: string | undefined
}) {
  const messages = useSyncExternalStore(session.subscribe, session.getMessagesSnapshot, session.getMessagesSnapshot)
  const visibleMessages = useMemo(() => staging ? [] : messages.filter(isTranscriptMessage), [messages, staging])
  const conversationRef = useRef<HTMLElement | null>(null)
  const followLatest = useRef(true)
  const jumping = useRef(false)
  const [showJump, setShowJump] = useState(false)
  const scrollToLatest = useCallback((behavior?: ScrollBehavior): void => {
    const conversation = conversationRef.current
    if (conversation === null || !followLatest.current) {
      if (conversation !== null) setShowJump(true)
      return
    }
    if (typeof conversation.scrollTo === 'function') {
      conversation.scrollTo({ top: conversation.scrollHeight, ...(behavior === undefined ? {} : { behavior }) })
    } else conversation.scrollTop = conversation.scrollHeight
  }, [])
  useEffect(() => { scrollToLatest() }, [scrollToLatest, visibleMessages])
  useEffect(() => {
    const conversation = conversationRef.current
    if (conversation === null) return undefined
    const observer = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(() => scrollToLatest())
    observer?.observe(conversation)
    return () => observer?.disconnect()
  }, [scrollToLatest])
  useEffect(() => { scrollToLatest() }, [composerValue, noticeId, scrollToLatest, textSize])
  const interrupt = (): void => { jumping.current = false }
  return <>
    <main ref={conversationRef} className="conversation" onScroll={() => {
      const conversation = conversationRef.current
      if (conversation === null) return
      const nearLatest = conversation.scrollHeight - conversation.scrollTop - conversation.clientHeight <= 96
      if (jumping.current) { if (nearLatest) jumping.current = false; return }
      followLatest.current = nearLatest
      setShowJump(!nearLatest)
    }} onWheel={interrupt} onTouchStart={interrupt} onPointerDown={interrupt}>
      {visibleMessages.map((message) => <TranscriptRow key={message.id} message={message} />)}
    </main>
    {showJump ? <button type="button" className="conversation__jump" aria-label="Jump to latest" onClick={() => {
      followLatest.current = true
      jumping.current = true
      setShowJump(false)
      scrollToLatest('smooth')
    }}>Jump to latest</button> : null}
  </>
}

const TranscriptRow = memo(function TranscriptRow({ message }: { readonly message: TranscriptMessage }) {
  if (!message.answerStage || message.role !== 'assistant') return <ChatBubble message={message} />
  return <div className="conversation__answer">
    <AnswerStage compact={isAndroidRuntime} answer={message.content} className="message message--assistant" {...(message.contentKind === undefined ? {} : { contentKind: message.contentKind })} {...message.answerStage} />
    {message.customDirectTimings ? <small className="conversation__timings">{duration(message.customDirectTimings.firstTextMs)}</small> : null}
  </div>
})

export function ResponseAnnouncements({ session }: { readonly session: FrontendSession }) {
  const announcement = useSyncExternalStore(session.subscribe, session.getAnnouncementSnapshot, session.getAnnouncementSnapshot)
  return <div className="status-announcer"><div className="sr-only" role="status" aria-live="polite" aria-atomic="true" aria-label="Response announcements">{announcement}</div></div>
}

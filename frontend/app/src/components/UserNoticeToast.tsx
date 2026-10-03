import type { JSX } from 'react'
import type { UserNotice } from '../types/chat'

interface UserNoticeToastProps {
  readonly notice: UserNotice | null
}

export function UserNoticeToast({ notice }: UserNoticeToastProps): JSX.Element | null {
  if (notice === null) {
    return null
  }

  const titleId = `conversation-notice-${notice.id}-title`
  const messageId = `conversation-notice-${notice.id}-message`

  return (
    <aside
      className={`notice-toast notice-toast--${notice.tone}`}
      role="status"
      aria-live="polite"
      aria-labelledby={titleId}
      aria-describedby={messageId}
      tabIndex={0}
    >
      <strong id={titleId} className="notice-toast__title">{notice.title}</strong>
      <span id={messageId} className="notice-toast__message">{notice.message}</span>
    </aside>
  )
}

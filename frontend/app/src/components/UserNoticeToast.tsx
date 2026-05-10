import type { JSX } from 'react'
import type { UserNotice } from '../types/chat'

interface UserNoticeToastProps {
  readonly notice: UserNotice | null
}

export function UserNoticeToast({ notice }: UserNoticeToastProps): JSX.Element | null {
  if (notice === null) {
    return null
  }

  return (
    <aside
      className={`notice-toast notice-toast--${notice.tone}`}
      role="status"
      aria-live="polite"
    >
      <strong className="notice-toast__title">{notice.title}</strong>
      <span className="notice-toast__message">{notice.message}</span>
    </aside>
  )
}

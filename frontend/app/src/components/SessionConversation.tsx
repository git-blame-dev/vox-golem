import { memo } from 'react'
import { ResponseAnnouncements, SessionTranscript } from '../SessionTranscript'
import { sameExternalView, usePresentation } from '../sessionPolicy'
import type { SessionViewProps } from '../sessionPolicy'
import { CaptureHistory } from './CaptureHistory'
import { UserNoticeToast } from './UserNoticeToast'

export const SessionConversation = memo(function SessionConversation({ session, view }: SessionViewProps) {
  const { voiceNotesOpen,
    shortcutStaging, composerValue, uiTextSize, notices } = usePresentation(session, view, 'conversation')
  const { setVoiceNotesOpen, setNoticeInteraction } = view.actions
  return voiceNotesOpen ? <CaptureHistory onBack={() => setVoiceNotesOpen(false)} /> : <>
      <div className="conversation-region">
        <SessionTranscript session={session} staging={shortcutStaging} composerValue={composerValue} textSize={uiTextSize} noticeId={notices[0]?.id} />

        <div
          className="conversation-notice"
          onFocus={() => setNoticeInteraction((current) => ({ ...current, focused: true }))}
          onBlur={(event) => {
            if (!event.currentTarget.contains(event.relatedTarget)) {
              setNoticeInteraction((current) => ({ ...current, focused: false }))
            }
          }}
          onMouseEnter={() => setNoticeInteraction((current) => ({ ...current, hovered: true }))}
          onMouseLeave={() => setNoticeInteraction((current) => ({ ...current, hovered: false }))}
        >
          <UserNoticeToast notice={notices[0] ?? null} />
        </div>
      </div>

      <ResponseAnnouncements session={session} />
      </>
}, sameExternalView)

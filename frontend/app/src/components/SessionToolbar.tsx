import { memo } from 'react'
import headerIcon from '../../../../apps/desktop-tauri/src-tauri/icons/icon.svg?url'
import { isAndroidRuntime, isBrowserPreviewRuntime } from '../lib/tauri'
import { SETTINGS_DIALOG_ID } from '../lib/useSettingsNavigation'
import { sameExternalView, usePresentation } from '../sessionPolicy'
import type { SessionViewProps } from '../sessionPolicy'
import { AndroidVoiceSecondaryControls } from './AndroidVoiceControls'
import { CopyConversationButton } from './ChatBubble'
import { ControlIcon } from './ControlIcon'


export const SessionToolbar = memo(function SessionToolbar({ session, view }: SessionViewProps) {
  const {
    updateRuntimeBlocked, shortcutStaging, androidAssistant, androidConversationId,
    headPagePending, androidVoiceActive, voiceNotesOpen,
    snapshotControlsFresh,
    promptState, resetPending,
    headerBusy,
    supportsDesktopRuntime, ttsPlaying, ttsEnabled, canToggleTts,
  } = usePresentation(session, view, 'toolbar')
  const { settingsOpen, settingsButtonRef, openSettings } = view.navigation
  const {
    setVoiceNotesOpen, selectAndroidConversation, toggleTts,
  } = view.actions
  const notesControl = <button type="button" className="shell__control shell__icon-control" aria-label={voiceNotesOpen ? 'Chat' : 'Voice notes'} title={voiceNotesOpen ? 'Chat' : 'Voice notes'} aria-pressed={voiceNotesOpen} onClick={() => setVoiceNotesOpen((open) => !open)}>
    <ControlIcon kind="notes" />
  </button>
  const conversationControl = androidAssistant !== null ? <label className="shell__control shell__icon-control shell__conversation-picker" title={androidAssistant.conversations.find((item) => item.id === androidConversationId)?.title ?? 'Conversation'}>
    <ControlIcon kind="conversation" />
    <select className="shell__conversation-select" aria-label="Conversation" value={androidConversationId} disabled={shortcutStaging || resetPending || promptState !== 'idle' || androidVoiceActive || headPagePending || !snapshotControlsFresh} onChange={(event) => void selectAndroidConversation('select_conversation', event.target.value)}>
      <option value="0" disabled>New draft</option>
      {androidAssistant.conversations.map((conversation) => <option key={conversation.id} value={conversation.id}>{conversation.title}</option>)}
    </select>
  </label> : null
  const exportConversationControl = <CopyConversationButton session={session} disabled={shortcutStaging || resetPending || updateRuntimeBlocked} />
  const settingsControl = <button ref={settingsButtonRef} type="button" className="shell__control shell__settings-button shell__icon-control" onClick={openSettings} aria-label="Settings" aria-haspopup="dialog" aria-controls={SETTINGS_DIALOG_ID} aria-expanded={settingsOpen} title="Settings">
    <ControlIcon kind="settings" />
  </button>
  const desktopSpeakerDescription = ttsPlaying ? 'Speaking…' : ttsEnabled ? 'Speak replies enabled' : canToggleTts ? 'Speak replies off' : 'Speech output is unavailable'
  const desktopSpeakerControl = supportsDesktopRuntime ? (
    <button
      type="button"
      className="shell__control shell__icon-control shell__speaker-control"
      aria-label="Speak replies"
      title={`Speak replies: ${desktopSpeakerDescription}`}
      aria-pressed={ttsEnabled}
      disabled={!canToggleTts && !ttsEnabled}
      onClick={() => void toggleTts(!ttsEnabled)}
    >
      <ControlIcon kind="speaker" />
    </button>
  ) : null

  return (
    <header className="shell__header" aria-label="VoxGolem">
      <div className="shell__identity" data-shell-identity>
        <img className="shell__logo" src={headerIcon} alt="VoxGolem" width={40} height={40} data-busy={headerBusy} />
        <div className="shell__runtime">
          {isBrowserPreviewRuntime() ? <span data-browser-preview>Browser preview</span> : null}
        </div>
      </div>
      <div className="shell__header-actions" role={isAndroidRuntime ? 'group' : undefined} aria-label={isAndroidRuntime ? 'Android toolbar' : undefined} aria-describedby={isAndroidRuntime && shortcutStaging ? 'new-voice-draft-status' : undefined}>
        {isAndroidRuntime ? <>{!voiceNotesOpen ? <AndroidVoiceSecondaryControls /> : null}{notesControl}{conversationControl}{exportConversationControl}{settingsControl}</> : null}
        {!isAndroidRuntime ? <>{desktopSpeakerControl}{exportConversationControl}{settingsControl}</> : null}
      </div>
    </header>
  )
}, sameExternalView)

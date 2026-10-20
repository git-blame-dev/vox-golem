import { memo } from 'react'
import { isAndroidRuntime } from '../lib/tauri'
import { localInstantCanRetry, sameExternalView, usePresentation, voiceInputReady } from '../sessionPolicy'
import type { SessionViewProps } from '../sessionPolicy'
import { AndroidComposerVoiceControl } from './AndroidVoiceControls'
import { ControlIcon } from './ControlIcon'
import { InstantModelSelect } from './InstantModelSelect'
import { PromptComposer } from './PromptComposer'

export const SessionComposer = memo(function SessionComposer({ session, view }: SessionViewProps) {
  const { voiceNotesOpen, shortcutStaging, androidAssistant, conversationPagePending, resetPending, promptState,
    androidVoiceActive, headPagePending, snapshotControlsFresh, composerValue, uiTextSize, partialTranscript,
    typedCompletionSuffix, voiceCompletionSuffix, canSend, startupState, assistantSettingsLoadError, selectedInstantOption,
    selectedInstantProfileReady, canToggleMic, micStarting, micActive, assistantSettings, instantAssistantOptions,
    instantControlsDisabled, runtimeStatus, updateRuntimeBlocked,
    promptActivity, wakeConfidence, customFailure, supportsDesktopRuntime, customRetryPending } = usePresentation(session, view, 'composer')
  const { onSubmit, loadMoreAndroidConversations, loadMoreAndroidHeads, selectAndroidConversation, changeComposer,
    onComposerKeyDown, acceptTypedCompletion, clearCompletion, toggleMic, changeInstant, cancelPrompt, retryCustom } = view.actions
  const canStop = (promptState === 'executing' || promptState === 'stopping') && startupState.kind === 'ready' && startupState.promptCancellationAvailable
  const busy = promptState !== 'idle' || runtimeStatus === 'processing' || runtimeStatus === 'executing'
  const actionLabel = canStop ? promptState === 'stopping' ? 'Stopping…' : 'Stop' : busy ? 'Working…' : 'Send'
  const selectedUnavailable = startupState.kind === 'ready' && (selectedInstantOption?.available !== true || !selectedInstantProfileReady)
  let unavailableReason = selectedInstantOption?.reason ?? 'selected provider is unavailable'
  if (selectedInstantOption?.available === true && !selectedInstantProfileReady) unavailableReason = 'selected local model profile is not loaded'
  if (unavailableReason === 'response_backend is not configured') unavailableReason = 'selected provider is not configured'
  const showAvailability = startupState.kind === 'error' || startupState.kind === 'warming_model' || assistantSettingsLoadError !== null || selectedUnavailable
  return !voiceNotesOpen ? <form className="composer" onSubmit={onSubmit}>
    {shortcutStaging ? <p id="new-voice-draft-status" className={isAndroidRuntime ? 'sr-only' : undefined} role="status">New voice draft — Stop &amp; send starts a new conversation. Previous history and draft are preserved.</p> : null}
    {isAndroidRuntime && androidAssistant !== null ? <div className="composer__actions">
      {androidAssistant.conversationOffset < androidAssistant.conversationCount ? <button type="button" disabled={conversationPagePending || resetPending || promptState !== 'idle' || androidVoiceActive} onClick={() => void loadMoreAndroidConversations()}>{conversationPagePending ? 'Loading conversations…' : 'More conversations'}</button> : null}
      {androidAssistant.branchChoices.length > 0 ? <label>Choose continuation <select aria-label="Choose continuation" value="" disabled={shortcutStaging || resetPending || promptState !== 'idle' || androidVoiceActive || headPagePending || !snapshotControlsFresh} onChange={(event) => void selectAndroidConversation('select_conversation_head', event.target.value)}>
        <option value="" disabled>Choose continuation</option>
        {androidAssistant.branchChoices.map((head, index) => <option key={head} value={head}>Continuation {index + 1}</option>)}
      </select></label> : null}
      {androidAssistant.branchOffset < androidAssistant.branchCount ? <button type="button" disabled={headPagePending || resetPending || promptState !== 'idle' || androidVoiceActive} onClick={() => void loadMoreAndroidHeads()}>{headPagePending ? 'Loading continuations…' : 'More continuations'}</button> : null}
      {androidAssistant.error !== null && androidAssistant.error !== 'interrupted' ? <span role="status">Native conversation status: {androidAssistant.error.replaceAll('_', ' ')}</span> : null}
    </div> : null}
    <div className="composer__surface">
      <div className="composer__prompt-row">
        <PromptComposer
          id="promptComposer"
          className="composer__input"
          aria-label="Prompt"
          value={composerValue}
          typographyRevision={uiTextSize}
          partialTranscript={partialTranscript}
          ghostSuffix={typedCompletionSuffix}
          onChange={changeComposer}
          onKeyDown={onComposerKeyDown}
          onAcceptCompletion={acceptTypedCompletion}
          onDismissCompletion={clearCompletion}
          partialCompletionSuffix={voiceCompletionSuffix}
          placeholder="Ask Assistant…"
          rows={1}
          maxRows={6}
        />
      </div>
      <div className="composer__utility-row">
        <InstantModelSelect variant="pill" id="composerInstantSelect" label="Instant model" value={assistantSettings.instant} options={instantAssistantOptions} disabled={instantControlsDisabled} canRetry={(value) => localInstantCanRetry(startupState, value)} onChange={(value) => void changeInstant(value)} />
        <div className="composer__utility-actions">
          {!isAndroidRuntime && runtimeStatus === 'sleeping' && wakeConfidence !== null ? (
            <div className={wakeConfidence >= 0.7 ? 'composer__wake composer__wake--high' : wakeConfidence >= 0.4 ? 'composer__wake composer__wake--medium' : 'composer__wake composer__wake--low'}>
              <span className="composer__wake-primary">wake: {(wakeConfidence * 100).toFixed(2)}%</span>
            </div>
          ) : null}
          {isAndroidRuntime ? <AndroidComposerVoiceControl /> : voiceInputReady(startupState) ? <button type="button" className="shell__control shell__icon-control" onClick={toggleMic} disabled={!canToggleMic} aria-label={micStarting ? 'Starting mic…' : micActive ? 'Stop mic' : 'Start mic'} title={micActive ? 'Stop mic' : 'Start mic'}><ControlIcon kind={micActive ? 'stop' : 'mic'} /></button> : null}
          <button type={busy ? 'button' : 'submit'} className="composer__button shell__icon-control" aria-label={actionLabel} title={actionLabel}
            disabled={canStop ? promptState === 'stopping' || updateRuntimeBlocked : busy || !canSend} onClick={canStop ? cancelPrompt : undefined}>
            <ControlIcon kind={canStop ? 'stop' : 'send'} />
          </button>
        </div>
      </div>
      {showAvailability ? <div className="composer__availability">
        {startupState.kind === 'error' ? (
          <p className="shell__error">Startup error: {startupState.message}</p>
        ) : null}
        {assistantSettingsLoadError !== null ? (
          <p className="shell__error">Assistant settings unavailable: {assistantSettingsLoadError}</p>
        ) : null}
        {selectedUnavailable ? (
          <p className="shell__error" role="status">
            Sending disabled: {unavailableReason}
          </p>
        ) : null}
        {selectedUnavailable && assistantSettings.instant.startsWith('custom-') && customFailure && supportsDesktopRuntime ? <button type="button" className="shell__control" aria-label="Retry Custom" disabled={customRetryPending || updateRuntimeBlocked} onClick={() => void retryCustom()}>{customRetryPending ? 'Retrying…' : 'Retry Custom'}</button> : null}
        {startupState.kind === 'warming_model' ? (
          <p className="shell__loading">Model loading: {startupState.message}</p>
        ) : null}
      </div> : null}
    </div>
    {promptActivity !== null ? (
      <div className="composer__actions composer__actions--secondary">
        <div className={promptActivity.compact ? 'sr-only' : 'composer__status'} role="status" aria-live="polite">{promptActivity.text}</div>
      </div>
    ) : null}
  </form> : null
}, sameExternalView)

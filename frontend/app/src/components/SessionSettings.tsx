import { memo } from 'react'
import type { AssistantSettings } from '../lib/assistantSettings'
import { isAndroidRuntime, isBrowserPreviewRuntime } from '../lib/tauri'
import { SETTINGS_DIALOG_ID } from '../lib/useSettingsNavigation'
import {
  UI_TEXT_SIZE_LABELS,
  UI_TEXT_SIZE_PERCENTAGES,
  localInstantCanRetry,
  parseUiTheme,
  sameExternalView,
  usePresentation,
  voiceInputReady,
} from '../sessionPolicy'
import type { SessionViewProps } from '../sessionPolicy'
import { ChatGptAuth } from './ChatGptAuth'
import { InstantModelSelect } from './InstantModelSelect'
import { NewVoiceShortcutSettings } from './NewVoiceShortcutSettings'
import { SpeechSettings } from './SpeechSettings'
import { UpdateSettings } from './UpdateSettings'
import { WakeClipSettings } from './WakeClipSettings'
import { WakeDiagnostics } from './WakeDiagnostics'

export const SessionSettings = memo(function SessionSettings({ session, view }: SessionViewProps) {
  const { uiTheme, assistantSettingsWriteError, uiThemeWriteError, uiTextSize, canDecreaseTextSize, canIncreaseTextSize,
    supportsDesktopRuntime, selectedAudioInputDevice, audioInputDeviceId, micStarting, updateRuntimeBlocked, audioInputDevices,
    autoStopOnSilence, startupState, voiceInputUnavailableReason, ttsEnabled, canToggleTts, androidVoiceStatus,
    androidSpeechStatusError, assistantSettings, instantControlsDisabled, instantAssistantOptions, assistantCapabilities,
    selectedInstantRetryable, selectedInstantOption, assistantControlsDisabled, deepAssistantOptions, reviewAssistantOptions,
    canEnableDeep, canEnableReview, appUpdates, updateInstallationDisabled, customFailure, customRetryPending } = usePresentation(session, view, 'settings')
  const { settingsOpen, settingsPanelRef, settingsCloseButtonRef, closeSettings, onSettingsKeyDown } = view.navigation
  const { changeUiTheme, adjustUiTextSize, changeAudioInputDevice, setAutoStopOnSilence, toggleTts, patchAssistantSettings,
    changeInstant, clearCompletion, retryCustom, refreshChatGptAuth } = view.actions
  return settingsOpen ? (
        <div
          className="settings-overlay"
          onMouseDown={() => closeSettings()}
          onKeyDown={onSettingsKeyDown}
        >
          <section
            ref={settingsPanelRef}
            className="settings-panel"
            role="dialog"
            id={SETTINGS_DIALOG_ID}
            aria-modal="true"
            aria-labelledby="settings-title"
            onMouseDown={(event) => event.stopPropagation()}
          >
            <div className="settings-panel__header">
              <div>
                <h2 id="settings-title">Settings</h2>
              </div>
              <button
                ref={settingsCloseButtonRef}
                type="button"
                className="settings-panel__close"
                onClick={() => closeSettings()}
                aria-label="Close settings"
              >
                ×
              </button>
              {assistantSettingsWriteError !== null || uiThemeWriteError !== null ? (
                <div className="settings-panel__write-error" role="alert" tabIndex={0}>
                  {assistantSettingsWriteError !== null ? <p className="shell__error">Assistant settings could not be saved: {assistantSettingsWriteError}</p> : null}
                  {uiThemeWriteError !== null ? <p className="shell__error">Theme could not be saved: {uiThemeWriteError}</p> : null}
                </div>
              ) : null}
            </div>
            <div className="settings-panel__row">
              <label htmlFor="uiTheme"><strong>Theme</strong></label>
              <select id="uiTheme" className="shell__control settings-panel__interactive" aria-label="Theme" value={uiTheme} onChange={(event) => changeUiTheme(parseUiTheme(event.target.value))}>
                <option value="dark">OLED Night</option><option value="slate">Slate</option><option value="light">Light</option>
              </select>
            </div>
            <div className="settings-panel__row">
              <div>
                <strong>Text size</strong>
                <p className="settings-panel__hint">Applies to chat, composer, and controls.</p>
              </div>
              <div className="settings-panel__stepper" aria-label="Text size controls">
                <button
                  type="button"
                  className="settings-panel__step-button settings-panel__interactive"
                  onClick={() => adjustUiTextSize(-1)}
                  disabled={!canDecreaseTextSize}
                  aria-label="Decrease text size"
                >
                  −
                </button>
                <span className="settings-panel__size" aria-live="polite">
                  {UI_TEXT_SIZE_LABELS[uiTextSize]} ({UI_TEXT_SIZE_PERCENTAGES[uiTextSize]})
                </span>
                <button
                  type="button"
                  className="settings-panel__step-button settings-panel__interactive"
                  onClick={() => adjustUiTextSize(1)}
                  disabled={!canIncreaseTextSize}
                  aria-label="Increase text size"
                >
                  +
                </button>
              </div>
            </div>
            <section className="settings-panel__assistant" aria-labelledby="model-settings-title">
              <h3 id="model-settings-title">Model</h3>
              <ChatGptAuth onChanged={refreshChatGptAuth} />
              <label className="settings-panel__interactive"><strong>Instant</strong><InstantModelSelect id="assistantInstantSelect" label="Instant" value={assistantSettings.instant} options={instantAssistantOptions} disabled={instantControlsDisabled} canRetry={(value) => localInstantCanRetry(startupState, value)} onChange={(value) => void changeInstant(value)} /></label>
              {startupState.kind === 'ready' && !assistantCapabilities.custom ? <div>
                <p className="settings-panel__hint">Custom connection: {customFailure?.reason ?? 'unavailable'}</p>
                {customFailure && supportsDesktopRuntime ? <button type="button" className="shell__control" aria-label="Retry Custom" disabled={customRetryPending || updateRuntimeBlocked} onClick={() => void retryCustom()}>{customRetryPending ? 'Retrying…' : 'Retry Custom'}</button> : null}
              </div> : null}
              {selectedInstantRetryable && selectedInstantOption ? <button className="settings-panel__interactive" type="button" aria-label="Retry local profile" disabled={instantControlsDisabled} onClick={() => void changeInstant(assistantSettings.instant)}>Retry local profile</button> : null}
              {!isAndroidRuntime ? <details className="settings-panel__advanced">
                <summary>Advanced model settings</summary>
                <label className="settings-panel__interactive"><strong>Deep model</strong><select aria-label="Deep model" value={assistantSettings.deep} disabled={assistantControlsDisabled} onChange={(event) => {
                  const value = event.target.value as AssistantSettings['deep']
                  if (deepAssistantOptions.find((option) => option.value === value)?.available) patchAssistantSettings({ deep: value })
                }}>
                  {deepAssistantOptions.map((option) => <option key={option.value} value={option.value} disabled={!option.available} title={option.reason}>{option.label}</option>)}
                </select></label>
                <label className="settings-panel__interactive"><strong>Review model</strong><select aria-label="Review model" value={assistantSettings.review} disabled={assistantControlsDisabled} onChange={(event) => {
                  const value = event.target.value as AssistantSettings['review']
                  if (reviewAssistantOptions.find((option) => option.value === value)?.available) patchAssistantSettings({ review: value })
                }}>
                  {reviewAssistantOptions.map((option) => <option key={option.value} value={option.value} disabled={!option.available} title={option.reason}>{option.label}</option>)}
                </select></label>
                <label className="settings-panel__interactive"><input type="checkbox" checked={assistantSettings.deepEnabled} disabled={assistantControlsDisabled || (!assistantSettings.deepEnabled && !canEnableDeep)} onChange={(event) => patchAssistantSettings({ deepEnabled: event.target.checked })} /> Deep</label>
                <label className="settings-panel__interactive"><input type="checkbox" checked={assistantSettings.reviewEnabled} disabled={assistantControlsDisabled || (!assistantSettings.reviewEnabled && !canEnableReview)} onChange={(event) => patchAssistantSettings({ reviewEnabled: event.target.checked })} /> Review</label>
                <label className="settings-panel__interactive"><input type="checkbox" checked={assistantSettings.completion} disabled={assistantControlsDisabled || (!assistantSettings.completion && !assistantCapabilities.qwenPrediction)} onChange={(event) => {
                  if (!event.target.checked) clearCompletion()
                  patchAssistantSettings({ completion: event.target.checked })
                }} /> Completion</label>
                <label className="settings-panel__interactive"><input type="checkbox" checked={assistantSettings.prefetch} disabled={assistantControlsDisabled || !supportsDesktopRuntime} aria-describedby="prefetch-help" onChange={(event) => patchAssistantSettings({ prefetch: event.target.checked })} /> Prefetch</label>
                <p id="prefetch-help" className="settings-panel__hint">{supportsDesktopRuntime ? 'Prefetch may transmit unaccepted predicted text when enabled.' : 'Prefetch is unavailable on this runtime.'}</p>
              </details> : null}
            </section>
            {supportsDesktopRuntime ? <div className="settings-panel__row settings-panel__mic-row">
              <div>
                <label htmlFor="audioInputDevice"><strong>Microphone</strong></label>
                <p className="settings-panel__hint">Choose the input used for wake-word listening.</p>
              </div>
               <select
                 className="settings-panel__interactive"
                id="audioInputDevice"
                aria-label="Microphone"
                title={selectedAudioInputDevice?.label ?? ''}
                value={audioInputDeviceId ?? ''}
                disabled={micStarting || updateRuntimeBlocked}
                onChange={(event) => changeAudioInputDevice(event.target.value)}
              >
                <option value="">System selected microphone</option>
               {audioInputDevices.map((device) => (
                  <option key={device.deviceId} value={device.deviceId}>{device.label}</option>
                ))}
              </select>
              {selectedAudioInputDevice && (
                <p className="settings-panel__hint settings-panel__mic-name">{selectedAudioInputDevice.label}</p>
              )}
           </div> : null}
            {supportsDesktopRuntime ? <div className="settings-panel__row">
              <div>
                <strong>Voice input</strong>
                <p className="settings-panel__hint">Stop listening automatically after silence.</p>
              </div>
               <label className="shell__toggle settings-panel__interactive">
                <input
                  type="checkbox"
                  checked={autoStopOnSilence}
                  onChange={(event) => setAutoStopOnSilence(event.target.checked)}
                  disabled={!voiceInputReady(startupState) || updateRuntimeBlocked}
                  aria-describedby={!voiceInputReady(startupState) ? 'voice-input-help' : undefined}
                />
                <span>Auto Stop</span>
              </label>
              {!voiceInputReady(startupState) ? <span id="voice-input-help" className="sr-only">{voiceInputUnavailableReason}</span> : null}
            </div> : null}
            {supportsDesktopRuntime || isAndroidRuntime ? <div className="settings-panel__row">
              <div>
                <strong>Speech output</strong>
                <p className="settings-panel__hint">Read assistant responses aloud.</p>
              </div>
               <label className="shell__toggle settings-panel__interactive">
                <input
                  id="tts-toggle"
                  type="checkbox"
                  checked={isAndroidRuntime ? (androidVoiceStatus?.ttsEnabled ?? ttsEnabled) : ttsEnabled}
                  disabled={isAndroidRuntime ? (view.voice.pending || (androidVoiceStatus?.ttsEnabled !== true && (instantControlsDisabled || androidVoiceStatus?.ttsAvailable !== true))) : !canToggleTts}
                  onChange={isAndroidRuntime ? () => view.voice.toggleSpeech() : (event) => { void toggleTts(event.target.checked) }}
                />
                <span>TTS</span>
              </label>
            </div> : null}
            {isAndroidRuntime || supportsDesktopRuntime ? <SpeechSettings android={isAndroidRuntime} androidStatus={androidVoiceStatus} statusError={androidSpeechStatusError} startupState={startupState}
              speechNetworkFallback={assistantSettings.speechNetworkFallback} speechSelection={assistantSettings} disabled={instantControlsDisabled}
              onSpeechSelectionChange={patchAssistantSettings}
              onSpeechNetworkFallbackChange={(enabled) => patchAssistantSettings({ speechNetworkFallback: enabled })} /> : null}
            <section className="settings-panel__web" aria-labelledby="web-settings-title">
              <h3 id="web-settings-title">Web access</h3>
              <label className="shell__toggle settings-panel__interactive"><input type="checkbox" aria-label="Allow web access" checked={assistantSettings.webEnabled} disabled={instantControlsDisabled}
                onChange={(event) => patchAssistantSettings({ webEnabled: event.target.checked })} /> Web research</label>
              <p className="settings-panel__hint">{assistantSettings.webEnabled ? 'Native search may send question-derived queries and public-page requests to external providers: Exa is primary, with Parallel as fallback when needed. Delegated OpenCode routes may use their own search providers.' : 'Web is off: no web lookup or search fallback. Normal chat still uses the selected chat provider; answers are not web-verified.'}</p>
              <p className="settings-panel__hint">The selected model decides whether to use available web tools. Models without tool support can still chat but cannot browse automatically. Search activity appears in the response status when reported.</p>
            </section>
            {isAndroidRuntime ? <NewVoiceShortcutSettings /> : null}
            <UpdateSettings updates={appUpdates} installationDisabled={updateInstallationDisabled} />
            {!isAndroidRuntime && !isBrowserPreviewRuntime() ? <>
              <WakeDiagnostics disabled={!voiceInputReady(startupState) || updateRuntimeBlocked} />
              <WakeClipSettings />
            </> : null}
          </section>
        </div>
      ) : null
}, sameExternalView)

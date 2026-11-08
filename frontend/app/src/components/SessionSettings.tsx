import { memo } from 'react'
import { Button, Disclosure, IconButton, SelectSetting, SettingLink, SettingRow, SettingSection, SwitchField } from '../ui/controls'
import { UI_THEMES } from '../ui/theme'
import { isAndroidRuntime, isBrowserPreviewRuntime } from '../lib/tauri'
import { SETTINGS_DIALOG_ID } from '../lib/useSettingsNavigation'
import { SETTINGS_PAGE_TITLES } from '../lib/settingsPages'
import type { SettingsPage } from '../lib/settingsPages'
import { UI_TEXT_SIZE_LABELS, UI_TEXT_SIZE_PERCENTAGES, localInstantCanRetry, sameExternalView, usePresentation, voiceInputReady } from '../sessionPolicy'
import type { SessionViewProps } from '../sessionPolicy'
import { ChatGptAuth } from './ChatGptAuth'
import { InstantModelSelect } from './InstantModelSelect'
import { SpeechSettings } from './SpeechSettings'
import { UpdateSettings } from './UpdateSettings'
import { WakeClipSettings } from './WakeClipSettings'
import { WakeDiagnostics } from './WakeDiagnostics'

export const SessionSettings = memo(function SessionSettings({ session, view }: SessionViewProps) {
  const { uiTheme, assistantSettingsWriteError, uiThemeWriteError, uiTextSize, canDecreaseTextSize, canIncreaseTextSize,
    supportsDesktopRuntime, audioInputDeviceId, micStarting, updateRuntimeBlocked, audioInputDevices,
    autoStopOnSilence, startupState, voiceInputUnavailableReason, ttsEnabled, canToggleTts, androidVoiceStatus,
    androidSpeechStatusError, assistantSettings, instantControlsDisabled, instantAssistantOptions, assistantCapabilities,
    selectedInstantRetryable, selectedInstantOption, appUpdates, updateInstallationDisabled, customFailure } = usePresentation(session, view, 'settings')
  const { settingsOpen, settingsPage: page, settingsPanelRef, settingsCloseButtonRef, backSettings, openSettingsPage, onSettingsKeyDown } = view.navigation
  const { changeUiTheme, adjustUiTextSize, changeAudioInputDevice, setAutoStopOnSilence, toggleTts, patchAssistantSettings,
    changeInstant, clearCompletion, refreshChatGptAuth } = view.actions
  if (!settingsOpen) return null
  const size = `${UI_TEXT_SIZE_LABELS[uiTextSize]} (${UI_TEXT_SIZE_PERCENTAGES[uiTextSize]})`
  const link = (destination: Exclude<SettingsPage, 'overview'>, description: string) => <SettingLink
    key={destination} title={SETTINGS_PAGE_TITLES[destination]} description={description} aria-label={SETTINGS_PAGE_TITLES[destination]} data-settings-page={destination}
    onClick={(event) => openSettingsPage(destination, event.currentTarget)} />
  return <div className="settings-overlay" onMouseDown={backSettings} onKeyDown={onSettingsKeyDown}>
    <section ref={settingsPanelRef} className="settings-panel" data-settings-view={page} role="dialog" id={SETTINGS_DIALOG_ID}
      aria-modal="true" aria-labelledby="settings-title" onMouseDown={(event) => event.stopPropagation()}>
      <div className={`settings-panel__header${page !== 'overview' ? ' settings-panel__header--detail' : ''}`}>
        <IconButton ref={settingsCloseButtonRef} label={page === 'overview' ? 'Close settings' : 'Back to settings'}
          className="settings-panel__close" onClick={backSettings}>
          <svg viewBox="0 0 24 24" aria-hidden="true" className={page === 'overview' ? 'settings-panel__back-art' : ''}><path d="m12 4-8 8 8 8M4 12h16" /></svg>
          {page === 'overview' ? <svg viewBox="0 0 24 24" aria-hidden="true" className="settings-panel__close-art"><path d="m6 6 12 12M6 18 18 6" /></svg> : null}
        </IconButton>
        <h2 id="settings-title">{SETTINGS_PAGE_TITLES[page]}</h2>
        {assistantSettingsWriteError !== null || uiThemeWriteError !== null ? <div className="settings-panel__write-error" role="alert" tabIndex={0}>
          {assistantSettingsWriteError !== null ? <p className="shell__error">Assistant settings could not be saved: {assistantSettingsWriteError}</p> : null}
          {uiThemeWriteError !== null ? <p className="shell__error">Theme could not be saved: {uiThemeWriteError}</p> : null}
        </div> : null}
      </div>
      <div className="settings-panel__content">
        {page === 'overview' ? <>
          <SettingSection title="Appearance">
            <fieldset className="ui-choice-setting"><legend>Theme</legend><div className="ui-choice-setting__options">
              {UI_THEMES.map(({ value, label }) => <label key={value}>
                <input type="radio" name="uiTheme" value={value} checked={uiTheme === value} onChange={() => changeUiTheme(value)} />
                <span><span aria-hidden="true" className="ui-choice-setting__check">✓</span>{label}</span>
              </label>)}
            </div></fieldset>
            {link('text', size)}
          </SettingSection>
          <SettingSection title="Preferences">
            {link('models', selectedInstantOption?.label ?? 'Choose a model')}
            {link('account', !isBrowserPreviewRuntime() && startupState.kind === 'ready' && !assistantCapabilities.custom ? `Custom connection: ${customFailure?.reason ?? 'unavailable'}` : 'Sign-in and connection status')}
            {link('voice', 'Voice input, spoken replies and language')}
            {link('web', assistantSettings.webEnabled ? 'Allowed when the model needs it' : 'Off')}
            {link('updates', 'Update availability and automatic updates')}
            {!isAndroidRuntime ? link('advanced', 'Completion, speech diagnostics and saved clips') : null}
          </SettingSection>
        </> : null}
        {page === 'text' ? <SettingSection title="Conversation text" description="Adjust the app text size live. Your device’s text scaling still applies.">
          <div className="settings-text-preview" aria-label="Text size preview"><h3>A clear view of your conversation</h3><p>This is how messages and replies will read at your chosen size.</p></div>
          <div className="settings-text-controls" aria-label="Text size controls">
            <IconButton label="Decrease text size" disabled={!canDecreaseTextSize} onClick={() => adjustUiTextSize(-1)}>−</IconButton>
            <span className="settings-panel__size" aria-live="polite">{size}</span>
            <IconButton label="Increase text size" disabled={!canIncreaseTextSize} onClick={() => adjustUiTextSize(1)}>+</IconButton>
          </div>
        </SettingSection> : null}
        {page === 'models' ? <SettingSection title="Current model" description="The same model, reasoning and Fast controls are always available from chat.">
          <div className="settings-model-picker"><InstantModelSelect id="assistantInstantSelect" label="Model"
            value={assistantSettings.instant} options={instantAssistantOptions} disabled={instantControlsDisabled}
            canRetry={(value) => localInstantCanRetry(startupState, value)} onChange={(value) => void changeInstant(value)}
            fastMode={assistantSettings.fastMode} onFastModeChange={(fastMode) => patchAssistantSettings({ fastMode })} /></div>
          {selectedInstantRetryable && selectedInstantOption ? <Button aria-label="Retry local profile" disabled={instantControlsDisabled} onClick={() => void changeInstant(assistantSettings.instant)}>Retry local profile</Button> : null}
        </SettingSection> : null}
        {page === 'account' ? <SettingSection title="Connections">
          {!isBrowserPreviewRuntime() ? <ChatGptAuth onChanged={refreshChatGptAuth} /> : <SettingRow title="Account sign-in" description="Available in the packaged app." />}
          {startupState.kind === 'ready' && !assistantCapabilities.custom ? <SettingRow title="Custom connection" description={customFailure?.reason ?? 'Unavailable'} /> : null}
        </SettingSection> : null}
        {page === 'voice' ? <>
          {supportsDesktopRuntime || isAndroidRuntime ? <SettingSection title="Voice preferences">
            {supportsDesktopRuntime ? <>
              <SelectSetting label="Microphone" value={audioInputDeviceId ?? ''} disabled={micStarting || updateRuntimeBlocked} onChange={(event) => changeAudioInputDevice(event.target.value)}>
                <option value="">System selected microphone</option>{audioInputDevices.map((device) => <option key={device.deviceId} value={device.deviceId}>{device.label}</option>)}
              </SelectSetting>
              <SwitchField label="Stop listening after silence" description="Finish voice input automatically when you stop speaking." checked={autoStopOnSilence}
                disabled={!voiceInputReady(startupState) || updateRuntimeBlocked} onChange={(event) => setAutoStopOnSilence(event.target.checked)}
                aria-describedby={!voiceInputReady(startupState) ? 'voice-input-help' : undefined} />
              {!voiceInputReady(startupState) ? <p id="voice-input-help" className="ui-supporting-text">{voiceInputUnavailableReason}</p> : null}
            </> : null}
            <SwitchField label="Speak replies" description="Read assistant responses aloud." id="tts-toggle"
              checked={isAndroidRuntime ? (androidVoiceStatus?.ttsEnabled ?? ttsEnabled) : ttsEnabled}
              disabled={isAndroidRuntime ? (view.voice.pending || (androidVoiceStatus?.ttsEnabled !== true && (instantControlsDisabled || androidVoiceStatus?.ttsAvailable !== true))) : !canToggleTts}
              onChange={isAndroidRuntime ? () => view.voice.toggleSpeech() : (event) => { void toggleTts(event.target.checked) }} />
            <SpeechSettings android={isAndroidRuntime} androidStatus={androidVoiceStatus} statusError={androidSpeechStatusError} startupState={startupState}
              speechNetworkFallback={assistantSettings.speechNetworkFallback} speechSelection={assistantSettings} disabled={instantControlsDisabled}
              onSpeechSelectionChange={patchAssistantSettings}
              {...(supportsDesktopRuntime ? { onSpeechReconnect: () => { void toggleTts(true, true) } } : {})}
              onSpeechNetworkFallbackChange={(enabled) => patchAssistantSettings({ speechNetworkFallback: enabled })} />
          </SettingSection> : <SettingRow title="Voice and audio" description="Voice controls are available in the packaged app when supported by the device." />}
        </> : null}
        {page === 'web' ? <SettingSection title="Search preferences">
          <SwitchField label="Allow web research" aria-label="Allow web access" description="Questions may be sent to external search providers." checked={assistantSettings.webEnabled}
            disabled={instantControlsDisabled} onChange={(event) => patchAssistantSettings({ webEnabled: event.target.checked })} />
          <Disclosure title="How web research works">
            <p>{assistantSettings.webEnabled ? 'Native search uses Exa, with Parallel as fallback. Delegated OpenCode routes may use their own search providers. Queries and public-page requests may be sent to those providers.' : 'Web is off: no web lookup or search fallback. Normal chat still uses the selected chat provider; answers are not web-verified.'}</p>
            <p>The selected model decides when to use available web tools. Models without tool support can chat but cannot browse automatically. Reported search activity appears with the answer.</p>
          </Disclosure>
        </SettingSection> : null}
        {page === 'updates' ? <UpdateSettings updates={appUpdates} installationDisabled={updateInstallationDisabled} /> : null}
        {page === 'advanced' && !isAndroidRuntime ? <>
          <SettingSection title="Typing assistance"><SwitchField label="Completion" description="Suggest text while you type, when the local model is available."
            checked={assistantSettings.completion} disabled={instantControlsDisabled || (!assistantSettings.completion && !assistantCapabilities.qwenPrediction)}
            onChange={(event) => { if (!event.target.checked) clearCompletion(); patchAssistantSettings({ completion: event.target.checked }) }} /></SettingSection>
          {supportsDesktopRuntime ? <SettingSection title="Diagnostics"><WakeDiagnostics disabled={!voiceInputReady(startupState) || updateRuntimeBlocked} /><WakeClipSettings /></SettingSection> : null}
        </> : null}
      </div>
    </section>
  </div>
}, sameExternalView)

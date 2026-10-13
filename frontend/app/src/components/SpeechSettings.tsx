import { useId } from 'react'
import type { AndroidSpeechMetadata, AndroidSpeechVoiceOption, AndroidVoiceStatus } from '../lib/androidVoice'
import { visibleSpeechError, openSpeechServicesStore } from '../lib/androidVoice'
import { DEFAULT_SPEECH_SELECTION } from '../lib/assistantSettings'
import type { SpeechSelection } from '../lib/assistantSettings'
import type { SpeechServiceMetadata, StartupState } from '../types/chat'

interface Props {
  readonly android: boolean
  readonly androidStatus: AndroidVoiceStatus | null
  readonly statusError: string | null
  readonly startupState: StartupState
  readonly speechNetworkFallback?: boolean
  readonly disabled?: boolean
  readonly onSpeechNetworkFallbackChange?: (enabled: boolean) => void
  readonly speechSelection?: SpeechSelection
  readonly onSpeechSelectionChange?: (selection: SpeechSelection) => void
}

export function SpeechSettings({ android, androidStatus, statusError, startupState, speechNetworkFallback = true, disabled = false, onSpeechNetworkFallbackChange, speechSelection = DEFAULT_SPEECH_SELECTION, onSpeechSelectionChange }: Props) {
  const capabilities = startupState.kind === 'ready' || startupState.kind === 'warming_model'
    ? startupState.capabilities : []
  const speechServices = startupState.kind === 'ready' || startupState.kind === 'warming_model'
    ? startupState.speechServices : undefined
  const localOnlyAvailable = androidStatus?.speechOptions?.localOnlyRecognitionAvailable ?? (androidStatus?.localOnlyRecognitionAvailable === true)
  const unreportedStatus = statusError ? 'Native speech status is not reported. Keep Chat foreground to refresh native speech status.' : null
  return <section className="settings-panel__speech" aria-labelledby="speech-settings-title">
    <h3 id="speech-settings-title">Speech services</h3>
    {android ? <>
      {androidStatus?.missingSpeechServices ? <div className="settings-panel__missing-speech" role="alert">
        <p><strong>Speech services required:</strong> {androidStatus.speechRequirementMessage || 'Speech Recognition & Synthesis by Google is required for local voice dictation and speech output on this device.'}</p>
        <button type="button" className="shell__control settings-panel__interactive" onClick={() => void openSpeechServicesStore()}>Open Play Store</button>
      </div> : null}
      {(localOnlyAvailable || !speechNetworkFallback) && onSpeechNetworkFallbackChange ? <>
        <label className="shell__toggle settings-panel__interactive"><input type="checkbox" aria-label="Allow speech network fallback" checked={speechNetworkFallback} disabled={disabled} onChange={(event) => { if (!disabled && (event.target.checked || localOnlyAvailable)) onSpeechNetworkFallbackChange(event.target.checked) }} /> Allow speech network fallback</label>
        <p>Local-first speech requested{speechNetworkFallback ? '; network fallback permitted when supported.' : '; network fallback disabled where supported.'} This preference is not proof of offline processing; the provider may not report the actual route.</p>
      </> : null}
      {!localOnlyAvailable ? <p>{androidStatus === null ? statusError ? 'Dedicated on-device recognition capability is not reported.' : 'Checking dedicated on-device recognition capability.' : 'The dedicated on-device API is unavailable or not reported. Ordinary recognition providers may still work offline; this does not verify or enforce offline processing.'} {speechNetworkFallback ? 'Disabling speech network fallback is unavailable until the dedicated capability is reported.' : 'You can re-enable speech network fallback to allow provider-controlled recognition when available.'}</p> : null}
      {onSpeechSelectionChange ? <AndroidSpeechSelectors status={androidStatus} selection={speechSelection} fallback={speechNetworkFallback} localOnlyAvailable={localOnlyAvailable} disabled={disabled} onChange={onSpeechSelectionChange} /> : null}
      <h4>Voice input (STT)</h4>
      <p>{androidStatus?.reason || unreportedStatus || 'Checking native speech…'}</p>
      <p>{androidStatus?.recognitionRoute ?? 'Recognition provider, locale and processing route not reported'}</p>
      <AndroidSpeechIdentity metadata={androidStatus?.recognitionMetadata} />
      <h4>Speech output (TTS)</h4>
      <p>{androidStatus?.ttsRoute ?? 'Speech engine, voice and processing route not reported'}</p>
      <AndroidSpeechIdentity metadata={androidStatus?.ttsMetadata} />
      <p>{androidStatus === null ? unreportedStatus || 'Checking speech output…' : androidStatus.ttsAvailable ? 'Available' : 'Unavailable'}{androidStatus === null ? '' : androidStatus.ttsEnabled ? ' · Speak replies enabled' : ' · Speak replies disabled'}</p>
      {visibleSpeechError(androidStatus?.error) ? <p>{visibleSpeechError(androidStatus?.error)}</p> : null}
      {statusError ? <p>{statusError}</p> : null}
    </> : <>
      {(['parakeet', 'tts', 'wake_word', 'vad'] as const).map((id) => {
        const capability = capabilities.find((item) => item.id === id)
        const label = { parakeet: 'Voice input (STT)', tts: 'Speech output (TTS)', wake_word: 'Wake word', vad: 'Voice activity detection' }[id]
        return <div key={id}>
          <h4>{label}</h4>
          <p>{capability ? `${capability.state.replaceAll('_', ' ')} · ${capability.reason}` : 'Status not reported'}</p>
          {capability?.actualProvider ? <p>Reported execution provider: {capability.actualProvider.toUpperCase()}</p> : null}
          {capability?.state === 'available' ? <p>Local processing</p> : null}
          {id === 'parakeet' || id === 'tts' ? <SpeechIdentity metadata={id === 'parakeet' ? speechServices?.stt : speechServices?.tts} output={id === 'tts'} /> : null}
        </div>
      })}
      {(startupState.kind === 'ready' || startupState.kind === 'warming_model') && startupState.voiceInputError ? <p>{startupState.voiceInputError}</p> : null}
    </>}
  </section>
}

interface SpeechChoice { readonly value: string; readonly label: string; readonly disabled?: boolean }

function SpeechChoiceSelect({ label, value, choices, disabled, onChange, description }: {
  readonly label: string; readonly value: string; readonly choices: readonly SpeechChoice[]; readonly disabled: boolean; readonly onChange: (value: string) => void; readonly description?: string
}) {
  const id = useId()
  const options: readonly SpeechChoice[] = [{ value: '', label: 'Automatic (native selection)' }, ...choices]
  const reported = options.some((option) => option.value === value)
  return <><label className="speech-selection__field" htmlFor={id}>{label}
    <select id={id} aria-label={label} aria-describedby={description ? `${id}-help` : undefined} value={value} disabled={disabled} onChange={(event) => {
      const proposed = options.find((option) => option.value === event.target.value)
      if (!disabled && proposed && !proposed.disabled && proposed.value !== value) onChange(proposed.value)
    }}>
      {!reported ? <option value={value} disabled>{value} — not currently reported; choose Automatic to clear</option> : null}
      {options.map((option) => <option key={option.value} value={option.value} disabled={option.disabled ?? false}>{option.label}</option>)}
    </select>
  </label>{description ? <p id={`${id}-help`}>{description}</p> : null}</>
}

function AndroidSpeechSelectors({ status, selection, fallback, localOnlyAvailable, disabled, onChange }: {
  readonly status: AndroidVoiceStatus | null; readonly selection: SpeechSelection; readonly fallback: boolean; readonly localOnlyAvailable: boolean; readonly disabled: boolean; readonly onChange: (selection: SpeechSelection) => void
}) {
  const options = status?.speechOptions
  const languages = fallback ? options?.recognitionSupportedLanguages : localOnlyAvailable ? options?.recognitionInstalledLanguages : null
  const providers = [...new Set(options?.ttsProviders ?? [])].filter((provider) => provider !== '')
  const voices = new Map<string, AndroidSpeechVoiceOption>()
  for (const voice of options?.ttsVoices ?? []) {
    if (voice.provider !== selection.speechTtsProvider || voice.name === '') continue
    if (!voices.has(voice.name)) voices.set(voice.name, voice)
  }
  const modelId = useId()
  return <section aria-label="Requested speech configuration">
    <h4>Requested speech configuration</h4>
    <p>These are saved requests, not proof of the provider, locale, voice or processing route used by the last session.</p>
    <SpeechChoiceSelect label="Speech language" value={selection.speechLanguage} choices={[...new Set(languages ?? [])].filter((language) => language !== '').map((language) => ({ value: language, label: language }))} disabled={disabled} onChange={(speechLanguage) => onChange({ ...selection, speechLanguage })} />
    {languages == null ? <p>Language support is not reported under this policy. Only Automatic is available until supported choices are reported.</p> : null}
    <SpeechChoiceSelect label="TTS engine" value={selection.speechTtsProvider} choices={providers.map((provider) => ({ value: provider, label: provider }))} disabled={disabled} onChange={(speechTtsProvider) => onChange({ ...selection, speechTtsProvider, speechTtsVoice: '' })} description="Changing the TTS engine resets the requested voice to Automatic. Voices for another engine appear only after its native inventory is prepared." />
    <SpeechChoiceSelect label="TTS voice" value={selection.speechTtsVoice} choices={[...voices.values()].map((voice) => ({ value: voice.name, label: `${voice.name} · ${voice.language}${voice.networkRequired ? fallback ? ' · network required' : ' · unavailable: network fallback is off' : ' · SDK network not required'}`, disabled: voice.networkRequired && !fallback }))} disabled={disabled} onChange={(speechTtsVoice) => onChange({ ...selection, speechTtsVoice })} description="Only reported voices for the requested engine are listed. The native runtime validates language/voice compatibility. Clear a network voice to Automatic before disabling fallback. Automatic does not confirm which voice will be selected." />
    {(selection.speechLanguage !== '' || selection.speechTtsProvider !== '' || selection.speechTtsVoice !== '') ? <button type="button" className="shell__control settings-panel__interactive" aria-label="Use Automatic speech settings" disabled={disabled} onClick={() => { if (!disabled) onChange(DEFAULT_SPEECH_SELECTION) }}>Use Automatic speech settings</button> : null}
    <label className="speech-selection__field" htmlFor={modelId}>Speech model<select id={modelId} aria-label="Speech model" disabled><option>Unavailable — model identifiers not reported</option></select></label>
    <section aria-label="Last native-reported requested configuration">
      <h4>Last native-reported requested configuration</h4>
      <p>Language: {status?.speechLanguage || 'Automatic / not reported'}</p>
      <p>TTS engine: {status?.speechTtsProvider || 'Automatic / not reported'}</p>
      <p>TTS voice: {status?.speechTtsVoice || 'Automatic / not reported'}</p>
    </section>
  </section>
}

function AndroidSpeechIdentity({ metadata }: { readonly metadata: AndroidSpeechMetadata | null | undefined }) {
  return <>
    {metadata ? <>
      {metadata.configuredProvider !== undefined ? <p>Configured provider preference: {metadata.configuredProvider ?? 'Not reported'}</p> : null}
      {metadata.selectedProvider !== undefined ? <p>Selected provider: {metadata.selectedProvider ?? 'Not reported'}</p> : null}
      {metadata.reportedProvider !== undefined ? <p>Reported provider: {metadata.reportedProvider ?? 'Not reported'}</p> : null}
      <p>Requested locale: {metadata.requestedLanguage}</p>
      <p>Selected locale: {metadata.selectedLanguage ?? 'Not reported'}</p>
      <p>Provider-reported locale: {metadata.reportedLanguage ?? 'Not reported'}</p>
      <p>Installed locales: {metadata.installedLanguages === null ? 'Not reported' : metadata.installedLanguages.join(', ') || 'None reported'}</p>
      {metadata.supportedLanguages !== undefined ? <p>Supported locales: {metadata.supportedLanguages === null ? 'Not reported' : metadata.supportedLanguages.join(', ') || 'None reported'}</p> : null}
      {metadata.selectedVoice !== undefined ? <p>Selected voice profile: {metadata.selectedVoice ?? 'Not reported'}</p> : null}
      {metadata.networkRequired !== undefined ? <p>SDK voice network requirement: {metadata.networkRequired ? 'Required' : 'Not required'}</p> : null}
      {metadata.fallbackReason ? <p>Selection or fallback reason: {metadata.fallbackReason}</p> : null}
    </> : null}
    <p>{metadata?.actualProcessing === 'on_device_api' ? 'Reported processing route: On-device API' : 'Actual local/network processing: Not reported'}</p>
    <p>Underlying model identity: {metadata?.model ?? 'Not reported'}</p>
    <p>Offline trial verification reported by runtime: {metadata?.offlineTrialVerified === true ? 'Yes' : 'Not reported'}</p>
  </>
}

function SpeechIdentity({ metadata, output }: { readonly metadata: SpeechServiceMetadata | undefined; readonly output: boolean }) {
  return <>
    {metadata ? <>
      <p>Configured engine: {metadata.configuredEngine ?? 'Not configured'}</p>
      <p>Runtime engine: {metadata.runtimeEngine ?? 'Not loaded'}</p>
    </> : <p>Engine identity not reported</p>}
    <p>{metadata?.configuredModel ? `Configured model: ${metadata.configuredModel}` : 'Model identity not reported'}</p>
    {output ? <p>{metadata?.configuredVoice ? `Configured voice: ${metadata.configuredVoice}` : 'Voice identity not reported'}</p> : null}
    <p>{metadata?.configuredLocale ? `Configured locale: ${metadata.configuredLocale}` : 'Locale not reported'}</p>
  </>
}

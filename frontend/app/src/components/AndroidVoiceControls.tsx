import { useId } from 'react'
import type { ReactNode } from 'react'
import type { AndroidVoiceAdapterView } from '../lib/useAndroidVoiceAdapter'
import { visibleSpeechError } from '../lib/androidVoice'
import { ControlIcon } from './ControlIcon'
import type { ControlIconState } from './ControlIcon'
import { PhoneActionOverflow } from './PhoneActionOverflow'
import './AndroidVoiceControls.css'

interface Props {
  readonly voice: AndroidVoiceAdapterView
  readonly disabled: boolean
  readonly generationAvailable: boolean
  readonly compact?: boolean
  readonly modelControl?: ReactNode
  readonly secondaryControls?: ReactNode
}

export function AndroidVoiceControls({ voice, disabled, generationAvailable, compact = false, modelControl, secondaryControls }: Props) {
  const descriptionId = useId()
  const { status, pending, error, ownedSessionError, statusError, confirmationRequired, active,
    start, stop, finishDictation, finishShortcut, toggleSpeech } = voice
  const sessionError = visibleSpeechError(error ?? ownedSessionError)
  const progress = pending || status?.dictation === 'checking' ? 'Starting voice…'
    : status?.shortcutPhase === 'finishing' || status?.dictation === 'finishing' ? 'Finishing dictation…'
    : status?.ttsPhase === 'speaking' || status?.loop === 'speaking' ? 'Speaking…'
    : status?.loop === 'generating' ? 'Responding…'
    : status?.loop === 'between_turns' ? 'Ready for next turn…'
    : status?.readyForSpeech === true ? 'Listening…' : 'Starting voice…'
  const dictating = status?.mode === 'dictation' && ['checking', 'listening', 'finishing'].includes(status.dictation)
  const conversing = status?.mode === 'loop' && ['listening', 'generating', 'speaking', 'between_turns'].includes(status.loop)
  const shortcutCanFinish = status?.mode === 'shortcut' && ['listening', 'finishing'].includes(status.shortcutPhase ?? '')
  const speechDisabled = pending || (status?.ttsEnabled !== true && (disabled || status?.ttsAvailable !== true))
  if (compact) {
    const compactError = sessionError ?? statusError
    const voiceState: ControlIconState = compactError !== null ? 'error'
      : pending || status === null || status.dictation === 'checking' ? 'busy'
      : status.shortcutPhase === 'finishing' || status.dictation === 'finishing' ? 'finishing'
      : status.ttsPhase === 'speaking' || status.loop === 'speaking' ? 'speaking'
      : status.loop === 'generating' ? 'generating'
      : status.loop === 'between_turns' ? 'between-turns'
      : active ? 'listening' : status.available ? 'ready' : 'unavailable'
    const voiceDescription = compactError ?? (active ? progress : status === null ? 'Checking voice availability…' : status.available ? 'Ready to dictate' : status.reason || 'Native dictation is unavailable')
    const micLabel = 'Ask'
    const micDescription = `${voiceDescription}. One finalized utterance sends in the current chat, then idles`
    const conversationLabel = active ? 'Stop voice' : 'Talk'
    const conversationDescription = active ? `${voiceDescription}. Cancel recognition, request and playback; do not submit` : !generationAvailable ? `${voiceDescription}. Talk requires an available model` : `${voiceDescription}. Reopen listening after each reply in the current chat`
    const speakerState: ControlIconState = compactError !== null ? 'error' : pending || status === null ? 'busy' : status.ttsPhase === 'speaking' || status.loop === 'speaking' ? 'speaking' : status.ttsEnabled ? 'ready' : status.ttsAvailable ? 'off' : 'unavailable'
    const speakerDescription = compactError ?? (pending ? 'Voice controls busy' : status === null ? 'Checking speech availability…' : speakerState === 'speaking' ? 'Speaking…' : status.ttsEnabled ? 'Speak replies enabled' : status.ttsAvailable ? 'Speak replies off' : 'Native speech output is unavailable')
    const iconClass = 'shell__control shell__icon-control'
    return <div className="composer__actions android-voice-controls android-voice-controls--compact" role="group" aria-label="Android voice controls">
      <button type="button" className={iconClass} aria-label={micLabel} title={`${micLabel}: ${micDescription}`} aria-describedby={`${descriptionId}-mic`} aria-pressed={status?.mode === 'ask' && active} disabled={disabled || active || !generationAvailable || status?.available !== true} onClick={() => void start(false, undefined, 'single_turn')}><ControlIcon kind="mic" state={!active && !generationAvailable && compactError === null ? 'unavailable' : voiceState} /></button>
      <button type="button" className={iconClass} aria-label={conversationLabel} title={`${conversationLabel}: ${conversationDescription}`} aria-describedby={`${descriptionId}-conversation`} aria-pressed={conversing} disabled={!active && (disabled || !generationAvailable || status?.available !== true)} onClick={active ? stop : () => void start(true)}><ControlIcon kind={active ? 'stop' : 'conversation'} state={!active && !generationAvailable && compactError === null ? 'unavailable' : voiceState} /></button>
      <PhoneActionOverflow>
        <button type="button" className={iconClass} aria-label="Speak replies" title={`Speak replies: ${speakerDescription}`} aria-describedby={`${descriptionId}-speaker`} aria-pressed={status?.ttsEnabled ?? false} disabled={speechDisabled} onClick={toggleSpeech}><ControlIcon kind="speaker" state={speakerState} /></button>
        {modelControl}
        {secondaryControls}
      </PhoneActionOverflow>
      {confirmationRequired ? <span role="status" data-voice-confirmation>Tap Ask to start voice in this chat.</span> : null}
      <span className="sr-only" id={`${descriptionId}-mic`}>{micDescription}</span>
      <span className="sr-only" id={`${descriptionId}-conversation`}>{conversationDescription}</span>
      <span className="sr-only" id={`${descriptionId}-speaker`}>{speakerDescription}</span>
      {compactError ? <span className="sr-only" role="alert">{compactError}</span> : active ? <span className="sr-only" role="status">{progress}</span> : null}
    </div>
  }
  return <div className="composer__actions android-voice-controls" role="group" aria-label="Android voice controls">
    <button type="button" aria-label="Dictate" title="Dictate" aria-pressed={dictating} disabled={disabled || active || status?.available !== true} onClick={() => void start(false)}>Dictate</button>
    <button type="button" aria-label="Ask" title="Ask" aria-pressed={status?.mode === 'ask' && active} disabled={disabled || !generationAvailable || active || status?.available !== true} onClick={() => void start(false, undefined, 'single_turn')}>Ask</button>
    <button type="button" aria-label="Talk" title="Talk" aria-pressed={conversing} disabled={disabled || !generationAvailable || active || status?.available !== true} onClick={() => void start(true)}>Talk</button>
    <button type="button" aria-label="Speak replies" title="Speak replies" aria-pressed={status?.ttsEnabled ?? false} disabled={speechDisabled} onClick={toggleSpeech}>Speak replies</button>
    {modelControl}
    {confirmationRequired ? <span role="status" data-voice-confirmation>Tap Ask to start voice in this chat.</span> : null}
    {shortcutCanFinish ? <button type="button" disabled={status.shortcutPhase === 'finishing'} onClick={finishShortcut}>Stop &amp; send</button> : status?.dictation === 'listening' ? <button type="button" onClick={finishDictation}>Finish dictation</button> : null}
    {active ? <button type="button" onClick={stop}>Stop voice</button> : null}
    {sessionError ? <span role="alert">{sessionError}</span> : active ? <span role="status">{progress}</span> : null}
  </div>
}

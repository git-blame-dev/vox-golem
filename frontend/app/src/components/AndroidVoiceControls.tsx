import { createContext, useContext, useEffect, useId, useRef, useState } from 'react'
import type { MouseEvent, ReactNode } from 'react'
import type { AndroidVoiceAdapterView } from '../lib/useAndroidVoiceAdapter'
import { visibleSpeechError, openSpeechServicesStore } from '../lib/androidVoice'
import { ControlIcon } from './ControlIcon'
import type { ControlIconState } from './ControlIcon'
import './AndroidVoiceControls.css'

interface VoiceProps {
  readonly voice: AndroidVoiceAdapterView
  readonly disabled: boolean
  readonly generationAvailable: boolean
  readonly interactionBlocked?: boolean
}

function useVoiceControls({ voice, disabled, generationAvailable, interactionBlocked = false }: VoiceProps) {
  const descriptionId = useId()
  const [showMissingSpeechPrompt, setShowMissingSpeechPrompt] = useState(false)
  const [intentNotice, setIntentNotice] = useState<string | null>(null)
  const speechDialogRef = useRef<HTMLDialogElement>(null)
  const speechInvokerRef = useRef<HTMLElement | null>(null)
  useEffect(() => {
    if (!showMissingSpeechPrompt) return
    const dialog = speechDialogRef.current!
    const invoker = speechInvokerRef.current
    dialog.showModal()
    return () => { dialog.close(); if (invoker?.isConnected) invoker.focus() }
  }, [showMissingSpeechPrompt])
  const { status, pending, error, ownedSessionError, statusError, active } = voice
  const sessionError = visibleSpeechError(error ?? ownedSessionError)
  const isMissingSpeech = status?.missingSpeechServices === true
  const missingRequirementText = status?.speechRequirementMessage || 'Speech Recognition & Synthesis by Google is required for local STT or TTS on this device.'
  const compactError = sessionError ?? statusError
  const voiceState: ControlIconState = compactError !== null ? 'error'
    : pending || status === null || status.dictation === 'checking' ? 'busy'
    : status.shortcutPhase === 'finishing' || status.dictation === 'finishing' ? 'finishing'
    : status.ttsPhase === 'speaking' || status.loop === 'speaking' ? 'speaking'
    : status.loop === 'generating' ? 'generating'
    : status.loop === 'between_turns' ? 'between-turns'
    : active ? 'listening' : status.available ? 'ready' : isMissingSpeech ? 'error' : 'unavailable'
  const progress = pending || status?.dictation === 'checking' ? 'Starting voice…'
    : status?.shortcutPhase === 'finishing' || status?.dictation === 'finishing' ? 'Finishing dictation…'
    : status?.ttsPhase === 'speaking' || status?.loop === 'speaking' ? 'Speaking…'
    : status?.loop === 'generating' ? 'Responding…'
    : status?.loop === 'between_turns' ? 'Ready for next turn…'
    : status?.readyForSpeech === true ? 'Listening…' : 'Starting voice…'
  const rawVoiceDescription = compactError !== null ? compactError
    : active ? progress
    : status === null ? 'Checking voice availability…'
    : isMissingSpeech ? `${missingRequirementText} Tap to install from Play Store.`
    : status.available ? 'Ready to dictate'
    : status.reason || 'Native dictation is unavailable'
  const voiceDescription = active && status?.mode === 'loop' ? `${rawVoiceDescription}. Cancel recognition, request and playback; do not submit.` : `${rawVoiceDescription}.`
  const startDisabled = disabled || active || interactionBlocked || showMissingSpeechPrompt || (status?.available !== true && !isMissingSpeech)
  const withSpeechServices = (action: () => void, invoker: HTMLElement) => {
    if (isMissingSpeech) { speechInvokerRef.current = invoker; setShowMissingSpeechPrompt(true) }
    else action()
  }
  const start = (policy: 'single_turn', invoker: HTMLElement) => {
    if (startDisabled) return
    if (!generationAvailable) {
      setIntentNotice('Ask requires an available model.')
      return
    }
    setIntentNotice(null)
    withSpeechServices(() => { void voice.start(false, undefined, policy) }, invoker)
  }
  const speechDisabled = pending || interactionBlocked || (status?.ttsEnabled !== true && (disabled || (status?.ttsAvailable !== true && !isMissingSpeech)))
  const speakerState: ControlIconState = compactError !== null ? 'error' : pending || status === null ? 'busy' : status.ttsPhase === 'speaking' || status.loop === 'speaking' ? 'speaking' : status.ttsEnabled ? 'ready' : status.ttsAvailable ? 'off' : 'unavailable'
  const speakerDescription = compactError ?? (pending ? 'Voice controls busy' : status === null ? 'Checking speech availability…' : speakerState === 'speaking' ? 'Speaking…' : status.ttsEnabled ? 'Speak replies enabled' : status.ttsAvailable ? 'Speak replies off' : 'Native speech output is unavailable')
  const dialog = showMissingSpeechPrompt ? (
    <dialog ref={speechDialogRef} className="speech-prompt-dialog" aria-labelledby={`${descriptionId}-missing-title`} aria-describedby={`${descriptionId}-missing-description`}
      onCancel={(event) => { event.preventDefault(); setShowMissingSpeechPrompt(false) }}
      onClick={(event) => { if (event.target === event.currentTarget) setShowMissingSpeechPrompt(false) }}>
      <div className="speech-prompt-dialog__content">
        <h4 id={`${descriptionId}-missing-title`}>Speech Services Required</h4><p id={`${descriptionId}-missing-description`}>{missingRequirementText}</p>
        <div className="speech-prompt-dialog__actions">
          <button type="button" className="shell__control" onClick={() => { setShowMissingSpeechPrompt(false); void openSpeechServicesStore() }}>Open Play Store</button>
          <button type="button" autoFocus className="shell__control" onClick={() => setShowMissingSpeechPrompt(false)}>Cancel</button>
        </div>
      </div>
    </dialog>
  ) : null
  return { voice, descriptionId, start, startDisabled, compactError, sessionError, intentNotice, progress, voiceDescription, voiceState,
    speechDisabled, speakerState, speakerDescription, withSpeechServices, dialog, isMissingSpeech }
}
const VoiceControlsContext = createContext<ReturnType<typeof useVoiceControls> | null>(null)
function useControls() {
  const controls = useContext(VoiceControlsContext)
  if (controls === null) throw new Error('Voice controls require their shared provider')
  return controls
}
export function AndroidVoiceControlsProvider({ children, ...props }: VoiceProps & { readonly children: ReactNode }) {
  const controls = useVoiceControls(props)
  const dictating = !controls.voice.pending && controls.voice.status?.mode === 'dictation' && ['checking', 'listening', 'finishing'].includes(controls.voice.status.dictation)
  return <VoiceControlsContext value={controls}>
    {children}
    {controls.compactError !== null ? <span className="sr-only" role="alert">{controls.compactError}</span> : null}
    {controls.intentNotice !== null && !controls.voice.active ? <span className="sr-only" role="status">{controls.intentNotice}</span>
      : controls.voice.active ? <span className="sr-only" role="status">{dictating ? `Dictating · draft only · ${controls.progress}` : controls.progress}</span> : null}
    {controls.voice.confirmationRequired ? <span className="sr-only" data-voice-confirmation="true" role="status">Tap Ask to start voice input.</span> : null}
    {controls.dialog}
  </VoiceControlsContext>
}

function VoiceIntentButton() {
  const { voice, descriptionId, start, startDisabled, voiceDescription } = useControls()
  const { status, pending, active, stop, finishDictation, finishShortcut } = voice
  const manualListening = !pending && status?.mode === 'dictation' && status.dictation === 'listening'
  const manualFinishing = !pending && status?.mode === 'dictation' && status.dictation === 'finishing'
  const shortcutListening = !pending && status?.mode === 'shortcut' && status.shortcutPhase === 'listening'
  const shortcutFinishing = !pending && status?.mode === 'shortcut' && status.shortcutPhase === 'finishing'
  const label = !active ? 'Ask' : manualListening ? 'Finish dictation' : manualFinishing ? 'Finishing dictation' : shortcutListening ? 'Stop & send' : shortcutFinishing ? 'Finishing voice draft' : 'Stop voice'
  const voiceMode = !active ? 'idle' : status?.mode ?? 'ask'
  const activate = (event: MouseEvent<HTMLButtonElement>) => {
    if (event.button !== 0) return
    if (active) {
      if (manualListening) finishDictation()
      else if (shortcutListening) finishShortcut()
      else if (!manualFinishing && !shortcutFinishing) stop()
    } else {
      start('single_turn', event.currentTarget)
    }
  }
  return <>
    <button type="button" className="shell__control shell__icon-control" aria-label={label} title={`${label}: ${voiceDescription}`} aria-describedby={`${descriptionId}-mic`}
      data-voice-mode={voiceMode}
      disabled={active ? manualFinishing || shortcutFinishing : startDisabled}
      onClick={activate}
      onContextMenu={(event) => { if (event.button !== 0) event.preventDefault() }}>
      <ControlIcon kind={shortcutListening ? 'stop-send' : active ? 'stop' : 'mic'} />
    </button>
    <span id={`${descriptionId}-mic`} className="sr-only">{voiceDescription}</span>
  </>
}

export function AndroidComposerVoiceControl() {
  return <div className="android-voice-controls android-voice-controls--composer" role="group" aria-label="Android voice controls">
    <VoiceIntentButton />
  </div>
}

export function AndroidVoiceSecondaryControls() {
  const { descriptionId, speechDisabled, speakerState, speakerDescription, withSpeechServices, voice } = useControls()
  return <>
    {voice.active ? <button type="button" className="shell__control shell__icon-control" aria-label="Cancel voice" onClick={voice.stop}><ControlIcon kind="stop" /></button> : null}
    <button type="button" className="shell__control shell__icon-control" aria-label="Speak replies" title={`Speak replies: ${speakerDescription}`} aria-describedby={`${descriptionId}-speaker`} aria-pressed={voice.status?.ttsEnabled ?? false} disabled={speechDisabled}
      onClick={(event) => withSpeechServices(voice.toggleSpeech, event.currentTarget)}><ControlIcon kind="speaker" state={speakerState} /></button>
    <span className="sr-only" id={`${descriptionId}-speaker`}>{speakerDescription}</span>
  </>
}

import { createContext, useCallback, useContext, useEffect, useId, useRef, useState } from 'react'
import type { MouseEvent, PointerEvent, ReactNode } from 'react'
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
  const progress = pending || status?.dictation === 'checking' ? 'Starting voice…'
    : status?.shortcutPhase === 'finishing' || status?.dictation === 'finishing' ? 'Finishing dictation…'
    : status?.ttsPhase === 'speaking' || status?.loop === 'speaking' ? 'Speaking…'
    : status?.loop === 'generating' ? 'Responding…'
    : status?.loop === 'between_turns' ? 'Ready for next turn…'
    : status?.readyForSpeech === true ? 'Listening…' : 'Starting voice…'
  const compactError = sessionError ?? statusError
  const voiceState: ControlIconState = compactError !== null ? 'error'
    : pending || status === null || status.dictation === 'checking' ? 'busy'
    : status.shortcutPhase === 'finishing' || status.dictation === 'finishing' ? 'finishing'
    : status.ttsPhase === 'speaking' || status.loop === 'speaking' ? 'speaking'
    : status.loop === 'generating' ? 'generating'
    : status.loop === 'between_turns' ? 'between-turns'
    : active ? 'listening' : status.available ? 'ready' : isMissingSpeech ? 'error' : 'unavailable'
  const voiceDescription = compactError ?? (active ? progress : status === null ? 'Checking voice availability…' : isMissingSpeech ? `${missingRequirementText} Tap to install from Play Store.` : status.available ? 'Ready to dictate' : status.reason || 'Native dictation is unavailable')
  const startDisabled = disabled || active || interactionBlocked || showMissingSpeechPrompt || (status?.available !== true && !isMissingSpeech)
  const withSpeechServices = (action: () => void, invoker: HTMLElement) => {
    if (isMissingSpeech) { speechInvokerRef.current = invoker; setShowMissingSpeechPrompt(true) }
    else action()
  }
  const start = (policy: 'manual' | 'single_turn', invoker: HTMLElement) => {
    if (startDisabled) return
    if (policy === 'single_turn' && !generationAvailable) {
      setIntentNotice('Ask requires an available model. Use Dictate into draft to transcribe without sending.')
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
    speechDisabled, speakerState, speakerDescription, withSpeechServices, dialog }
}
const VoiceControlsContext = createContext<ReturnType<typeof useVoiceControls> | null>(null)
function useControls() {
  const controls = useContext(VoiceControlsContext)
  if (controls === null) throw new Error('Voice controls require their shared provider')
  return controls
}
export function AndroidVoiceControlsProvider({ children, ...props }: VoiceProps & { readonly children: ReactNode }) {
  const controls = useVoiceControls(props)
  return <VoiceControlsContext value={controls}>{children}{controls.dialog}</VoiceControlsContext>
}

/** Hold only selects intent. Native capture begins after a deliberate release inside. */
function VoiceIntentButton() {
  const { voice, descriptionId, start, startDisabled, voiceDescription, voiceState } = useControls()
  const [armed, setArmed] = useState(false)
  const [previousDisabled, setPreviousDisabled] = useState(startDisabled)
  if (previousDisabled !== startDisabled) {
    setPreviousDisabled(startDisabled)
    if (startDisabled) setArmed(false)
  }
  const gesture = useRef<{ id: number; x: number; y: number; armed: boolean; cancelled: boolean } | null>(null)
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null)
  const suppressUntil = useRef(0)
  const clearTimer = useCallback(() => { if (timer.current !== null) clearTimeout(timer.current); timer.current = null }, [])
  const cancel = useCallback(() => {
    clearTimer()
    if (gesture.current) { gesture.current.cancelled = true; suppressUntil.current = performance.now() + 1000 }
    setArmed(false)
  }, [clearTimer])
  useEffect(() => {
    if (startDisabled) {
      if (timer.current !== null) clearTimeout(timer.current)
      timer.current = null
      if (gesture.current) { gesture.current.cancelled = true; suppressUntil.current = performance.now() + 1000 }
    }
    return () => { if (timer.current !== null) clearTimeout(timer.current) }
  }, [startDisabled])
  useEffect(() => {
    const hidden = () => { if (document.visibilityState !== 'visible') cancel() }
    document.addEventListener('visibilitychange', hidden)
    return () => document.removeEventListener('visibilitychange', hidden)
  }, [cancel])
  const inside = (event: PointerEvent<HTMLButtonElement>) => {
    const bounds = event.currentTarget.getBoundingClientRect()
    return event.clientX >= bounds.left && event.clientX <= bounds.right && event.clientY >= bounds.top && event.clientY <= bounds.bottom
  }
  const { status, pending, active, stop, finishDictation, finishShortcut } = voice
  const manualListening = !pending && status?.mode === 'dictation' && status.dictation === 'listening'
  const manualFinishing = !pending && status?.mode === 'dictation' && status.dictation === 'finishing'
  const shortcutListening = !pending && status?.mode === 'shortcut' && status.shortcutPhase === 'listening'
  const shortcutFinishing = !pending && status?.mode === 'shortcut' && status.shortcutPhase === 'finishing'
  const label = !active ? 'Ask' : manualListening ? 'Finish dictation' : manualFinishing ? 'Finishing dictation' : shortcutListening ? 'Stop & send' : shortcutFinishing ? 'Finishing voice draft' : 'Stop voice'
  const description = `${voiceDescription}. ${active ? manualListening || manualFinishing ? 'Draft only; speech is not sent. Tap to finish, or use Cancel voice.' : shortcutListening || shortcutFinishing ? 'Stop & send submits this shortcut draft. Cancel voice does not send.' : 'Cancel recognition, request and playback; do not submit' : 'Tap to send existing draft plus speech automatically. Hold, then release inside to dictate into the draft only; tap again to finish.'}`
  const activate = (event: MouseEvent<HTMLButtonElement>) => {
    if (event.detail > 0 && performance.now() < suppressUntil.current) { suppressUntil.current = 0; return }
    if (active) { if (manualListening) finishDictation(); else if (shortcutListening) finishShortcut(); else if (!manualFinishing && !shortcutFinishing) stop() }
    else start('single_turn', event.currentTarget)
  }
  return <>
    <button type="button" className="shell__control shell__icon-control" aria-label={label} title={`${label}: ${description}`} aria-describedby={`${descriptionId}-mic`}
      data-voice-mode={active ? status?.mode === 'dictation' ? 'dictate' : status?.mode : armed ? 'dictate-armed' : 'idle'} disabled={active ? manualFinishing || shortcutFinishing : startDisabled}
      onPointerDown={(event) => {
        if (event.button !== 0 || !event.isPrimary) return
        clearTimer(); gesture.current = null; setArmed(false)
        suppressUntil.current = 0
        if (active || startDisabled) return
        gesture.current = { id: event.pointerId, x: event.clientX, y: event.clientY, armed: false, cancelled: false }
        timer.current = setTimeout(() => { timer.current = null; if (gesture.current && !gesture.current.cancelled) { gesture.current.armed = true; setArmed(true) } }, 380)
      }}
      onPointerMove={(event) => { const current = gesture.current; if (current?.id === event.pointerId && (Math.hypot(event.clientX - current.x, event.clientY - current.y) > 12 || !inside(event))) cancel() }}
      onPointerLeave={cancel} onPointerCancel={cancel} onLostPointerCapture={cancel} onBlur={cancel}
      onPointerUp={(event) => {
        const current = gesture.current
        if (!current || current.id !== event.pointerId) return
        clearTimer(); gesture.current = null; setArmed(false)
        if (current.cancelled || !inside(event)) { suppressUntil.current = performance.now() + 1000; return }
        if (current.armed) { suppressUntil.current = performance.now() + 1000; start('manual', event.currentTarget) }
      }}
      onContextMenu={(event) => { if (gesture.current || performance.now() < suppressUntil.current) event.preventDefault() }} onClick={activate}>
      <ControlIcon kind={!active ? 'mic' : manualListening ? 'finish' : shortcutListening ? 'stop-send' : 'stop'} state={voiceState} />
    </button>
    <span id={`${descriptionId}-mic`} className="sr-only">{description}</span>
    {armed ? <span className="android-voice-controls__armed" role="status">Release to dictate</span> : null}
  </>
}

export function AndroidComposerVoiceControl() {
  const { voice, compactError, intentNotice, progress } = useControls()
  const dictating = !voice.pending && voice.status?.mode === 'dictation' && ['checking', 'listening', 'finishing'].includes(voice.status.dictation)
  return <div className="android-voice-controls android-voice-controls--composer" role="group" aria-label="Android voice controls">
    <VoiceIntentButton />
    {voice.confirmationRequired ? <span role="status" data-voice-confirmation>Tap Ask to start voice in this chat.</span> : null}
    {compactError ? <span className="sr-only" role="alert">{compactError}</span> : null}
    {intentNotice && !voice.active ? <span role="status">{intentNotice}</span> : voice.active ? <span className="android-voice-controls__mode" role="status">{dictating ? `Dictating · draft only · ${progress}` : progress}</span> : null}
  </div>
}

export function AndroidVoiceSecondaryControls() {
  const { voice, descriptionId, start, startDisabled, speechDisabled, speakerState, speakerDescription, withSpeechServices } = useControls()
  return <>
    <button type="button" className="shell__control" aria-label="Dictate into draft" disabled={startDisabled} onClick={(event) => start('manual', event.currentTarget)}><ControlIcon kind="mic" /><span className="android-voice-controls__action-label">Dictate into draft</span></button>
    {voice.active ? <button type="button" className="shell__control" aria-label="Cancel voice" onClick={voice.stop}><ControlIcon kind="stop" /><span className="android-voice-controls__action-label">Cancel voice</span></button> : null}
    <button type="button" className="shell__control" aria-label="Speak replies" title={`Speak replies: ${speakerDescription}`} aria-describedby={`${descriptionId}-speaker`} aria-pressed={voice.status?.ttsEnabled ?? false} disabled={speechDisabled}
      onClick={(event) => withSpeechServices(voice.toggleSpeech, event.currentTarget)}><ControlIcon kind="speaker" state={speakerState} /><span className="android-voice-controls__action-label">Speak replies</span></button>
    <span className="sr-only" id={`${descriptionId}-speaker`}>{speakerDescription}</span>
  </>
}

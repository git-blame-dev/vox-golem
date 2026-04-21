import { useEffect, useMemo, useRef, useState } from 'react'
import type { FormEvent, KeyboardEvent } from 'react'
import { ChatBubble } from './components/ChatBubble'
import { playCue } from './lib/audioCues'
import { shouldSubmitComposer } from './lib/composer'
import { startLiveAudioSource } from './lib/liveAudioSource'
import type { LiveAudioSource } from './lib/liveAudioSource'
import { executePrompt } from './lib/promptExecution'
import {
  ingestAudioFrame,
  invokeRuntimeControl,
} from './lib/runtimeControl'
import type { RuntimeControlArgs } from './lib/runtimeControl'
import { DEFAULT_CUE_ASSET_PATHS, loadStartupState } from './lib/startupState'
import {
  createVoiceActivityState,
  syncVoiceActivityState,
  updateVoiceActivityState,
} from './lib/voiceActivity'
import { createExecutionMessages, getInitialMessages } from './state/appShell'
import { cueForTransition, transitionRuntimeStatus } from './state/runtimeMachine'
import type {
  BackendRuntimePhase,
  ChatMessage,
  RuntimeControlResult,
  RuntimeStatus,
  StartupState,
} from './types/chat'
import './App.css'

function App() {
  const [startupState, setStartupState] = useState<StartupState>({ kind: 'loading' })
  const [runtimeStatus, setRuntimeStatus] = useState<RuntimeStatus>('initializing')
  const [composerValue, setComposerValue] = useState('')
  const [autoStopOnSilence, setAutoStopOnSilence] = useState(true)
  const [micStarting, setMicStarting] = useState(false)
  const [micActive, setMicActive] = useState(false)
  const [messages, setMessages] = useState<readonly ChatMessage[]>(() =>
    getInitialMessages(),
  )
  const liveAudioSourceRef = useRef<LiveAudioSource | null>(null)
  const appActiveRef = useRef(true)
  const autoStopOnSilenceRef = useRef(true)
  const micAutoStartedRef = useRef(false)
  const runtimeStatusRef = useRef<RuntimeStatus>('initializing')
  const startupStateRef = useRef<StartupState>({ kind: 'loading' })
  const voiceActivityStateRef = useRef(createVoiceActivityState())

  useEffect(() => {
    let active = true

    void loadStartupState().then((nextState) => {
      if (!active) {
        return
      }

      startupStateRef.current = nextState
      setStartupState(nextState)
      runtimeStatusRef.current = nextState.kind === 'ready' ? toRuntimeStatus(nextState.runtimePhase) : 'error'
      setRuntimeStatus(nextState.kind === 'ready' ? toRuntimeStatus(nextState.runtimePhase) : 'error')

      if (nextState.kind === 'ready') {
        setMessages((currentMessages) => [
          ...currentMessages,
          {
            id: `system-startup-ready-${Date.now()}`,
            role: 'system',
            content:
              `Startup ready: runtime=${nextState.runtimePhase}, ` +
              `startCue=${nextState.cueAssetPaths.startListening}, ` +
              `stopCue=${nextState.cueAssetPaths.stopListening}`,
          },
        ])
      }

      if (nextState.kind === 'error') {
        setMessages((currentMessages) => [
          ...currentMessages,
          {
            id: `system-startup-error-${Date.now()}`,
            role: 'system',
            content: `Startup error: ${nextState.message}`,
          },
        ])
      }
    })

    return () => {
      active = false
    }
  }, [])

  useEffect(() => {
    return () => {
      appActiveRef.current = false
      liveAudioSourceRef.current?.stop()
      liveAudioSourceRef.current = null
    }
  }, [])

  const canSend = useMemo(
    () =>
      startupState.kind === 'ready' &&
      (runtimeStatus === 'sleeping' || runtimeStatus === 'result_ready') &&
      composerValue.trim().length > 0,
    [composerValue, runtimeStatus, startupState.kind],
  )

  const canStartListening =
    startupState.kind === 'ready' &&
    startupState.voiceInputAvailable &&
    !micActive &&
    (runtimeStatus === 'sleeping' || runtimeStatus === 'result_ready')
  const canMarkSilence =
    startupState.kind === 'ready' && startupState.voiceInputAvailable && runtimeStatus === 'listening'
  const canMarkResultReady =
    startupState.kind === 'ready' &&
    (runtimeStatus === 'processing' || runtimeStatus === 'executing')
  const canResetToIdle =
    startupState.kind === 'ready' &&
    (runtimeStatus === 'result_ready' || runtimeStatus === 'error')
  const canToggleMic =
    startupState.kind === 'ready' && startupState.voiceInputAvailable && !micStarting
  const cueAssetPaths =
    startupState.kind === 'ready'
      ? startupState.cueAssetPaths
      : DEFAULT_CUE_ASSET_PATHS

  useEffect(() => {
    autoStopOnSilenceRef.current = autoStopOnSilence
  }, [autoStopOnSilence])

  const runtimeLabel = getRuntimeLabel(runtimeStatus)
  const runtimeDetail = getRuntimeDetail(runtimeStatus, startupState, micActive, autoStopOnSilence)

  const reportCuePlaybackError = (cueType: 'start_listening' | 'stop_listening', error: unknown): void => {
    const message = error instanceof Error ? error.message : 'Unknown cue playback error'

    console.error('[cue] playback failure', {
      cueType,
      cueAssetPaths,
      runtimeStatus: runtimeStatusRef.current,
      error,
    })

    setMessages((currentMessages) => [
      ...currentMessages,
      {
        id: `system-cue-error-${Date.now()}`,
        role: 'system',
        content:
          `Cue playback error: ${message} ` +
          `[cue=${cueType}, startCue=${cueAssetPaths.startListening}, stopCue=${cueAssetPaths.stopListening}]`,
      },
    ])
  }

  const applyTransition = (
    previousStatus: RuntimeStatus,
    event: Parameters<typeof transitionRuntimeStatus>[1],
  ): RuntimeStatus => {
    const nextStatus = transitionRuntimeStatus(previousStatus, event)

    if (nextStatus === previousStatus) {
      return previousStatus
    }

    runtimeStatusRef.current = nextStatus
    setRuntimeStatus(nextStatus)

    if (nextStatus !== 'listening') {
      voiceActivityStateRef.current = createVoiceActivityState()
    }

    const cueType = cueForTransition(previousStatus, nextStatus)

    if (cueType !== null) {
      void playCue(cueType, cueAssetPaths).catch((error: unknown) => {
        console.error('[cue] playback failure during transition', {
          cueType,
          cueAssetPaths,
          runtimeStatusBefore: previousStatus,
          runtimeStatusAfter: nextStatus,
          error,
        })

        applyTransition(nextStatus, 'fail')
        reportCuePlaybackError(cueType, error)
      })
    }

    return nextStatus
  }

  const applyRuntimeStatus = (nextStatus: RuntimeStatus): RuntimeStatus => {
    const previousStatus = runtimeStatusRef.current

    if (nextStatus === previousStatus) {
      return previousStatus
    }

    runtimeStatusRef.current = nextStatus
    setRuntimeStatus(nextStatus)

    if (nextStatus !== 'listening') {
      voiceActivityStateRef.current = createVoiceActivityState()
    }

    const cueType = cueForTransition(previousStatus, nextStatus)

    if (cueType !== null) {
      void playCue(cueType, cueAssetPaths).catch((error: unknown) => {
        console.error('[cue] playback failure while applying runtime status', {
          cueType,
          cueAssetPaths,
          runtimeStatusBefore: previousStatus,
          runtimeStatusAfter: nextStatus,
          error,
        })

        enterRuntimeError()
        reportCuePlaybackError(cueType, error)
      })
    }

    return nextStatus
  }

  const enterRuntimeError = (): void => {
    runtimeStatusRef.current = 'error'
    voiceActivityStateRef.current = createVoiceActivityState()
    setRuntimeStatus('error')
  }

  const transitionFromCurrentStatus = (
    event: Parameters<typeof transitionRuntimeStatus>[1],
  ): void => {
    if (startupStateRef.current.kind !== 'ready') {
      return
    }

    applyTransition(runtimeStatusRef.current, event)
  }

  const applyRuntimeControlResult = (
    runtimePhase: RuntimeControlResult,
    options: {
      readonly quiet?: boolean
    } = {},
  ): void => {
    const { quiet = false } = options
    applyRuntimeStatus(toRuntimeStatus(runtimePhase.runtimePhase))

    if (quiet) {
      return
    }

    const nextMessages: ChatMessage[] = []

    if (runtimePhase.transcriptionReadySamples !== null) {
      nextMessages.push({
        id: `system-transcription-ready-${Date.now()}`,
        role: 'system',
        content: `transcription_ready:\n${runtimePhase.transcriptionReadySamples} samples captured`,
      })
    }

    if (runtimePhase.transcriptText !== null) {
      nextMessages.push({
        id: `system-transcript-${Date.now()}`,
        role: 'system',
        content: `transcript:\n${runtimePhase.transcriptText}`,
      })
    }

    setMessages((currentMessages) => [...currentMessages, ...nextMessages])
  }

  const syncRuntimeControl = async (
    command:
      | 'begin_listening'
      | 'record_speech_activity'
      | 'mark_silence'
      | 'mark_result_ready'
      | 'reset_session',
    options: {
      readonly args?: RuntimeControlArgs
      readonly fallbackEvent?: Parameters<typeof transitionRuntimeStatus>[1]
      readonly quiet?: boolean
    } = {},
  ): Promise<RuntimeControlResult | null> => {
    if (startupStateRef.current.kind !== 'ready') {
      return null
    }

    const { args, fallbackEvent, quiet } = options

    try {
      const runtimePhase = await invokeRuntimeControl(command, args)

      if (runtimePhase === null) {
        if (fallbackEvent !== undefined) {
          transitionFromCurrentStatus(fallbackEvent)
        }

        return null
      }

      applyRuntimeControlResult(runtimePhase, quiet === undefined ? {} : { quiet })
      return runtimePhase
    } catch (error) {
      const message = toDisplayErrorMessage(error)

      recoverFromRuntimeControlError(command)
      setMessages((currentMessages) => [
        ...currentMessages,
        {
          id: `system-runtime-control-error-${Date.now()}`,
          role: 'system',
          content: `Runtime control error (${command}): ${message}`,
        },
      ])
      return null
    }
  }

  const recoverFromRuntimeControlError = (
    command: 'begin_listening' | 'record_speech_activity' | 'mark_silence' | 'mark_result_ready' | 'reset_session',
  ): void => {
    if (command === 'mark_silence') {
      applyRuntimeStatus('sleeping')
      return
    }

    enterRuntimeError()
  }

  const runPrompt = async (
    prompt: string,
    source: 'typed' | 'voice',
  ): Promise<void> => {
    if (startupStateRef.current.kind !== 'ready') {
      return
    }

    const trimmedPrompt = prompt.trim()

    if (trimmedPrompt.length === 0) {
      return
    }

    const currentStatus = runtimeStatusRef.current
    const executingStatus = applyTransition(currentStatus, 'submit_prompt')

    if (executingStatus === currentStatus) {
      return
    }

    setMessages((currentMessages) => [
      ...currentMessages,
      {
        id: `user-${Date.now()}`,
        role: 'user',
        content: trimmedPrompt,
      },
    ])

    if (source === 'typed') {
      setComposerValue('')
    }

    try {
      const result = await executePrompt(trimmedPrompt)
      const nextMessages = createExecutionMessages(result)

      setMessages((currentMessages) => [...currentMessages, ...nextMessages])
      applyRuntimeStatus(toRuntimeStatus(result.runtimePhase))

      if (liveAudioSourceRef.current !== null && result.runtimePhase === 'result_ready') {
        await syncRuntimeControl('reset_session', {
          fallbackEvent: 'reset_to_sleeping',
          quiet: true,
        })
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : 'Prompt execution failed'

      applyTransition(executingStatus, 'fail')
      setMessages((currentMessages) => [
        ...currentMessages,
        {
          id: `system-exec-error-${Date.now()}`,
          role: 'system',
          content: `Execution error: ${message}`,
        },
      ])
    }
  }

  const maybeRunVoiceTranscript = (
    runtimePhase: RuntimeControlResult | null,
  ): void => {
    if (runtimePhase === null || runtimePhase.transcriptText === null) {
      return
    }

    void runPrompt(runtimePhase.transcriptText, 'voice')
  }

  const handleMarkSilence = async (): Promise<void> => {
    try {
      await playCue('stop_listening', cueAssetPaths)
    } catch (error) {
      reportCuePlaybackError('stop_listening', error)
    }

    const runtimePhase = await syncRuntimeControl('mark_silence', {
      fallbackEvent: 'end_listening',
    })

    maybeRunVoiceTranscript(runtimePhase)
  }

  const stopLiveAudio = (content: string): void => {
    liveAudioSourceRef.current?.stop()
    liveAudioSourceRef.current = null
    voiceActivityStateRef.current = createVoiceActivityState()
    setMicStarting(false)
    setMicActive(false)
    setMessages((currentMessages) => [
      ...currentMessages,
      {
        id: `system-live-audio-${Date.now()}`,
        role: 'system',
        content,
      },
    ])
  }

  const reportLiveAudioError = (error: unknown): void => {
    const message = error instanceof Error ? error.message : 'Live audio capture failed'

    stopLiveAudio(`live_audio_error:\n${message}`)
  }

  const startMic = async (): Promise<void> => {
    if (startupStateRef.current.kind !== 'ready' || liveAudioSourceRef.current !== null || micStarting) {
      return
    }

    setMicStarting(true)

    try {
      const liveAudioSource = await startLiveAudioSource({
        onFrame: async (frame) => {
          try {
            const nowMs = Date.now()
            const status = await ingestAudioFrame(frame)

            if (status !== null) {
              const nextStatus = toRuntimeStatus(status.runtimePhase)

              applyRuntimeStatus(nextStatus)
              voiceActivityStateRef.current = syncVoiceActivityState(
                voiceActivityStateRef.current,
                nextStatus,
                status.lastActivityMs,
              )

              if (nextStatus === 'listening' && autoStopOnSilenceRef.current) {
                const voiceActivityUpdate = updateVoiceActivityState(voiceActivityStateRef.current, nowMs)

                voiceActivityStateRef.current = voiceActivityUpdate.state

                if (voiceActivityUpdate.shouldMarkSilence) {
                  await handleMarkSilence()
                }
              }
            }
          } catch (error) {
            enterRuntimeError()
            throw error
          }
        },
        onError: reportLiveAudioError,
      })

      if (!appActiveRef.current) {
        liveAudioSource.stop()
        return
      }

      liveAudioSourceRef.current = liveAudioSource
      setMicStarting(false)
      setMicActive(true)
      setMessages((currentMessages) => [
        ...currentMessages,
        {
          id: `system-live-audio-${Date.now()}`,
          role: 'system',
          content: 'live_audio:\ndefault microphone started',
        },
      ])
    } catch (error) {
      if (!appActiveRef.current) {
        return
      }

      setMicStarting(false)
      reportLiveAudioError(error)
    }
  }

  const toggleMic = (): void => {
    if (micActive) {
      stopLiveAudio('live_audio:\ndefault microphone stopped')
      return
    }

    void startMic()
  }

  useEffect(() => {
    if (
      startupState.kind !== 'ready' ||
      !startupState.voiceInputAvailable ||
      micActive ||
      micStarting ||
      micAutoStartedRef.current
    ) {
      return
    }

    micAutoStartedRef.current = true
    void startMic()
  }, [micActive, micStarting, startupState])

  const sendPrompt = async (): Promise<void> => {
    if (startupState.kind !== 'ready') {
      return
    }

    const prompt = composerValue.trim()

    if (prompt.length === 0) {
      return
    }

    await runPrompt(prompt, 'typed')
  }

  const onSubmit = (event: FormEvent<HTMLFormElement>): void => {
    event.preventDefault()
    void sendPrompt()
  }

  const onComposerKeyDown = (
    event: KeyboardEvent<HTMLTextAreaElement>,
  ): void => {
    if (!shouldSubmitComposer(event.key, event.shiftKey)) {
      return
    }

    event.preventDefault()
    void sendPrompt()
  }

  return (
    <div className="shell">
      <header className="shell__header">
        <p className="shell__eyebrow">VoxGolem</p>
        <h1>Windows Tauri MVP Shell</h1>
        <p className="shell__status">Status: {runtimeLabel}</p>
        <p className="shell__phase">{runtimeDetail}</p>
        <div className="shell__badges" aria-label="Runtime badges">
          <span className="shell__badge">Mic {micActive ? 'on' : 'off'}</span>
          <span className="shell__badge">
            Voice {startupState.kind === 'ready' && startupState.voiceInputAvailable ? 'ready' : 'limited'}
          </span>
          <span className="shell__badge">Auto stop {autoStopOnSilence ? 'on' : 'off'}</span>
        </div>
        {startupState.kind === 'error' ? (
          <p className="shell__error">Startup error: {startupState.message}</p>
        ) : null}
        {startupState.kind === 'ready' && !startupState.voiceInputAvailable ? (
          <p className="shell__error">
            Voice input unavailable: {startupState.voiceInputError ?? 'Parakeet failed to initialize'}
          </p>
        ) : null}
        <div className="shell__controls" role="group" aria-label="Runtime controls">
          <button
            type="button"
            className="shell__control"
            onClick={toggleMic}
            disabled={!canToggleMic}
          >
            {micStarting ? 'Starting mic...' : micActive ? 'Stop mic' : 'Start mic'}
          </button>
          <button
            type="button"
            className="shell__control"
            onClick={() => {
              void handleMarkSilence()
            }}
            disabled={!canMarkSilence}
          >
            Stop listening and process
          </button>
          <button
            type="button"
            className="shell__control"
            onClick={() => {
              void syncRuntimeControl(
                'reset_session',
                {
                  fallbackEvent:
                    runtimeStatus === 'error' ? 'recover_from_error' : 'reset_to_sleeping',
                },
              )
            }}
            disabled={!canResetToIdle}
          >
            Reset to idle
          </button>
        </div>
        <label className="shell__toggle">
          <input
            type="checkbox"
            checked={autoStopOnSilence}
            onChange={(event) => setAutoStopOnSilence(event.target.checked)}
            disabled={startupState.kind !== 'ready' || !startupState.voiceInputAvailable}
          />
          <span>Auto stop on silence</span>
        </label>
        <details className="shell__manual-controls">
          <summary>Manual fallback controls</summary>
          <div className="shell__controls" role="group" aria-label="Manual fallback controls">
            <button
              type="button"
              className="shell__control"
              onClick={() => {
                void syncRuntimeControl('begin_listening', { fallbackEvent: 'begin_listening' })
              }}
              disabled={!canStartListening}
            >
              Start listening
            </button>
            <button
              type="button"
              className="shell__control"
              onClick={() => {
                void syncRuntimeControl('mark_result_ready', { fallbackEvent: 'response_ready' })
              }}
              disabled={!canMarkResultReady}
            >
              Mark result ready
            </button>
          </div>
        </details>
      </header>

      <main className="conversation" aria-live="polite">
        {messages.map((message) => (
          <ChatBubble key={message.id} message={message} />
        ))}
      </main>

      <form className="composer" onSubmit={onSubmit}>
        <label className="composer__label" htmlFor="promptComposer">
          Prompt
        </label>
        <textarea
          id="promptComposer"
          className="composer__input"
          value={composerValue}
          onChange={(event) => setComposerValue(event.target.value)}
          onKeyDown={onComposerKeyDown}
          placeholder="Type a prompt..."
          rows={3}
        />
        <div className="composer__actions">
          <span className="composer__hint">Enter to send, Shift+Enter for newline</span>
          <button type="submit" className="composer__button" disabled={!canSend}>
            Send
          </button>
        </div>
      </form>
    </div>
  )
}

function toRuntimeStatus(runtimePhase: BackendRuntimePhase): RuntimeStatus {
  return runtimePhase
}

function toDisplayErrorMessage(error: unknown): string {
  if (error instanceof Error) {
    return error.message
  }

  if (typeof error === 'string') {
    return error
  }

  return String(error)
}

function getRuntimeLabel(runtimeStatus: RuntimeStatus): string {
  switch (runtimeStatus) {
    case 'initializing':
      return 'Starting up'
    case 'sleeping':
      return 'Waiting'
    case 'listening':
      return 'Listening'
    case 'processing':
      return 'Transcribing'
    case 'executing':
      return 'Running'
    case 'result_ready':
      return 'Ready'
    case 'error':
      return 'Error'
  }
}

function getRuntimeDetail(
  runtimeStatus: RuntimeStatus,
  startupState: StartupState,
  micActive: boolean,
  autoStopOnSilence: boolean,
): string {
  if (startupState.kind === 'error') {
    return 'Configuration or startup failed before voice services could begin.'
  }

  if (startupState.kind === 'ready' && !startupState.voiceInputAvailable) {
    return 'Typed prompts still work, but local voice input needs the configured wake-word, VAD, and STT assets.'
  }

  switch (runtimeStatus) {
    case 'initializing':
      return 'Loading runtime services and local voice assets.'
    case 'sleeping':
      return micActive
        ? 'Mic is live and waiting for the wake word or manual fallback controls.'
        : 'Start the mic to listen hands-free, or use typed prompts below.'
    case 'listening':
      return autoStopOnSilence
        ? 'Speech is being captured. Stop talking and the assistant will transcribe automatically.'
        : 'Speech is being captured. Use Stop listening and process when you are done talking.'
    case 'processing':
      return 'The captured voice turn is being transcribed locally before execution.'
    case 'executing':
      return 'OpenCode is running the request now.'
    case 'result_ready':
      return 'The last result finished successfully. You can speak again or send another typed prompt.'
    case 'error':
      return 'The last voice or execution step failed. Reset to idle to recover.'
  }
}

export default App

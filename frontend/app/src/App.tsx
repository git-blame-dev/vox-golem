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
import {
  DEFAULT_CUE_ASSET_PATHS,
  DEFAULT_SILENCE_TIMEOUT_MS,
  isStartupStateSettled,
  loadStartupState,
} from './lib/startupState'
import { invokeTauriCommand } from './lib/tauri'
import { createVoiceTelemetryRecorder } from './lib/voiceTelemetry'
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
  ResponseProfile,
  RuntimeStatus,
  StartupState,
} from './types/chat'
import './App.css'

function App() {
  const [startupState, setStartupState] = useState<StartupState>({ kind: 'loading' })
  const [runtimeStatus, setRuntimeStatus] = useState<RuntimeStatus>('initializing')
  const [composerValue, setComposerValue] = useState('')
  const [autoStopOnSilence, setAutoStopOnSilence] = useState(true)
  const [wakeConfidence, setWakeConfidence] = useState<number | null>(null)
  const [isSwitchingResponseProfile, setIsSwitchingResponseProfile] = useState(false)
  const [micStarting, setMicStarting] = useState(false)
  const [micActive, setMicActive] = useState(false)
  const [messages, setMessages] = useState<readonly ChatMessage[]>(() =>
    getInitialMessages(),
  )
  const conversationRef = useRef<HTMLElement | null>(null)
  const liveAudioSourceRef = useRef<LiveAudioSource | null>(null)
  const liveAudioSessionIdRef = useRef(0)
  const liveAudioInFlightFramesRef = useRef(0)
  const isSwitchingResponseProfileRef = useRef(false)
  const appActiveRef = useRef(true)
  const autoStopOnSilenceRef = useRef(true)
  const micAutoStartedRef = useRef(false)
  const runtimeStatusRef = useRef<RuntimeStatus>('initializing')
  const startupStateRef = useRef<StartupState>({ kind: 'loading' })
  const voiceActivityStateRef = useRef(createVoiceActivityState())
  const voiceTelemetryRef = useRef<ReturnType<typeof createVoiceTelemetryRecorder> | null>(null)

  if (voiceTelemetryRef.current === null) {
    voiceTelemetryRef.current = createVoiceTelemetryRecorder()
  }

  const voiceTelemetry = voiceTelemetryRef.current

  const currentSilenceTimeoutMs = (): number => {
    if (
      startupStateRef.current.kind === 'ready' ||
      startupStateRef.current.kind === 'warming_model'
    ) {
      return startupStateRef.current.silenceTimeoutMs
    }

    return DEFAULT_SILENCE_TIMEOUT_MS
  }

  const waitForInFlightLiveAudioFrames = async (): Promise<void> => {
    while (appActiveRef.current && liveAudioInFlightFramesRef.current > 0) {
      await new Promise((resolve) => setTimeout(resolve, 10))
    }
  }

  useEffect(() => {
    let active = true

    void (async () => {
      while (active) {
        const nextState = await loadStartupState()

        if (!active) {
          return
        }

        const previousKind = startupStateRef.current.kind
        applyStartupState(nextState)

        if (nextState.kind === 'ready' && previousKind !== 'ready') {
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

        if (nextState.kind === 'error' && previousKind !== 'error') {
          setMessages((currentMessages) => [
            ...currentMessages,
            {
              id: `system-startup-error-${Date.now()}`,
              role: 'system',
              content: `Startup error: ${nextState.message}`,
            },
          ])
        }

        if (isStartupStateSettled(nextState)) {
          return
        }

        await new Promise((resolve) => window.setTimeout(resolve, 500))
      }
    })()

    return () => {
      active = false
    }
  }, [])

  useEffect(() => {
    return () => {
      appActiveRef.current = false
      liveAudioSessionIdRef.current += 1
      liveAudioSourceRef.current?.stop()
      liveAudioSourceRef.current = null
    }
  }, [])

  useEffect(() => {
    const conversation = conversationRef.current

    if (conversation === null) {
      return
    }

    if (typeof conversation.scrollTo === 'function') {
      conversation.scrollTo({ top: conversation.scrollHeight })
      return
    }

    conversation.scrollTop = conversation.scrollHeight
  }, [messages])

  const canSend = useMemo(
    () =>
      startupState.kind === 'ready' &&
      runtimeStatus === 'sleeping' &&
      composerValue.trim().length > 0,
    [composerValue, runtimeStatus, startupState.kind],
  )

  const canStartListening =
    startupState.kind === 'ready' &&
    startupState.voiceInputAvailable &&
    !micActive &&
    runtimeStatus === 'sleeping'
  const canMarkSilence =
    startupState.kind === 'ready' && startupState.voiceInputAvailable && runtimeStatus === 'listening'
  const canResetToIdle = startupState.kind === 'ready' && runtimeStatus === 'error'
  const canToggleMic =
    startupState.kind === 'ready' && startupState.voiceInputAvailable && !micStarting
  const cueAssetPaths =
    startupState.kind === 'ready'
      ? startupState.cueAssetPaths
      : DEFAULT_CUE_ASSET_PATHS
  const responseProfileState =
    startupState.kind === 'ready' || startupState.kind === 'warming_model'
      ? {
          selected: startupState.selectedResponseProfile,
          supported: startupState.supportedResponseProfiles,
        }
      : null
  const canSwitchResponseProfile =
    startupState.kind === 'ready' &&
    runtimeStatus === 'sleeping' &&
    !micStarting &&
    !isSwitchingResponseProfile

  useEffect(() => {
    autoStopOnSilenceRef.current = autoStopOnSilence
  }, [autoStopOnSilence])

  useEffect(() => {
    isSwitchingResponseProfileRef.current = isSwitchingResponseProfile
  }, [isSwitchingResponseProfile])

  const applyStartupState = (nextState: StartupState): void => {
    startupStateRef.current = nextState
    setStartupState(nextState)

    const nextRuntimeStatus = startupStateToRuntimeStatus(nextState)
    runtimeStatusRef.current = nextRuntimeStatus
    setRuntimeStatus(nextRuntimeStatus)
  }

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
      setWakeConfidence(null)
    }

    const cueType = cueForTransition(previousStatus, nextStatus)

    if (cueType !== null) {
      voiceTelemetry.record('cue_play_requested', {
        details: {
          cueType,
          source: 'apply_transition',
        },
      })

      void playCue(cueType, cueAssetPaths)
        .then(() => {
          voiceTelemetry.record('cue_play_started', {
            details: {
              cueType,
              source: 'apply_transition',
            },
          })
        })
        .catch((error: unknown) => {
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
      setWakeConfidence(null)
    }

    const cueType = cueForTransition(previousStatus, nextStatus)

    if (cueType !== null) {
      voiceTelemetry.record('cue_play_requested', {
        details: {
          cueType,
          source: 'apply_runtime_status',
        },
      })

      void playCue(cueType, cueAssetPaths)
        .then(() => {
          voiceTelemetry.record('cue_play_started', {
            details: {
              cueType,
              source: 'apply_runtime_status',
            },
          })
        })
        .catch((error: unknown) => {
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

  const recordRuntimeControlTelemetry = (runtimePhase: RuntimeControlResult): void => {
    const telemetry = runtimePhase.telemetry

    if (telemetry === null) {
      return
    }

    if (telemetry.backendIngestStartedMs !== null) {
      voiceTelemetry.record('backend_ingest_started', {
        atMs: telemetry.backendIngestStartedMs,
        frameId: telemetry.frameId,
      })
    }

    if (telemetry.backendIngestCompletedMs !== null) {
      voiceTelemetry.record('backend_ingest_completed', {
        atMs: telemetry.backendIngestCompletedMs,
        frameId: telemetry.frameId,
      })
    }

    if (telemetry.wakeDetectedMs !== null) {
      voiceTelemetry.record('wake_detected', {
        atMs: telemetry.wakeDetectedMs,
        frameId: telemetry.frameId,
      })
    }

    if (telemetry.transcriptionStartedMs !== null) {
      voiceTelemetry.record('transcription_started', {
        atMs: telemetry.transcriptionStartedMs,
        frameId: telemetry.frameId,
      })
    }

    if (telemetry.transcriptionCompletedMs !== null) {
      voiceTelemetry.record('transcription_completed', {
        atMs: telemetry.transcriptionCompletedMs,
        frameId: telemetry.frameId,
      })
    }
  }

  const enterRuntimeError = (): void => {
    runtimeStatusRef.current = 'error'
    voiceActivityStateRef.current = createVoiceActivityState()
    setWakeConfidence(null)
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
    const previousStatus = runtimeStatusRef.current
    const nextStatus = toRuntimeStatus(runtimePhase.runtimePhase)

    applyRuntimeStatus(nextStatus)
    recordRuntimeControlTelemetry(runtimePhase)

    if (runtimePhase.telemetry?.wakeConfidence !== null && runtimePhase.telemetry?.wakeConfidence !== undefined) {
      setWakeConfidence(runtimePhase.telemetry.wakeConfidence)
    }

    if (previousStatus !== 'listening' && nextStatus === 'listening') {
      voiceTelemetry.record('runtime_status_set_listening', {
        frameId: runtimePhase.telemetry?.frameId ?? null,
      })
    }

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
    command: 'begin_listening' | 'record_speech_activity' | 'mark_silence' | 'reset_session',
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

  const handleMarkSilence = async (telemetryFrameId: string | null = null): Promise<void> => {
    voiceTelemetry.record('cue_play_requested', {
      details: {
        cueType: 'stop_listening',
        source: 'mark_silence',
      },
    })

    try {
      await playCue('stop_listening', cueAssetPaths)
      voiceTelemetry.record('cue_play_started', {
        details: {
          cueType: 'stop_listening',
          source: 'mark_silence',
        },
      })
    } catch (error) {
      reportCuePlaybackError('stop_listening', error)
    }

    const runtimePhase = await syncRuntimeControl(
      'mark_silence',
      telemetryFrameId === null
        ? { fallbackEvent: 'end_listening' }
        : {
            args: { telemetryFrameId },
            fallbackEvent: 'end_listening',
          },
    )

    maybeRunVoiceTranscript(runtimePhase)
  }

  const stopLiveAudio = (content: string | null = null): void => {
    liveAudioSessionIdRef.current += 1
    liveAudioSourceRef.current?.stop()
    liveAudioSourceRef.current = null
    voiceActivityStateRef.current = createVoiceActivityState()
    setMicStarting(false)
    setMicActive(false)

    if (content !== null) {
      setMessages((currentMessages) => [
        ...currentMessages,
        {
          id: `system-live-audio-${Date.now()}`,
          role: 'system',
          content,
        },
      ])
    }
  }

  const reportLiveAudioError = (error: unknown): void => {
    const message = error instanceof Error ? error.message : 'Live audio capture failed'

    stopLiveAudio(`live_audio_error:\n${message}`)
  }

  const startMic = async (): Promise<void> => {
    if (startupStateRef.current.kind !== 'ready' || liveAudioSourceRef.current !== null || micStarting) {
      return
    }

    const liveAudioSessionId = liveAudioSessionIdRef.current + 1
    liveAudioSessionIdRef.current = liveAudioSessionId

    setMicStarting(true)

    try {
      const liveAudioSource = await startLiveAudioSource({
        onFrame: async (frame) => {
          if (liveAudioSessionId !== liveAudioSessionIdRef.current) {
            return
          }

          liveAudioInFlightFramesRef.current += 1

          try {
            const nowMs = Date.now()
            const frameId = voiceTelemetry.nextFrameId(nowMs)

            voiceTelemetry.record('frontend_frame_captured', {
              atMs: nowMs,
              frameId,
              details: {
                sampleCount: frame.length,
              },
            })
            voiceTelemetry.record('frontend_frame_sent', {
              frameId,
            })

            const status = await ingestAudioFrame(
              frame,
              frameId === null ? {} : { telemetryFrameId: frameId },
            )

            if (liveAudioSessionId !== liveAudioSessionIdRef.current) {
              return
            }

            if (status !== null) {
              const nextStatus = toRuntimeStatus(status.runtimePhase)

              applyRuntimeControlResult(status, { quiet: true })
              voiceActivityStateRef.current = syncVoiceActivityState(
                voiceActivityStateRef.current,
                nextStatus,
                status.lastActivityMs,
              )

              if (nextStatus === 'listening' && autoStopOnSilenceRef.current) {
                const voiceActivityUpdate = updateVoiceActivityState(
                  voiceActivityStateRef.current,
                  nowMs,
                  currentSilenceTimeoutMs(),
                )

                voiceActivityStateRef.current = voiceActivityUpdate.state

                if (voiceActivityUpdate.shouldMarkSilence) {
                  await handleMarkSilence(frameId)
                }
              }
            }
          } catch (error) {
            if (liveAudioSessionId !== liveAudioSessionIdRef.current) {
              return
            }

            enterRuntimeError()
            throw error
          } finally {
            liveAudioInFlightFramesRef.current = Math.max(0, liveAudioInFlightFramesRef.current - 1)
          }
        },
        onError: reportLiveAudioError,
      })

      if (!appActiveRef.current || liveAudioSessionId !== liveAudioSessionIdRef.current) {
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
      if (!appActiveRef.current || liveAudioSessionId !== liveAudioSessionIdRef.current) {
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

  const switchResponseProfile = async (profile: ResponseProfile): Promise<void> => {
    if (startupStateRef.current.kind !== 'ready') {
      return
    }

    if (isSwitchingResponseProfileRef.current) {
      return
    }

    const currentState = startupStateRef.current

    if (currentState.selectedResponseProfile === profile) {
      return
    }

    isSwitchingResponseProfileRef.current = true
    setIsSwitchingResponseProfile(true)

    const shouldReportMicStopForSwitch =
      micActive || micStarting || liveAudioSourceRef.current !== null
    stopLiveAudio(
      shouldReportMicStopForSwitch
        ? 'live_audio:\ndefault microphone stopped for profile switch'
        : null,
    )

    await waitForInFlightLiveAudioFrames()

    const settleStartupState = async (): Promise<void> => {
      while (true) {
        if (!appActiveRef.current) {
          return
        }

        const nextState = await loadStartupState()

        if (!appActiveRef.current) {
          return
        }

        applyStartupState(nextState)

        if (isStartupStateSettled(nextState)) {
          break
        }

        await new Promise((resolve) => window.setTimeout(resolve, 500))
      }
    }

    const warmingState: StartupState = {
      kind: 'warming_model',
      cueAssetPaths: currentState.cueAssetPaths,
      runtimePhase: 'initializing',
      voiceInputAvailable: currentState.voiceInputAvailable,
      voiceInputError: currentState.voiceInputError,
      silenceTimeoutMs: currentState.silenceTimeoutMs,
      message: `Switching response profile to ${getResponseProfileLabel(profile)}...`,
      selectedResponseProfile: profile,
      supportedResponseProfiles: currentState.supportedResponseProfiles,
    }

    startupStateRef.current = warmingState
    setStartupState(warmingState)
    runtimeStatusRef.current = 'initializing'
    setRuntimeStatus('initializing')

    try {
      await invokeTauriCommand('switch_response_profile', { profile })

      await settleStartupState()
    } catch (error) {
      try {
        await settleStartupState()
      } catch {
        if (appActiveRef.current) {
          applyStartupState(currentState)
        }
      }

      if (appActiveRef.current) {
        setMessages((currentMessages) => [
          ...currentMessages,
          {
            id: `system-switch-profile-error-${Date.now()}`,
            role: 'system',
            content: `Response profile switch error: ${toDisplayErrorMessage(error)}`,
          },
        ])
      }
    } finally {
      isSwitchingResponseProfileRef.current = false
      if (appActiveRef.current) {
        setIsSwitchingResponseProfile(false)
      }
    }
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
  // eslint-disable-next-line react-hooks/exhaustive-deps
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
        <div className="shell__badges" aria-label="Runtime badges">
          <span className="shell__badge">Mic {micActive ? 'on' : 'off'}</span>
          <span className="shell__badge">
            Voice {startupState.kind === 'ready' && startupState.voiceInputAvailable ? 'ready' : 'limited'}
          </span>
          <span className="shell__badge">Auto stop {autoStopOnSilence ? 'on' : 'off'}</span>
          {runtimeStatus === 'listening' && wakeConfidence !== null ? (
            <span className="shell__badge">Wake trigger score {wakeConfidence.toFixed(3)}</span>
          ) : null}
        </div>
        {startupState.kind === 'error' ? (
          <p className="shell__error">Startup error: {startupState.message}</p>
        ) : null}
        {startupState.kind === 'ready' && !startupState.voiceInputAvailable ? (
          <p className="shell__error">
            Voice input unavailable: {startupState.voiceInputError ?? 'Parakeet failed to initialize'}
          </p>
        ) : null}
        <div className="shell__toggles-line">
          {responseProfileState !== null ? (
            <div className="shell__controls" role="group" aria-label="Response profile controls">
              <label className="shell__select-field" htmlFor="responseProfileSelect">
                Response profile
              </label>
              <select
                id="responseProfileSelect"
                className="shell__select"
                value={responseProfileState.selected}
                disabled={!canSwitchResponseProfile || responseProfileState.supported.length < 2}
                onChange={(event) => {
                  const nextProfile = parseResponseProfileValue(event.target.value)
                  if (nextProfile === null || !responseProfileState.supported.includes(nextProfile)) {
                    return
                  }

                  void switchResponseProfile(nextProfile)
                }}
              >
                {RESPONSE_PROFILE_ORDER.map((profile) => (
                  <option
                    key={profile}
                    value={profile}
                    disabled={!responseProfileState.supported.includes(profile)}
                  >
                    {getResponseProfileLabel(profile)}
                  </option>
                ))}
              </select>
            </div>
          ) : null}
          <label className="shell__toggle">
            <input
              type="checkbox"
              checked={autoStopOnSilence}
              onChange={(event) => setAutoStopOnSilence(event.target.checked)}
              disabled={startupState.kind !== 'ready' || !startupState.voiceInputAvailable}
            />
            <span>Auto stop on silence</span>
          </label>
        </div>
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
                  fallbackEvent: 'recover_from_error',
                },
              )
            }}
            disabled={!canResetToIdle}
          >
            Reset to idle
          </button>
        </div>
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
          </div>
        </details>
      </header>

      <main ref={conversationRef} className="conversation" aria-live="polite">
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

const RESPONSE_PROFILE_ORDER: readonly ResponseProfile[] = ['fast', 'quality']

function getResponseProfileLabel(profile: ResponseProfile): 'Fast' | 'Quality' {
  return profile === 'fast' ? 'Fast' : 'Quality'
}

function parseResponseProfileValue(value: string): ResponseProfile | null {
  if (value === 'fast' || value === 'quality') {
    return value
  }

  return null
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

function startupStateToRuntimeStatus(startupState: StartupState): RuntimeStatus {
  if (startupState.kind === 'error') {
    return 'error'
  }

  if (startupState.kind === 'loading') {
    return 'initializing'
  }

  return toRuntimeStatus(startupState.runtimePhase)
}

export default App

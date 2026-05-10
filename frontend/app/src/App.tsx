import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { FormEvent, KeyboardEvent } from 'react'
import { ChatBubble } from './components/ChatBubble'
import { UserNoticeToast } from './components/UserNoticeToast'
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
  DEFAULT_TTS_OUTPUT_GAIN_DB,
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
import { isTranscriptMessage } from './types/chat'
import type {
  BackendRuntimePhase,
  ChatMessage,
  PromptExecutionResult,
  RuntimeControlResult,
  ResponseProfile,
  RuntimeStatus,
  StartupState,
  UiTextSize,
  UserNotice,
} from './types/chat'
import './App.css'

const NOTICE_AUTO_DISMISS_MS = 4_000
const MAX_NOTICE_QUEUE_LENGTH = 3
const UI_TEXT_SIZE_STEPS: readonly UiTextSize[] = ['small', 'medium', 'large', 'extra_large']
const DEFAULT_UI_TEXT_SIZE: UiTextSize = 'medium'

const UI_TEXT_SIZE_LABELS: Record<UiTextSize, string> = {
  small: 'Small',
  medium: 'Medium',
  large: 'Large',
  extra_large: 'Extra Large',
}

const UI_TEXT_SIZE_PERCENTAGES: Record<UiTextSize, string> = {
  small: '90%',
  medium: '100%',
  large: '112.5%',
  extra_large: '125%',
}

type RuntimeDiagnosticKind =
  | 'frontend_notice'
  | 'cue'
  | 'runtime_control'
  | 'execution'
  | 'tts'
  | 'audio'
  | 'profile'

function App() {
  const [startupState, setStartupState] = useState<StartupState>({ kind: 'loading' })
  const [runtimeStatus, setRuntimeStatus] = useState<RuntimeStatus>('initializing')
  const [composerValue, setComposerValue] = useState('')
  const [autoStopOnSilence, setAutoStopOnSilence] = useState(true)
  const [ttsEnabled, setTtsEnabled] = useState(false)
  const [wakeConfidence, setWakeConfidence] = useState<number | null>(null)
  const [isSwitchingResponseProfile, setIsSwitchingResponseProfile] = useState(false)
  const [micStarting, setMicStarting] = useState(false)
  const [micActive, setMicActive] = useState(false)
  const [notices, setNotices] = useState<readonly UserNotice[]>([])
  const [settingsOpen, setSettingsOpen] = useState(false)
  const [uiTextSize, setUiTextSize] = useState<UiTextSize>(DEFAULT_UI_TEXT_SIZE)
  const [messages, setMessages] = useState<readonly ChatMessage[]>(() =>
    getInitialMessages(),
  )
  const visibleMessages = useMemo(
    () => messages.filter(isTranscriptMessage),
    [messages],
  )
  const conversationRef = useRef<HTMLElement | null>(null)
  const settingsButtonRef = useRef<HTMLButtonElement | null>(null)
  const settingsPanelRef = useRef<HTMLElement | null>(null)
  const settingsCloseButtonRef = useRef<HTMLButtonElement | null>(null)
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

  const closeSettings = useCallback((): void => {
    setSettingsOpen(false)
    window.setTimeout(() => settingsButtonRef.current?.focus(), 0)
  }, [])

  const recordRuntimeDiagnostic = useCallback((kind: RuntimeDiagnosticKind, detail: string): void => {
    void invokeTauriCommand('record_frontend_runtime_diagnostic', {
      event: {
        kind,
        detail,
      },
    }).catch(() => undefined)
  }, [])

  const addNotice = useCallback((notice: Omit<UserNotice, 'id'>): void => {
    setNotices((currentNotices) => {
      const nextNotice = {
        ...notice,
        id: `notice-${Date.now()}-${currentNotices.length}`,
      }

      if (currentNotices.length < MAX_NOTICE_QUEUE_LENGTH) {
        return [...currentNotices, nextNotice]
      }

      const activeNotice = currentNotices[0]
      if (activeNotice === undefined) {
        return [nextNotice]
      }

      return [
        activeNotice,
        ...currentNotices.slice(-(MAX_NOTICE_QUEUE_LENGTH - 2)),
        nextNotice,
      ]
    })
    recordRuntimeDiagnostic('frontend_notice', `${notice.title}: ${notice.message}`)
  }, [recordRuntimeDiagnostic])

  useEffect(() => {
    if (notices.length === 0) {
      return undefined
    }

    const timeoutId = window.setTimeout(() => {
      setNotices((currentNotices) => currentNotices.slice(1))
    }, NOTICE_AUTO_DISMISS_MS)

    return () => window.clearTimeout(timeoutId)
  }, [notices])

  useEffect(() => {
    if (!settingsOpen) {
      return
    }

    settingsCloseButtonRef.current?.focus()
  }, [settingsOpen])

  useEffect(() => {
    let active = true

    void invokeTauriCommand('get_ui_text_size')
      .then((payload) => {
        if (!active) {
          return
        }

        setUiTextSize(parseUiTextSize(payload))
      })
      .catch(() => undefined)

    return () => {
      active = false
    }
  }, [])

  const persistUiTextSize = async (nextTextSize: UiTextSize, previousTextSize: UiTextSize): Promise<void> => {
    setUiTextSize(nextTextSize)

    try {
      const payload = await invokeTauriCommand('set_ui_text_size', { textSize: nextTextSize })
      setUiTextSize(parseUiTextSize(payload))
    } catch (error) {
      setUiTextSize(previousTextSize)
      addNotice({
        tone: 'error',
        title: 'Setting not saved',
        message: toDisplayErrorMessage(error),
      })
    }
  }

  const adjustUiTextSize = (direction: -1 | 1): void => {
    const currentIndex = UI_TEXT_SIZE_STEPS.indexOf(uiTextSize)
    const nextIndex = Math.max(0, Math.min(UI_TEXT_SIZE_STEPS.length - 1, currentIndex + direction))
    const nextTextSize = UI_TEXT_SIZE_STEPS[nextIndex]

    if (nextTextSize === undefined || nextTextSize === uiTextSize) {
      return
    }

    void persistUiTextSize(nextTextSize, uiTextSize)
  }


  const currentSilenceTimeoutMs = (): number => {
    if (
      startupStateRef.current.kind === 'ready' ||
      startupStateRef.current.kind === 'warming_model'
    ) {
      return startupStateRef.current.silenceTimeoutMs
    }

    return DEFAULT_SILENCE_TIMEOUT_MS
  }

  const currentTtsOutputGainDb = (): number => {
    if (
      startupStateRef.current.kind === 'ready' ||
      startupStateRef.current.kind === 'warming_model'
    ) {
      return startupStateRef.current.ttsOutputGainDb
    }

    return DEFAULT_TTS_OUTPUT_GAIN_DB
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
          addNotice({
            tone: 'error',
            title: 'Startup failed',
            message: nextState.message,
          })
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
  }, [addNotice])

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
  }, [visibleMessages])

  const canSend = useMemo(
    () =>
      startupState.kind === 'ready' &&
      runtimeStatus === 'sleeping' &&
      composerValue.trim().length > 0,
    [composerValue, runtimeStatus, startupState.kind],
  )

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
  const canToggleTts = startupState.kind === 'ready' && !isSwitchingResponseProfile

  useEffect(() => {
    autoStopOnSilenceRef.current = autoStopOnSilence
  }, [autoStopOnSilence])

  useEffect(() => {
    isSwitchingResponseProfileRef.current = isSwitchingResponseProfile
  }, [isSwitchingResponseProfile])

  const applyStartupState = (nextState: StartupState): void => {
    startupStateRef.current = nextState
    setStartupState(nextState)
    if (nextState.kind === 'ready' || nextState.kind === 'warming_model') {
      setTtsEnabled(nextState.ttsEnabled)
    } else {
      setTtsEnabled(false)
    }

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

    recordRuntimeDiagnostic(
      'cue',
      `Cue playback error: ${message} [cue=${cueType}, startCue=${cueAssetPaths.startListening}, stopCue=${cueAssetPaths.stopListening}]`,
    )
    addNotice({
      tone: 'error',
      title: 'Cue playback failed',
      message,
    })
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
    command: 'mark_silence',
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

      recoverFromRuntimeControlError()
      addNotice({
        tone: 'error',
        title: 'Runtime control failed',
        message,
      })
      recordRuntimeDiagnostic('runtime_control', `Runtime control error (${command}): ${message}`)
      return null
    }
  }

  const recoverFromRuntimeControlError = (): void => {
    applyRuntimeStatus('sleeping')
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

      const assistantMessage = [...nextMessages].reverse().find((message) => message.role === 'assistant')
      if (assistantMessage === undefined) {
        addNotice(createPromptExecutionNotice(result))
      }

      if (
        ttsEnabled &&
        assistantMessage !== undefined &&
        assistantMessage.content.trim().length > 0
      ) {
        void synthesizeAndPlayAssistantReply(assistantMessage.content)
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : 'Prompt execution failed'

      applyTransition(executingStatus, 'fail')
      addNotice({
        tone: 'error',
        title: 'Response failed',
        message,
      })
      recordRuntimeDiagnostic('execution', `Execution error: ${message}`)
    }
  }

  const synthesizeAndPlayAssistantReply = async (text: string): Promise<void> => {
    let audioContext: AudioContext | null = null

    try {
      const payload = await invokeTauriCommand('synthesize_local_tts', { text })

      if (
        !isRecord(payload) ||
        !Array.isArray(payload['pcm_f32']) ||
        typeof payload['sample_rate_hz'] !== 'number' ||
        payload['sample_rate_hz'] <= 0 ||
        !Number.isFinite(payload['sample_rate_hz'])
      ) {
        throw new Error('TTS synthesis payload is invalid')
      }

      audioContext = new AudioContext()
      if (audioContext.state === 'suspended') {
        await audioContext.resume()
      }
      const sampleRate = Math.trunc(payload['sample_rate_hz'])
      const pcm = Float32Array.from(
        payload['pcm_f32'].map((sample) => {
          const finiteSample = typeof sample === 'number' && Number.isFinite(sample) ? sample : 0
          return Math.max(-1, Math.min(1, finiteSample))
        }),
      )

      if (pcm.length === 0) {
        return
      }

      const buffer = audioContext.createBuffer(1, pcm.length, sampleRate)
      buffer.copyToChannel(pcm, 0)
      const source = audioContext.createBufferSource()
      source.buffer = buffer
      const gainNode = audioContext.createGain()
      gainNode.gain.value = Math.pow(10, currentTtsOutputGainDb() / 20)
      source.connect(gainNode)
      gainNode.connect(audioContext.destination)

      await new Promise<void>((resolve) => {
        source.onended = () => resolve()
        source.start()
      })
    } catch (error) {
      const message = toDisplayErrorMessage(error)

      addNotice({
        tone: 'error',
        title: 'TTS playback failed',
        message,
      })
      recordRuntimeDiagnostic('tts', `TTS synthesis error: ${message}`)
    } finally {
      if (audioContext !== null) {
        await audioContext.close().catch(() => undefined)
      }
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

    recordRuntimeDiagnostic('audio', `Live audio error: ${message}`)
    addNotice({
      tone: 'error',
      title: 'Microphone unavailable',
      message,
    })
    stopLiveAudio()
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
      ttsEnabled: currentState.ttsEnabled,
      ttsOutputGainDb: currentState.ttsOutputGainDb,
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
        const message = toDisplayErrorMessage(error)

        addNotice({
          tone: 'error',
          title: 'Profile switch failed',
          message,
        })
        recordRuntimeDiagnostic('profile', `Response profile switch error: ${message}`)
      }
    } finally {
      isSwitchingResponseProfileRef.current = false
      if (appActiveRef.current) {
        setIsSwitchingResponseProfile(false)
      }
    }
  }

  const toggleTts = async (enabled: boolean): Promise<void> => {
    const previousEnabled = ttsEnabled
    setTtsEnabled(enabled)

    try {
      const payload = await invokeTauriCommand('set_tts_enabled', { enabled })

      if (
        typeof payload === 'object' &&
        payload !== null &&
        'enabled' in payload &&
        typeof payload.enabled === 'boolean'
      ) {
        setTtsEnabled(payload.enabled)
      } else {
        setTtsEnabled(enabled)
      }
    } catch (error) {
      const message = toDisplayErrorMessage(error)

      setTtsEnabled(previousEnabled)
      addNotice({
        tone: 'error',
        title: 'TTS setting failed',
        message,
      })
      recordRuntimeDiagnostic('tts', `TTS toggle error: ${message}`)
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
  const onSettingsKeyDown = (event: KeyboardEvent<HTMLDivElement>): void => {
    if (event.key === 'Escape') {
      event.preventDefault()
      closeSettings()
      return
    }

    if (event.key !== 'Tab') {
      return
    }

    const focusableElements = Array.from(
      settingsPanelRef.current?.querySelectorAll<HTMLElement>(
        'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
      ) ?? [],
    )

    const firstElement = focusableElements[0]
    const lastElement = focusableElements.at(-1)

    if (firstElement === undefined || lastElement === undefined) {
      return
    }

    if (event.shiftKey && document.activeElement === firstElement) {
      event.preventDefault()
      lastElement.focus()
      return
    }

    if (!event.shiftKey && document.activeElement === lastElement) {
      event.preventDefault()
      firstElement.focus()
    }
  }

  const uiTextSizeIndex = Math.max(0, UI_TEXT_SIZE_STEPS.indexOf(uiTextSize))
  const canDecreaseTextSize = uiTextSizeIndex > 0
  const canIncreaseTextSize = uiTextSizeIndex < UI_TEXT_SIZE_STEPS.length - 1


  return (
    <div className="shell" data-ui-text-size={uiTextSize}>

      <main ref={conversationRef} className="conversation" aria-live="polite">
        {visibleMessages.map((message) => (
          <ChatBubble key={message.id} message={message} />
        ))}
      </main>

      <UserNoticeToast notice={notices[0] ?? null} />

      {settingsOpen ? (
        <div
          className="settings-overlay"
          onMouseDown={closeSettings}
          onKeyDown={onSettingsKeyDown}
        >
          <section
            ref={settingsPanelRef}
            className="settings-panel"
            role="dialog"
            aria-modal="true"
            aria-labelledby="settings-title"
            onMouseDown={(event) => event.stopPropagation()}
          >
            <div className="settings-panel__header">
              <div>
                <h2 id="settings-title">Settings</h2>
                <p>Adjust the app text size live.</p>
              </div>
              <button
                ref={settingsCloseButtonRef}
                type="button"
                className="settings-panel__close"
                onClick={closeSettings}
                aria-label="Close settings"
              >
                ×
              </button>
            </div>
            <div className="settings-panel__row">
              <div>
                <strong>Text size</strong>
                <p className="settings-panel__hint">Applies to chat, composer, and controls.</p>
              </div>
              <div className="settings-panel__stepper" aria-label="Text size controls">
                <button
                  type="button"
                  className="settings-panel__step-button"
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
                  className="settings-panel__step-button"
                  onClick={() => adjustUiTextSize(1)}
                  disabled={!canIncreaseTextSize}
                  aria-label="Increase text size"
                >
                  +
                </button>
              </div>
            </div>
          </section>
        </div>
      ) : null}

      <form className="composer" onSubmit={onSubmit}>
        <textarea
          id="promptComposer"
          className="composer__input"
          aria-label="Prompt"
          value={composerValue}
          onChange={(event) => setComposerValue(event.target.value)}
          onKeyDown={onComposerKeyDown}
          placeholder="Type a prompt..."
          rows={3}
        />
        {startupState.kind === 'error' ? (
          <p className="shell__error">Startup error: {startupState.message}</p>
        ) : null}
        {startupState.kind === 'ready' && !startupState.voiceInputAvailable ? (
          <p className="shell__error">
            Voice input unavailable: {startupState.voiceInputError ?? 'Parakeet failed to initialize'}
          </p>
        ) : null}
        {startupState.kind === 'warming_model' ? (
          <p className="shell__loading">Model loading: {startupState.message}</p>
        ) : null}
        <div className="composer__actions">
          {responseProfileState !== null ? (
            <select
              id="responseProfileSelect"
              className="shell__select"
              aria-label="Response profile"
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
          ) : null}
          <label className="shell__toggle">
            <input
              type="checkbox"
              checked={autoStopOnSilence}
              onChange={(event) => setAutoStopOnSilence(event.target.checked)}
              disabled={startupState.kind !== 'ready' || !startupState.voiceInputAvailable}
            />
            <span>Auto Stop</span>
          </label>
          <label className="shell__toggle" htmlFor="tts-toggle">
            <input
              id="tts-toggle"
              type="checkbox"
              checked={ttsEnabled}
              disabled={!canToggleTts}
              onChange={(event) => {
                void toggleTts(event.target.checked)
              }}
            />
            <span>TTS</span>
          </label>
          <button
            type="button"
            className="shell__control"
            onClick={toggleMic}
            disabled={!canToggleMic}
          >
            {micStarting ? 'Starting mic...' : micActive ? 'Stop mic' : 'Start mic'}
          </button>
          <div className="composer__send-side">
            {wakeConfidence !== null ? (
              <div
                className={wakeConfidence >= 0.7 ? 'composer__wake composer__wake--high' : wakeConfidence >= 0.4 ? 'composer__wake composer__wake--medium' : 'composer__wake composer__wake--low'}
                aria-live="polite"
              >
                <span className="composer__wake-primary">
                  wake: {(wakeConfidence * 100).toFixed(0)}%
                </span>
              </div>
            ) : null}
            <button
              ref={settingsButtonRef}
              type="button"
              className="shell__control shell__settings-button"
              onClick={() => setSettingsOpen(true)}
              aria-label="Settings"
              aria-haspopup="dialog"
              aria-expanded={settingsOpen}
              title="Settings"
            >
              ⚙
            </button>
            <button type="submit" className="composer__button" disabled={!canSend}>
              Send
            </button>
          </div>
        </div>
      </form>
    </div>
  )
}

function toRuntimeStatus(runtimePhase: BackendRuntimePhase): RuntimeStatus {
  return runtimePhase
}

const RESPONSE_PROFILE_ORDER: readonly ResponseProfile[] = ['fast', 'quality']

function parseUiTextSize(value: unknown): UiTextSize {
  if (
    value === 'small' ||
    value === 'medium' ||
    value === 'large' ||
    value === 'extra_large'
  ) {
    return value
  }

  return DEFAULT_UI_TEXT_SIZE
}

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

function createPromptExecutionNotice(result: PromptExecutionResult): Omit<UserNotice, 'id'> {
  const hasExecutionFailure =
    (result.exitCode !== null && result.exitCode !== 0) ||
    result.stderr.trim().length > 0 ||
    result.events.some((event) => event.kind === 'error')

  if (hasExecutionFailure) {
    return {
      tone: 'error',
      title: 'Response failed',
      message: 'The response could not be completed. Try again.',
    }
  }

  return {
    tone: 'info',
    title: 'No response',
    message: 'No response was returned. Try again.',
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null
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

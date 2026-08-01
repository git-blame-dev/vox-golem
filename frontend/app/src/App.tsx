import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { FormEvent, KeyboardEvent } from 'react'
import { flushSync } from 'react-dom'
import { ChatBubble } from './components/ChatBubble'
import { AnswerStage } from './components/AnswerStage'
import type { AnswerStageStatusEntry, AnswerPriorVersion } from './components/AnswerStage'
import { PromptComposer } from './components/PromptComposer'
import { UserNoticeToast } from './components/UserNoticeToast'
import { UpdateSettings } from './components/UpdateSettings'
import { playCue } from './lib/audioCues'
import { shouldSubmitComposer } from './lib/composer'
import { listAudioInputDevices, startLiveAudioSource } from './lib/liveAudioSource'
import type { AudioInputDevice, LiveAudioSource } from './lib/liveAudioSource'
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
import { getTauriInternals, invokeTauriCommand } from './lib/tauri'
import { useAppUpdates } from './lib/useAppUpdates'
import { DEFAULT_ASSISTANT_SETTINGS, deepOptions, instantOptions, parseAssistantSettings, reviewOptions, serializeAssistantSettings } from './lib/assistantSettings'
import type { AssistantSettings } from './lib/assistantSettings'
import { acceptsPartialTranscriptionEvent, parsePartialTranscriptionEvent } from './lib/partialTranscription'
import { parseCompletionEvent } from './lib/completionEvents'
import { createVoiceTelemetryRecorder } from './lib/voiceTelemetry'
import {
  createVoiceActivityState,
  syncVoiceActivityState,
  updateVoiceActivityState,
} from './lib/voiceActivity'
import { getInitialMessages } from './state/appShell'
import { cueForTransition, transitionRuntimeStatus } from './state/runtimeMachine'
import { isTranscriptMessage } from './types/chat'
import { firstNonEmptyCompletedLine, firstNonEmptyStreamingLine, isValidTtsFirstLine } from './lib/ttsValidation'
import type {
  BackendRuntimePhase,
  ChatMessage,
  RuntimeControlResult,
  ResponseProfile,
  RuntimeStatus,
  StartupState,
  UiTextSize,
  UiTheme,
  UserNotice,
} from './types/chat'
import './App.css'

const NOTICE_AUTO_DISMISS_MS = 4_000
const WAKE_CONFIDENCE_PEAK_HOLD_MS = 1_000
const MAX_NOTICE_QUEUE_LENGTH = 3
const UI_TEXT_SIZE_STEPS: readonly UiTextSize[] = ['small', 'medium', 'large', 'extra_large']
const DEFAULT_UI_TEXT_SIZE: UiTextSize = 'medium'
const DEFAULT_UI_THEME: UiTheme = 'dark'
const AUDIO_INPUT_DEVICE_STORAGE_KEY = 'voxgolem.audioInputDeviceId'

function loadAudioInputDeviceId(): string | null {
  try {
    return window.localStorage.getItem(AUDIO_INPUT_DEVICE_STORAGE_KEY)
  } catch {
    return null
  }
}

function persistAudioInputDeviceId(deviceId: string | null): void {
  try {
    if (deviceId === null) {
      window.localStorage.removeItem(AUDIO_INPUT_DEVICE_STORAGE_KEY)
    } else {
      window.localStorage.setItem(AUDIO_INPUT_DEVICE_STORAGE_KEY, deviceId)
    }
  } catch {
    // Browser storage is optional; the selected device still applies to this session.
  }
}

const voiceInputReady = (state: StartupState): boolean =>
  state.kind === 'ready' &&
  state.voiceInputAvailable &&
  hasAvailableCapabilities(state, ['wake_word', 'vad', 'parakeet'])

const instantMatchesResponseProfile = (
  state: StartupState,
  instant: AssistantSettings['instant'],
): boolean =>
  state.kind === 'ready' &&
  (instant === 'local-fast'
    ? state.selectedResponseProfile === 'fast'
    : instant === 'local-quality'
      ? state.selectedResponseProfile === 'quality'
      : true)

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
  const appUpdates = useAppUpdates()
  const [startupState, setStartupState] = useState<StartupState>({ kind: 'loading' })
  const [runtimeStatus, setRuntimeStatus] = useState<RuntimeStatus>('initializing')
  const [composerValue, setComposerValue] = useState('')
  const [partialTranscript, setPartialTranscript] = useState('')
  const [typedCompletionSuffix, setTypedCompletionSuffix] = useState('')
  const [voiceCompletionSuffix, setVoiceCompletionSuffix] = useState('')
  const [autoStopOnSilence, setAutoStopOnSilence] = useState(true)
  const [ttsEnabled, setTtsEnabled] = useState(false)
  const [ttsPlaying, setTtsPlaying] = useState(false)
  const [pendingTtsCommands, setPendingTtsCommands] = useState(0)
  const [wakeConfidence, setWakeConfidence] = useState<number | null>(null)
  const [isSwitchingResponseProfile, setIsSwitchingResponseProfile] = useState(false)
  const [micStarting, setMicStarting] = useState(false)
  const [micActive, setMicActive] = useState(false)
  const [audioInputDevices, setAudioInputDevices] = useState<readonly AudioInputDevice[]>([])
  const [audioInputDeviceId, setAudioInputDeviceId] = useState<string | null>(loadAudioInputDeviceId)
  const [settingsOpen, setSettingsOpen] = useState(false)
  const [assistantSettingsPending, setAssistantSettingsPending] = useState(
    () => getTauriInternals() !== null,
  )
  const [assistantSettingsLoadError, setAssistantSettingsLoadError] = useState<string | null>(null)
  const [assistantSettings, setAssistantSettings] = useState<AssistantSettings>(DEFAULT_ASSISTANT_SETTINGS)
  const assistantSettingsRef = useRef<AssistantSettings>(DEFAULT_ASSISTANT_SETTINGS)
  const assistantSettingsPendingRef = useRef(getTauriInternals() !== null)
  const persistedAssistantSettingsRef = useRef<AssistantSettings>(DEFAULT_ASSISTANT_SETTINGS)
  const assistantSettingsWriteRevisionRef = useRef(0)
  const assistantSettingsWriteChainRef = useRef<Promise<void>>(Promise.resolve())
  const instantSelectionRevisionRef = useRef(0)
  const [uiTextSize, setUiTextSize] = useState<UiTextSize>(DEFAULT_UI_TEXT_SIZE)
  const [uiTheme, setUiTheme] = useState<UiTheme>(DEFAULT_UI_THEME)
  const [notices, setNotices] = useState<readonly UserNotice[]>([])
  const [messages, setMessages] = useState<readonly ChatMessage[]>(() =>
    getInitialMessages(),
  )
  const [promptActivity, setPromptActivity] = useState<string | null>(null)
  const [promptState, setPromptState] = useState<'idle' | 'executing' | 'stopping'>('idle')
  const [resetPending, setResetPending] = useState(false)
  const promptRef = useRef<{
    id: string
    assistantId: string
    text: string
    error: string | null
    terminal: boolean
    tts: boolean
    corrected: boolean
    stages: AnswerStageStatusEntry[]
    priorVersions: AnswerPriorVersion[]
     sources: import('./components/AnswerStage').AnswerSource[]
     ttsFirstLineSpoken: boolean
  } | null>(null)
  const visibleMessages = useMemo(
    () => messages.filter(isTranscriptMessage),
    [messages],
  )
  const conversationRef = useRef<HTMLElement | null>(null)
  const settingsButtonRef = useRef<HTMLButtonElement | null>(null)
  const settingsPanelRef = useRef<HTMLElement | null>(null)
  const settingsCloseButtonRef = useRef<HTMLButtonElement | null>(null)
  const liveAudioSourceRef = useRef<LiveAudioSource | null>(null)
  const liveAudioStartAbortRef = useRef<AbortController | null>(null)
  const audioInputDeviceIdRef = useRef(audioInputDeviceId)
  const liveAudioSessionIdRef = useRef(0)
  const liveAudioInFlightFramesRef = useRef(0)
  const uiTextSizeHydrationOverriddenRef = useRef(false)
  const uiThemeHydrationOverriddenRef = useRef(false)
  const isSwitchingResponseProfileRef = useRef(false)
  const appActiveRef = useRef(true)
  const autoStopOnSilenceRef = useRef(true)
  const micAutoStartedRef = useRef(false)
  const micStartInFlightRef = useRef(false)
  const runtimeStatusRef = useRef<RuntimeStatus>('initializing')
  const startupStateRef = useRef<StartupState>({ kind: 'loading' })
  const voiceActivityStateRef = useRef(createVoiceActivityState())
  const voiceTelemetryRef = useRef<ReturnType<typeof createVoiceTelemetryRecorder> | null>(null)
  const partialTranscriptionRef = useRef<{
    sessionId: number
    revision: number
    active: boolean
  } | null>(null)
  const partialTranscriptionAllowedRef = useRef(false)
  const expectedPartialSessionIdRef = useRef(0)
  const completionRevisionRef = useRef(0)
  const latestTypedCompletionRef = useRef<{ revision: number; lifecycle: number } | null>(null)
  const completionLifecycleRef = useRef(0)
  const resetPendingRef = useRef(false)
  const ttsGenerationRef = useRef(0)
  const ttsEnabledRef = useRef(false)
  const ttsPlaybackIdRef = useRef<number | null>(null)
  const uiTextSizeWriteRevisionRef = useRef(0)
  const uiThemeWriteRevisionRef = useRef(0)
  const ttsWriteRevisionRef = useRef(0)
  const uiTextSizeWriteChainRef = useRef<Promise<void>>(Promise.resolve())
  const uiThemeWriteChainRef = useRef<Promise<void>>(Promise.resolve())
  const ttsWriteChainRef = useRef<Promise<void>>(Promise.resolve())
  const wakeConfidenceRef = useRef<number | null>(null)
  const pendingWakeConfidenceRef = useRef<number | null>(null)
  const wakeConfidenceHoldRef = useRef<number | null>(null)

  const cancelTts = (): void => {
    ttsGenerationRef.current += 1
    const playbackId = ttsPlaybackIdRef.current
    if (playbackId === null) {
      setTtsPlaying(false)
      return
    }
    void invokeTauriCommand('finish_tts_playback', { playbackId })
      .catch(() => undefined)
      .finally(() => {
        if (ttsPlaybackIdRef.current === playbackId) {
          ttsPlaybackIdRef.current = null
          setTtsPlaying(false)
        }
      })
  }

  const resetWakeConfidence = (): void => {
    if (wakeConfidenceHoldRef.current !== null) {
      window.clearTimeout(wakeConfidenceHoldRef.current)
      wakeConfidenceHoldRef.current = null
    }
    wakeConfidenceRef.current = null
    pendingWakeConfidenceRef.current = null
    setWakeConfidence(null)
  }

  const updateWakeConfidence = (nextConfidence: number): void => {
    if (!Number.isFinite(nextConfidence) || nextConfidence < 0 || nextConfidence > 1) return
    if (wakeConfidenceRef.current === null || nextConfidence > wakeConfidenceRef.current) {
      if (wakeConfidenceHoldRef.current !== null) {
        window.clearTimeout(wakeConfidenceHoldRef.current)
      }
      wakeConfidenceRef.current = nextConfidence
      pendingWakeConfidenceRef.current = nextConfidence
      setWakeConfidence(nextConfidence)
      wakeConfidenceHoldRef.current = window.setTimeout(() => {
        wakeConfidenceHoldRef.current = null
        wakeConfidenceRef.current = pendingWakeConfidenceRef.current
        setWakeConfidence(pendingWakeConfidenceRef.current)
      }, WAKE_CONFIDENCE_PEAK_HOLD_MS)
      return
    }

    pendingWakeConfidenceRef.current = nextConfidence
    if (wakeConfidenceHoldRef.current === null) {
      wakeConfidenceHoldRef.current = window.setTimeout(() => {
        wakeConfidenceHoldRef.current = null
        wakeConfidenceRef.current = pendingWakeConfidenceRef.current
        setWakeConfidence(pendingWakeConfidenceRef.current)
      }, WAKE_CONFIDENCE_PEAK_HOLD_MS)
    }
  }

  useEffect(() => {
    const tauri = getTauriInternals()
    if (tauri?.listen === undefined) return undefined
    let active = true
    let unlisten: (() => void) | undefined
    void tauri.listen('partial-transcription-event', (event) => {
      try {
        const parsed = parsePartialTranscriptionEvent(event.payload)
        if (!partialTranscriptionAllowedRef.current) return
        if (!acceptsPartialTranscriptionEvent(partialTranscriptionRef.current, parsed)) return
        partialTranscriptionRef.current = {
          sessionId: parsed.sessionId,
          revision: parsed.revision,
          active: true,
        }
        if (active) {
          setVoiceCompletionSuffix('')
          setPartialTranscript(parsed.text)
        }
      } catch {
        return
      }
    }).then((cleanup) => active ? (unlisten = cleanup) : cleanup()).catch(() => undefined)
    return () => { active = false; unlisten?.() }
  }, [])

  useEffect(() => {
    const tauri = getTauriInternals()
    if (tauri?.listen === undefined) return undefined
    let active = true
    let unlisten: (() => void) | undefined
    void tauri.listen('completion-event', (event) => {
      try {
        const parsed = parseCompletionEvent(event.payload)
        if (parsed.source === 'typed') {
          const latest = latestTypedCompletionRef.current
          if (latest === null || parsed.revision !== latest.revision || latest.lifecycle !== completionLifecycleRef.current) return
          latestTypedCompletionRef.current = null
          if (active) setTypedCompletionSuffix(parsed.suffix ?? '')
        } else {
          const transcript = partialTranscriptionRef.current
          if (transcript === null || !transcript.active || parsed.revision !== transcript.revision || parsed.voiceSessionId !== transcript.sessionId) return
          if (active) setVoiceCompletionSuffix(parsed.suffix ?? '')
        }
      } catch { return }
    }).then((cleanup) => active ? (unlisten = cleanup) : cleanup()).catch(() => undefined)
    return () => { active = false; unlisten?.() }
  }, [])

  const clearPartialTranscript = (): void => {
    partialTranscriptionAllowedRef.current = false
    if (partialTranscriptionRef.current !== null) {
      partialTranscriptionRef.current = { ...partialTranscriptionRef.current, active: false }
    }
    setPartialTranscript('')
    setVoiceCompletionSuffix('')
  }

  const clearCompletionDisplay = (): void => {
    completionLifecycleRef.current += 1
    completionRevisionRef.current += 1
    latestTypedCompletionRef.current = null
    setTypedCompletionSuffix('')
    setVoiceCompletionSuffix('')
  }

  const clearCompletion = (): void => {
    clearCompletionDisplay()
    void invokeTauriCommand('clear_completion').catch(() => undefined)
  }

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

      return [nextNotice, ...currentNotices].slice(0, MAX_NOTICE_QUEUE_LENGTH)
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
        if (!active || uiTextSizeHydrationOverriddenRef.current) {
          return
        }

        setUiTextSize(parseUiTextSize(payload))
      })
      .catch(() => undefined)

    return () => {
      active = false
    }
  }, [])

  useEffect(() => {
    if (getTauriInternals() === null) {
      return undefined
    }
    let active = true
    void invokeTauriCommand('get_assistant_settings').then((payload) => {
      if (active && assistantSettingsWriteRevisionRef.current === 0) {
        const parsed = parseAssistantSettings(payload)
        assistantSettingsRef.current = parsed
        persistedAssistantSettingsRef.current = parsed
        setAssistantSettings(parsed)
        assistantSettingsPendingRef.current = false
        setAssistantSettingsPending(false)
      }
    }).catch((error) => {
      if (active) {
        const message = toDisplayErrorMessage(error)
        setAssistantSettingsLoadError(message)
        addNotice({
          tone: 'error',
          title: 'Assistant settings unavailable',
          message,
        })
      }
    })
    return () => { active = false }
  }, [addNotice])

  useEffect(() => {
    const onEscape = (event: globalThis.KeyboardEvent) => {
      if (
        event.key === 'Escape' &&
        !settingsOpen &&
        startupState.kind === 'ready' &&
        startupState.promptCancellationAvailable
      ) {
        const active = promptRef.current
        if (active !== null && !active.terminal && promptState !== 'stopping') {
          cancelTts()
          setPromptState('stopping')
          void invokeTauriCommand('cancel_prompt', { requestId: active.id }).catch(() => {
            if (promptRef.current === active && !active.terminal) {
              setPromptState('executing')
            }
          })
        }
      }
    }
    window.addEventListener('keydown', onEscape)
    return () => window.removeEventListener('keydown', onEscape)
  }, [promptState, settingsOpen, startupState])

  useEffect(() => {
    document.documentElement.dataset['uiTheme'] = uiTheme

    return () => {
      delete document.documentElement.dataset['uiTheme']
    }
  }, [uiTheme])

  useEffect(() => {
    let active = true

    void invokeTauriCommand('get_ui_theme')
      .then((payload) => {
        if (!active || uiThemeHydrationOverriddenRef.current) {
          return
        }

        setUiTheme(parseUiTheme(payload))
      })
      .catch(() => undefined)

    return () => {
      active = false
    }
  }, [])

  const persistUiTextSize = async (nextTextSize: UiTextSize, previousTextSize: UiTextSize): Promise<void> => {
    const revision = ++uiTextSizeWriteRevisionRef.current
    uiTextSizeHydrationOverriddenRef.current = true
    setUiTextSize(nextTextSize)

    try {
      const write = uiTextSizeWriteChainRef.current.then(() =>
        invokeTauriCommand('set_ui_text_size', { textSize: nextTextSize }),
      )
      uiTextSizeWriteChainRef.current = write.then(() => undefined, () => undefined)
      const payload = await write
      if (revision === uiTextSizeWriteRevisionRef.current) {
        setUiTextSize(parseUiTextSize(payload))
      }
    } catch (error) {
      if (revision !== uiTextSizeWriteRevisionRef.current) return
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

  const persistUiTheme = async (nextTheme: UiTheme, previousTheme: UiTheme): Promise<void> => {
    const revision = ++uiThemeWriteRevisionRef.current
    uiThemeHydrationOverriddenRef.current = true
    setUiTheme(nextTheme)

    try {
      const write = uiThemeWriteChainRef.current.then(() =>
        invokeTauriCommand('set_ui_theme', { theme: nextTheme }),
      )
      uiThemeWriteChainRef.current = write.then(() => undefined, () => undefined)
      const payload = await write
      if (revision === uiThemeWriteRevisionRef.current) {
        setUiTheme(parseUiTheme(payload))
      }
    } catch (error) {
      if (revision !== uiThemeWriteRevisionRef.current) return
      setUiTheme(previousTheme)
      addNotice({
        tone: 'error',
        title: 'Theme not saved',
        message: toDisplayErrorMessage(error),
      })
    }
  }

  const toggleUiTheme = (): void => {
    const nextTheme = uiTheme === 'dark' ? 'light' : 'dark'
    void persistUiTheme(nextTheme, uiTheme)
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

  const waitForInFlightLiveAudioFrames = async (): Promise<void> => {
    while (appActiveRef.current && liveAudioInFlightFramesRef.current > 0) {
      await new Promise((resolve) => setTimeout(resolve, 10))
    }
  }

  const applyStartupState = (nextState: StartupState, syncRuntimeStatus = true): void => {
    startupStateRef.current = nextState
    setStartupState(nextState)
    if (nextState.kind === 'ready' || nextState.kind === 'warming_model') {
      ttsEnabledRef.current = nextState.ttsEnabled
      setTtsEnabled(nextState.ttsEnabled)
    } else {
      ttsEnabledRef.current = false
      setTtsEnabled(false)
    }

    if (syncRuntimeStatus) {
      const nextRuntimeStatus = startupStateToRuntimeStatus(nextState)
      runtimeStatusRef.current = nextRuntimeStatus
      setRuntimeStatus(nextRuntimeStatus)
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

        const previousState = startupStateRef.current
        const previousKind = previousState.kind
        const hydrated = previousKind !== 'loading'
        if (nextState.kind === 'error' && hydrated && previousKind === 'ready') {
          await new Promise((resolve) => window.setTimeout(resolve, 1_000))
          continue
        }
        if (isSwitchingResponseProfileRef.current) {
          await new Promise((resolve) => window.setTimeout(resolve, 500))
          continue
        }
        const syncRuntimeStatus = !hydrated
        applyStartupState(nextState, syncRuntimeStatus)

        if (nextState.kind === 'ready') {
          const newlyUnavailableCapabilities = nextState.capabilities.filter(({ id, state }) => {
            if (state !== 'not_configured' && state !== 'unavailable' && state !== 'failed') return false
            if (previousKind !== 'ready') return true
            const previousCapability = previousState.capabilities.find((capability) => capability.id === id)
            return previousCapability !== undefined && previousCapability.state !== state
          })
          if (newlyUnavailableCapabilities.length > 0) {
            addNotice({
              tone: 'error',
              title: 'Startup capabilities unavailable',
              message: newlyUnavailableCapabilities
                .map(({ id, reason }) => `${capabilityLabel(id)}: ${reason}`)
                .join('; '),
            })
          }
        }

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

        await new Promise((resolve) => window.setTimeout(
          resolve,
          isStartupStateSettled(nextState) ? 1_000 : 500,
        ))
      }
    })()

    return () => {
      active = false
    }
  }, [addNotice])

  useEffect(() => {
    appActiveRef.current = true
    return () => {
      appActiveRef.current = false
      cancelTts()
      if (wakeConfidenceHoldRef.current !== null) {
        window.clearTimeout(wakeConfidenceHoldRef.current)
      }
      liveAudioSessionIdRef.current += 1
      liveAudioStartAbortRef.current?.abort()
      liveAudioStartAbortRef.current = null
      liveAudioSourceRef.current?.stop()
      liveAudioSourceRef.current = null
    }
  }, [])

  useEffect(() => {
    if (!settingsOpen) {
      return
    }

    let active = true
    void listAudioInputDevices()
      .then((devices) => {
        if (active) setAudioInputDevices(devices)
      })
      .catch(() => {
        if (active) setAudioInputDevices([])
      })

    return () => {
      active = false
    }
  }, [settingsOpen])

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

  const assistantCapabilities = useMemo(() => capabilityMap(startupState), [startupState])
  const selectedInstantOption = instantOptions(assistantCapabilities)
    .find((option) => option.value === assistantSettings.instant)
  const selectedInstantRetryable = localInstantCanRetry(startupState, assistantSettings.instant)
  const selectedInstantProfileReady = instantMatchesResponseProfile(startupState, assistantSettings.instant)
  const canSend = useMemo(
    () =>
      startupState.kind === 'ready' &&
      selectedInstantOption?.available === true &&
      selectedInstantProfileReady &&
      !assistantSettingsPending &&
      !resetPending &&
      promptState === 'idle' &&
      runtimeStatus === 'sleeping' &&
      composerValue.trim().length > 0,
    [assistantSettingsPending, composerValue, promptState, resetPending, runtimeStatus, selectedInstantOption?.available, selectedInstantProfileReady, startupState.kind],
  )
  const updateInstallationDisabled =
    startupState.kind !== 'ready' ||
    runtimeStatus !== 'sleeping' ||
    promptState !== 'idle' ||
    resetPending ||
    isSwitchingResponseProfile ||
    assistantSettingsPending ||
    micStarting ||
    ttsPlaying ||
    pendingTtsCommands > 0

  const canToggleMic = voiceInputReady(startupState) && !micStarting
  const voiceInputUnavailableReason = startupState.kind === 'ready'
    ? (startupState.voiceInputError ?? (startupState.capabilities
      .filter((capability) => ['wake_word', 'vad', 'parakeet'].includes(capability.id) && capability.state !== 'available')
      .map((capability) => capability.reason ?? `${capabilityLabel(capability.id)} unavailable`)
      .join('; ') || 'required voice capabilities are unavailable'))
    : 'voice input is still starting'
  const cueAssetPaths =
    startupState.kind === 'ready'
      ? startupState.cueAssetPaths
      : DEFAULT_CUE_ASSET_PATHS
  const canToggleTts = startupState.kind === 'ready' &&
    capabilityIsAvailable(startupState, 'tts') && !isSwitchingResponseProfile
  const assistantControlsDisabled = startupState.kind !== 'ready' || assistantSettingsPending || isSwitchingResponseProfile
  const persistAssistantSettings = async (next: AssistantSettings): Promise<boolean> => {
    const revision = ++assistantSettingsWriteRevisionRef.current
    assistantSettingsPendingRef.current = true
    setAssistantSettingsPending(true)
    assistantSettingsRef.current = next
    setAssistantSettings(next)
    let resolveWrite: (settings: AssistantSettings) => void = () => undefined
    let rejectWrite: (error: unknown) => void = () => undefined
    const write = new Promise<AssistantSettings>((resolve, reject) => {
      resolveWrite = resolve
      rejectWrite = reject
    })
    assistantSettingsWriteChainRef.current = assistantSettingsWriteChainRef.current
      .then(async () => {
        const persisted = await invokeTauriCommand('set_assistant_settings', { settings: serializeAssistantSettings(next) })
        resolveWrite(parseAssistantSettings(persisted))
      })
      .catch(rejectWrite)
    try {
      const persisted = await write
      persistedAssistantSettingsRef.current = persisted
      if (revision === assistantSettingsWriteRevisionRef.current) {
        assistantSettingsRef.current = persisted
        setAssistantSettings(persisted)
      }
      return true
    } catch (error) {
      if (revision === assistantSettingsWriteRevisionRef.current) {
        assistantSettingsRef.current = persistedAssistantSettingsRef.current
        setAssistantSettings(persistedAssistantSettingsRef.current)
      }
      addNotice({ tone: 'error', title: 'Assistant settings failed', message: toDisplayErrorMessage(error) })
      return false
    } finally {
       if (revision === assistantSettingsWriteRevisionRef.current) {
         assistantSettingsPendingRef.current = false
         setAssistantSettingsPending(false)
       }
    }
  }
  const changeInstant = async (instant: AssistantSettings['instant']): Promise<void> => {
    const revision = ++instantSelectionRevisionRef.current
    const option = instantOptions(assistantCapabilities).find((item) => item.value === instant)
    if (!option?.available && !localInstantCanRetry(startupStateRef.current, instant)) return
    let previousProfile: ResponseProfile | null = null
    let switchedProfile = false
    if (instant === 'local-fast' || instant === 'local-quality') {
      const profile = instant === 'local-fast' ? 'fast' : 'quality'
      if (startupState.kind !== 'ready') return
      previousProfile = startupState.selectedResponseProfile
      if (previousProfile !== profile || localInstantCanRetry(startupStateRef.current, instant)) {
        if (!await switchResponseProfile(profile)) return
        switchedProfile = true
      }
      if (revision !== instantSelectionRevisionRef.current) return
      if (startupStateRef.current.kind !== 'ready' || startupStateRef.current.selectedResponseProfile !== profile) return
    }
    if (revision !== instantSelectionRevisionRef.current) return
    const persisted = await persistAssistantSettings({ ...assistantSettingsRef.current, instant })
    if (!persisted && revision === instantSelectionRevisionRef.current && switchedProfile && previousProfile !== null) {
      await switchResponseProfile(previousProfile)
    }
  }

  useEffect(() => {
    autoStopOnSilenceRef.current = autoStopOnSilence
  }, [autoStopOnSilence])

  useEffect(() => {
    isSwitchingResponseProfileRef.current = isSwitchingResponseProfile
  }, [isSwitchingResponseProfile])

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

    if (nextStatus === 'listening' && previousStatus !== 'listening') {
      resetWakeConfidence()
      expectedPartialSessionIdRef.current += 1
      partialTranscriptionRef.current = {
        sessionId: expectedPartialSessionIdRef.current,
        revision: 0,
        active: true,
      }
      partialTranscriptionAllowedRef.current = true
    } else if (nextStatus !== 'listening') {
      partialTranscriptionAllowedRef.current = false
      voiceActivityStateRef.current = createVoiceActivityState()
      resetWakeConfidence()
      clearPartialTranscript()
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

    if (nextStatus === 'listening' && previousStatus !== 'listening') {
      resetWakeConfidence()
      expectedPartialSessionIdRef.current += 1
      partialTranscriptionRef.current = {
        sessionId: expectedPartialSessionIdRef.current,
        revision: 0,
        active: true,
      }
      partialTranscriptionAllowedRef.current = true
    } else if (nextStatus !== 'listening') {
      partialTranscriptionAllowedRef.current = false
      voiceActivityStateRef.current = createVoiceActivityState()
      resetWakeConfidence()
      clearPartialTranscript()
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
    resetWakeConfidence()
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
    if (previousStatus === 'sleeping') {
      recordRuntimeControlTelemetry(runtimePhase)
    }

    if (
      nextStatus === 'sleeping' &&
      runtimePhase.telemetry?.wakeConfidence === null
    ) {
      resetWakeConfidence()
    } else if (
      nextStatus === 'sleeping' &&
      runtimePhase.telemetry?.wakeConfidence !== null &&
      runtimePhase.telemetry?.wakeConfidence !== undefined
    ) {
      updateWakeConfidence(runtimePhase.telemetry.wakeConfidence)
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
        id: 'system-transcription-ready',
        role: 'system',
        content: `transcription_ready:\n${runtimePhase.transcriptionReadySamples} samples captured`,
      })
    }

    if (runtimePhase.transcriptText !== null) {
      nextMessages.push({
        id: 'system-transcript',
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
      readonly isCurrent?: () => boolean
    } = {},
  ): Promise<RuntimeControlResult | null> => {
    if (startupStateRef.current.kind !== 'ready') {
      return null
    }

    const { args, fallbackEvent, quiet, isCurrent } = options

    try {
      const runtimePhase = await invokeRuntimeControl(command, args)
      if (isCurrent !== undefined && !isCurrent()) return null

      if (runtimePhase === null) {
        if (fallbackEvent !== undefined) {
          transitionFromCurrentStatus(fallbackEvent)
        }

        return null
      }

      applyRuntimeControlResult(runtimePhase, quiet === undefined ? {} : { quiet })
      return runtimePhase
    } catch (error) {
      if (isCurrent !== undefined && !isCurrent()) return null
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
    const currentStartupState = startupStateRef.current
    const currentSettings = assistantSettingsRef.current
    const selectedInstant = currentStartupState.kind === 'ready'
      ? instantOptions(capabilityMap(currentStartupState)).find((option) => option.value === currentSettings.instant)
      : undefined
    if (
      currentStartupState.kind !== 'ready' ||
      selectedInstant?.available !== true ||
      !instantMatchesResponseProfile(currentStartupState, currentSettings.instant) ||
      assistantSettingsPendingRef.current ||
      isSwitchingResponseProfileRef.current ||
      resetPendingRef.current ||
      promptRef.current !== null
    ) {
      return
    }

    if (prompt.trim().length === 0) {
      return
    }

    const currentStatus = runtimeStatusRef.current
    const executingStatus = applyTransition(currentStatus, 'submit_prompt')

    if (executingStatus === currentStatus) {
      return
    }
    clearPartialTranscript()
    cancelTts()

    // eslint-disable-next-line react-hooks/purity
    const requestId = `request-${Date.now()}-${Math.random().toString(36).slice(2)}`
    const assistantId = `assistant-${requestId}`
    promptRef.current = {
      id: requestId,
      assistantId,
      text: '',
      error: null,
      terminal: false,
      tts: ttsEnabledRef.current,
      corrected: false,
      stages: [
        { stage: 'instant', status: 'running' },
        ...(currentSettings.deepEnabled ? [{ stage: 'deep', status: 'queued' } as const] : []),
        ...(currentSettings.reviewEnabled ? [{ stage: 'review', status: 'queued' } as const] : []),
      ],
      priorVersions: [],
      sources: [],
      ttsFirstLineSpoken: false,
    }
    setPromptState('executing')
    setPromptActivity('Executing prompt…')
    setMessages((currentMessages) => [
      ...currentMessages,
      {
        id: `user-${Date.now()}`,
        role: 'user',
        content: prompt,
      },
    ])

    if (source === 'typed') {
      setComposerValue('')
      clearCompletionDisplay()
    }

    const handleEvent = (event: import('./types/chat').PromptExecutionEvent): void => {
      const active = promptRef.current
      if (active === null || active.id !== event.requestId || active.terminal) {
        return
      }
      if (event.kind === 'text') {
        active.text += event.text
        if (active.tts && ttsEnabledRef.current && !active.ttsFirstLineSpoken && active.text.includes('\n')) {
          const firstLine = firstNonEmptyStreamingLine(active.text)
          if (firstLine !== null && isValidTtsFirstLine(firstLine)) {
            active.ttsFirstLineSpoken = true
            void synthesizeAndPlayAssistantReply(firstLine)
          }
        }
        setMessages((current) => {
          const index = current.findIndex((message) => message.id === active.assistantId)
          if (index < 0) {
            return [...current, { id: active.assistantId, role: 'assistant', content: event.text, answerStage: { stages: active.stages, priorVersions: active.priorVersions, sources: active.sources } }]
          }
          return current.map((message, messageIndex) =>
            messageIndex === index
              ? { ...message, content: message.content + event.text, answerStage: { stages: active.stages, priorVersions: active.priorVersions, sources: active.sources } }
              : message,
          )
        })
      }
      if (event.kind === 'correction') {
        active.corrected = true
        if (active.text.trim().length > 0) {
          active.priorVersions.push({ id: `instant-${active.priorVersions.length + 1}`, text: active.text, label: 'Before correction' })
        }
        active.text = event.text
        active.stages = active.stages.map((stage) => stage.stage === event.stage ? { ...stage, status: 'corrected', detail: event.correction } : stage)
        setMessages((current) => {
          const corrected = {
            id: active.assistantId,
            role: 'assistant' as const,
            content: event.text,
             answerStage: { stages: active.stages, priorVersions: active.priorVersions, sources: active.sources },
          }
          return current.some((message) => message.id === active.assistantId)
            ? current.map((message) => message.id === active.assistantId ? { ...message, ...corrected } : message)
            : [...current, corrected]
        })
        setPromptActivity(event.correction)
        if (active.tts && ttsEnabledRef.current) {
          void synthesizeAndPlayAssistantReply(event.correction.replace(/^Correction:\s*/u, ''))
        }
      }
      if (event.kind === 'stage') {
        active.stages = active.stages.map((stage) => stage.stage === event.stage
          ? { stage: event.stage, status: event.status, ...(event.detail === undefined ? {} : { detail: event.detail }) }
          : stage)
        setMessages((current) => current.map((message) => message.id === active.assistantId
          ? { ...message, answerStage: { stages: active.stages, priorVersions: active.priorVersions, sources: active.sources } } : message))
      }
      if (event.kind === 'sources') {
        active.sources = [...event.sources]
        setMessages((current) => current.map((message) => message.id === active.assistantId
          ? { ...message, answerStage: { stages: active.stages, priorVersions: active.priorVersions, sources: active.sources } } : message))
      }
      if (event.kind === 'reasoning') {
        setPromptActivity(`Reasoning: ${event.text}`)
      }
      if (event.kind === 'status') {
        setPromptActivity(event.message)
      }
      if (event.kind === 'tool') {
        setPromptActivity(
          `${event.tool}: ${event.status}${event.detail ? ` - ${event.detail}` : ''}`,
        )
      }
      if (event.kind === 'error') {
        active.error = event.message
        setPromptActivity(event.message)
      }
    }
    try {
      const result = await executePrompt(requestId, prompt, handleEvent, source)
      const active = promptRef.current
      if (
        active === null ||
        active.id !== requestId ||
        result.requestId !== requestId ||
        active.terminal
      ) {
        return
      }
      active.terminal = true
      active.stages = active.stages.map((stage) => {
        if (result.outcome !== 'completed' && (stage.status === 'running' || stage.status === 'queued')) {
          return {
            ...stage,
            status: result.outcome === 'cancelled' ? 'cancelled' : 'failed',
          }
        }
        if (stage.stage === 'instant' && stage.status === 'running') return { ...stage, status: 'completed' }
        if (stage.status === 'queued') return { ...stage, status: 'stale', detail: 'Not required for this answer' }
        return stage
      })
      setMessages((current) => current.map((message) => message.id === active.assistantId
        ? { ...message, answerStage: { stages: active.stages, priorVersions: active.priorVersions, sources: active.sources } }
        : message))
      promptRef.current = null
      setPromptState('idle')
      setPromptActivity(null)
      applyRuntimeStatus(toRuntimeStatus(result.runtimePhase))
      if (result.outcome !== 'completed') {
        setMessages((current) =>
          current.filter(
            (message) => message.id !== assistantId || message.content.trim().length > 0,
          ),
        )
        addNotice({
          tone: result.outcome === 'error' ? 'error' : 'info',
          title: result.outcome === 'error' ? 'Response failed' : 'Response cancelled',
          message:
            result.outcome === 'error'
              ? result.errorMessage ?? active.error ?? 'The response could not be completed. Try again.'
              : 'The response was cancelled.',
        })
      } else if (active.text.trim().length === 0) {
        addNotice({
          tone: 'info',
          title: 'No response',
          message: 'No response was returned. Try again.',
        })
      } else if (active.tts && ttsEnabledRef.current && !active.corrected && !active.ttsFirstLineSpoken) {
        void synthesizeAndPlayAssistantReply(active.text)
      }
    } catch (error) {
      const message =
        error instanceof Error
          ? error.message
          : typeof error === 'string' && error.trim().length > 0
            ? error
            : 'Prompt execution failed'

      const active = promptRef.current
      if (active === null || active.id !== requestId) {
        return
      }
      active.terminal = true
      active.stages = active.stages.map((stage) =>
        stage.status === 'running' || stage.status === 'queued'
          ? { ...stage, status: 'failed', detail: message }
          : stage,
      )
      promptRef.current = null
      setMessages((current) => current
        .map((chatMessage) => chatMessage.id === active.assistantId
          ? { ...chatMessage, answerStage: { stages: active.stages, priorVersions: active.priorVersions, sources: active.sources } }
          : chatMessage)
        .filter((chatMessage) =>
          chatMessage.id !== active.assistantId || chatMessage.content.trim().length > 0,
        ))
      applyTransition(executingStatus, 'fail')
      addNotice({
        tone: 'error',
        title: 'Response failed',
        message,
      })
      recordRuntimeDiagnostic('execution', `Execution error: ${message}`)
      setPromptState('idle')
      setPromptActivity(null)
    }
  }

  const cancelPrompt = (): void => {
    const active = promptRef.current
    if (active === null || active.terminal || promptState === 'stopping') return
    clearPartialTranscript()
    cancelTts()
    setPromptState('stopping')
    void invokeTauriCommand('cancel_prompt', { requestId: active.id }).catch(() => {
      if (promptRef.current === active && !active.terminal) setPromptState('executing')
    })
  }

  const synthesizeAndPlayAssistantReply = async (text: string): Promise<void> => {
    let commandPending = false
    let playbackId: number | null = null
    cancelTts()
    const generation = ttsGenerationRef.current

    try {
      const speechText = firstNonEmptyCompletedLine(text) ?? ''
      if (!isValidTtsFirstLine(speechText)) return
      playbackId = parseNativeAudioRequestId(
        await invokeTauriCommand('reserve_local_tts_playback_id'),
        'TTS playback',
      )
      if (generation !== ttsGenerationRef.current || !ttsEnabledRef.current) return
      ttsPlaybackIdRef.current = playbackId
      setTtsPlaying(true)
      commandPending = true
      setPendingTtsCommands((count) => count + 1)
      const payload = await invokeTauriCommand('speak_local_tts', {
        text: speechText,
        playbackId,
      })
      if (generation !== ttsGenerationRef.current || !ttsEnabledRef.current) return

      if (
        !isRecord(payload) ||
        typeof payload['duration_ms'] !== 'number' ||
        payload['duration_ms'] < 0 ||
        !Number.isFinite(payload['duration_ms'])
      ) {
        throw new Error('TTS synthesis payload is invalid')
      }
    } catch (error) {
      if (generation !== ttsGenerationRef.current) return
      const message = toDisplayErrorMessage(error)

      addNotice({
        tone: 'error',
        title: 'TTS playback failed',
        message,
      })
      recordRuntimeDiagnostic('tts', `TTS error: ${message}`)
    } finally {
      if (commandPending) {
        setPendingTtsCommands((count) => Math.max(0, count - 1))
      }
      if (playbackId !== null && ttsPlaybackIdRef.current === playbackId) {
        ttsPlaybackIdRef.current = null
        setTtsPlaying(false)
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

  const handleMarkSilence = async (
    liveAudioSessionId: number,
    telemetryFrameId: string | null = null,
  ): Promise<void> => {
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

    if (liveAudioSessionId !== liveAudioSessionIdRef.current) return

    const runtimePhase = await syncRuntimeControl(
      'mark_silence',
      telemetryFrameId === null
        ? {
            fallbackEvent: 'end_listening',
            isCurrent: () => liveAudioSessionId === liveAudioSessionIdRef.current,
          }
        : {
            args: { telemetryFrameId },
            fallbackEvent: 'end_listening',
            isCurrent: () => liveAudioSessionId === liveAudioSessionIdRef.current,
          },
    )

    if (liveAudioSessionId !== liveAudioSessionIdRef.current) return
    clearPartialTranscript()
    maybeRunVoiceTranscript(runtimePhase)
  }

  const stopLiveAudio = (content: string | null = null): void => {
    liveAudioSessionIdRef.current += 1
    liveAudioStartAbortRef.current?.abort()
    liveAudioStartAbortRef.current = null
    liveAudioSourceRef.current?.stop()
    liveAudioSourceRef.current = null
    voiceActivityStateRef.current = createVoiceActivityState()
    resetWakeConfidence()
    setMicStarting(false)
    setMicActive(false)
    clearPartialTranscript()

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
    if (!voiceInputReady(startupStateRef.current) || liveAudioSourceRef.current !== null || micStarting || micStartInFlightRef.current) {
      return
    }
    micStartInFlightRef.current = true
    const startAbort = new AbortController()
    liveAudioStartAbortRef.current = startAbort

    const liveAudioSessionId = liveAudioSessionIdRef.current + 1
    liveAudioSessionIdRef.current = liveAudioSessionId

    setMicStarting(true)

    try {
      const liveAudioSource = await startLiveAudioSource({
        signal: startAbort.signal,
        ...(audioInputDeviceIdRef.current === null
          ? {}
          : { deviceId: audioInputDeviceIdRef.current }),
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
                  await handleMarkSilence(liveAudioSessionId, frameId)
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
        onStatus: (status) => {
          recordRuntimeDiagnostic('audio', `Microphone startup: ${status}`)
        },
        onSelectedDeviceFallback: (attemptedDeviceId) => {
          if (
            liveAudioSessionId !== liveAudioSessionIdRef.current ||
            audioInputDeviceIdRef.current !== attemptedDeviceId
          ) {
            return
          }
          audioInputDeviceIdRef.current = null
          setAudioInputDeviceId(null)
          persistAudioInputDeviceId(null)
        },
      })

      if (!appActiveRef.current || liveAudioSessionId !== liveAudioSessionIdRef.current) {
        liveAudioSource.stop()
        if (appActiveRef.current && voiceInputReady(startupStateRef.current)) {
          micAutoStartedRef.current = false
          queueMicrotask(() => void startMic())
        }
        return
      }

      liveAudioSourceRef.current = liveAudioSource
       setMicStarting(false)
       setMicActive(true)
       persistAudioInputDeviceId(audioInputDeviceIdRef.current)
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
        if (appActiveRef.current && voiceInputReady(startupStateRef.current)) {
          micAutoStartedRef.current = false
          queueMicrotask(() => void startMic())
        }
        return
      }

      setMicStarting(false)
      micStartInFlightRef.current = false
      reportLiveAudioError(error)
    } finally {
      if (liveAudioStartAbortRef.current === startAbort) {
        liveAudioStartAbortRef.current = null
      }
      micStartInFlightRef.current = false
    }
  }

  const toggleMic = (): void => {
    if (micActive) {
      stopLiveAudio('live_audio:\ndefault microphone stopped')
      return
    }

    void startMic()
  }

  const changeAudioInputDevice = (deviceId: string): void => {
    const selectedDeviceId = deviceId.length === 0 ? null : deviceId
    audioInputDeviceIdRef.current = selectedDeviceId
    setAudioInputDeviceId(selectedDeviceId)

    const shouldRestart = liveAudioSourceRef.current !== null
    stopLiveAudio()
    if (shouldRestart) {
      queueMicrotask(() => void startMic())
    }
  }

  const switchResponseProfile = async (profile: ResponseProfile): Promise<boolean> => {
    if (startupStateRef.current.kind !== 'ready') {
      return false
    }

    if (isSwitchingResponseProfileRef.current) {
      return false
    }

    const currentState = startupStateRef.current

    const profileInstant = profile === 'fast' ? 'local-fast' : 'local-quality'
    if (currentState.selectedResponseProfile === profile && !localInstantCanRetry(currentState, profileInstant)) {
      return true
    }

    isSwitchingResponseProfileRef.current = true
    cancelTts()
    clearPartialTranscript()
    setIsSwitchingResponseProfile(true)
    const requestedCapabilityId = profile === 'quality' ? 'local_quality' : 'local_fast'

    const shouldReportMicStopForSwitch =
      micActive || micStarting || liveAudioSourceRef.current !== null
    if (shouldReportMicStopForSwitch) micAutoStartedRef.current = false
    stopLiveAudio(
      shouldReportMicStopForSwitch
        ? 'live_audio:\ndefault microphone stopped for profile switch'
        : null,
    )

    await waitForInFlightLiveAudioFrames()

    const settleStartupState = async (): Promise<StartupState> => {
      while (true) {
        if (!appActiveRef.current) {
          return { kind: 'loading' }
        }

        const nextState = await loadStartupState()

        if (!appActiveRef.current) {
          return { kind: 'loading' }
        }

        if (nextState.kind === 'ready') {
          const requestedCapability = nextState.capabilities.find(({ id }) => id === requestedCapabilityId)
          if (requestedCapability !== undefined && requestedCapability.state !== 'warming') {
            applyStartupState(nextState, true)
            return nextState
          }
        }
        if (nextState.kind === 'error') {
          applyStartupState(nextState, false)
          return nextState
        }
        if (isStartupStateSettled(nextState)) {
          return nextState
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
      promptCancellationAvailable: currentState.promptCancellationAvailable,
      ttsEnabled: currentState.ttsEnabled,
      ttsOutputGainDb: currentState.ttsOutputGainDb,
      capabilities: currentState.capabilities,
    }

    startupStateRef.current = warmingState
    setStartupState(warmingState)
    runtimeStatusRef.current = 'initializing'
    setRuntimeStatus('initializing')

    try {
       await invokeTauriCommand('switch_response_profile', { profile })

       const settled = await settleStartupState()
      const requestedCapability = settled.kind === 'ready'
        ? settled.capabilities.find(({ id }) => id === requestedCapabilityId)
        : undefined
      if (settled.kind === 'ready' &&
        (settled.selectedResponseProfile !== profile || requestedCapability?.state !== 'available')) {
        const message = requestedCapability?.reason ?? `The ${getResponseProfileLabel(profile)} profile could not be started.`
        addNotice({ tone: 'error', title: 'Profile switch failed', message })
        recordRuntimeDiagnostic('profile', `Response profile switch error: ${message}`)
        applyStartupState(currentState)
        return false
      }
      if (settled.kind === 'error') {
        return false
      }
      if (settled.kind !== 'ready' || settled.selectedResponseProfile !== profile) {
        applyStartupState(currentState)
        return false
      }
      return settled.kind === 'ready' && settled.selectedResponseProfile === profile
    } catch (error) {
      try {
        const settled = await settleStartupState()
        if (settled.kind === 'ready' || settled.kind === 'warming_model') {
          applyStartupState(currentState)
        }
      } catch {
        if (appActiveRef.current) {
          applyStartupState(currentState)
        }
      }

      if (appActiveRef.current && startupStateRef.current.kind === 'warming_model') {
        applyStartupState(currentState)
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
      return false
    } finally {
      isSwitchingResponseProfileRef.current = false
      if (appActiveRef.current) {
        setIsSwitchingResponseProfile(false)
      }
    }
  }

  const toggleTts = async (enabled: boolean): Promise<void> => {
    const revision = ++ttsWriteRevisionRef.current
    const previousEnabled = ttsEnabled
    if (!enabled) cancelTts()

    try {
      const write = ttsWriteChainRef.current.then(() =>
        invokeTauriCommand('set_tts_enabled', { enabled }),
      )
      ttsWriteChainRef.current = write.then(() => undefined, () => undefined)
      const payload = await write

      if (revision !== ttsWriteRevisionRef.current) return
      if (
        typeof payload === 'object' &&
        payload !== null &&
        'enabled' in payload &&
        typeof payload.enabled === 'boolean'
      ) {
        ttsEnabledRef.current = payload.enabled
        setTtsEnabled(payload.enabled)
      } else {
        ttsEnabledRef.current = enabled
        setTtsEnabled(enabled)
      }
    } catch (error) {
      if (revision !== ttsWriteRevisionRef.current) return
      const message = toDisplayErrorMessage(error)

      ttsEnabledRef.current = previousEnabled
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
      !voiceInputReady(startupState) ||
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
    if (startupState.kind !== 'ready' || selectedInstantOption?.available !== true || assistantSettingsPending || isSwitchingResponseProfileRef.current || resetPendingRef.current || promptState !== 'idle' || runtimeStatusRef.current !== 'sleeping') {
      return
    }

    if (composerValue.trim().length === 0) {
      return
    }

    await runPrompt(composerValue, 'typed')
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

  const acceptTypedCompletion = (suffix: string): void => {
    if (suffix.length === 0) return
    const nextValue = `${composerValue}${suffix}`
    setComposerValue(nextValue)
    clearCompletionDisplay()
  }

  const selectedDeepOption = deepOptions(assistantCapabilities).find((option) => option.value === assistantSettings.deep)
  const selectedReviewOption = reviewOptions(assistantCapabilities).find((option) => option.value === assistantSettings.review)
  const canEnableDeep = selectedDeepOption?.available === true
  const canEnableReview = selectedReviewOption?.available === true

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

  const resetSession = async (): Promise<void> => {
    if (resetPendingRef.current) return
    resetPendingRef.current = true
    setResetPending(true)
    stopLiveAudio()
    const activePrompt = promptRef.current
    if (activePrompt !== null && !activePrompt.terminal) {
      activePrompt.terminal = true
      setPromptState('stopping')
      try {
        await invokeTauriCommand('cancel_prompt', { requestId: activePrompt.id })
      } catch {
        // reset_session remains authoritative for backend prompt settlement.
      }
    }
    promptRef.current = null
    cancelTts()
    clearPartialTranscript()
    clearCompletion()
    try {
      await waitForInFlightLiveAudioFrames()
      await invokeTauriCommand('reset_session')
      setMessages(getInitialMessages())
      setPromptActivity(null)
      setPromptState('idle')
      applyRuntimeStatus('sleeping')
    } catch (error) {
      setPromptActivity(null)
      setPromptState('idle')
      addNotice({ tone: 'error', title: 'Reset failed', message: toDisplayErrorMessage(error) })
    } finally {
      resetPendingRef.current = false
      setResetPending(false)
    }
  }

  const uiTextSizeIndex = Math.max(0, UI_TEXT_SIZE_STEPS.indexOf(uiTextSize))
  const canDecreaseTextSize = uiTextSizeIndex > 0
  const canIncreaseTextSize = uiTextSizeIndex < UI_TEXT_SIZE_STEPS.length - 1
  const nextUiThemeLabel = uiTheme === 'dark' ? 'light' : 'dark'
  const themeToggleLabel = `Switch to ${nextUiThemeLabel} mode`

  return (
    <div className="shell" data-ui-text-size={uiTextSize} data-ui-theme={uiTheme}>

      <main ref={conversationRef} className="conversation" aria-live="polite">
        {visibleMessages.map((message) => (
          message.answerStage && message.role === 'assistant' ? (
            <AnswerStage key={message.id} answer={message.content} className="message message--assistant" {...message.answerStage} />
          ) : <ChatBubble key={message.id} message={message} />
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
            <button type="button" className="shell__control" onClick={() => void resetSession()} aria-label="Reset Session" disabled={resetPending}>{resetPending ? 'Resetting...' : 'Reset Session'}</button>
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
            <div className="settings-panel__row">
              <div>
                <label htmlFor="audioInputDevice"><strong>Microphone</strong></label>
                <p className="settings-panel__hint">Choose the input used for wake-word listening.</p>
              </div>
              <select
                id="audioInputDevice"
                aria-label="Microphone"
                value={audioInputDeviceId ?? ''}
                disabled={micStarting}
                onChange={(event) => changeAudioInputDevice(event.target.value)}
              >
                <option value="">System selected microphone</option>
                {audioInputDevices.map((device) => (
                  <option key={device.deviceId} value={device.deviceId}>{device.label}</option>
                ))}
              </select>
            </div>
            <div className="settings-panel__assistant">
              <label><strong>Instant</strong><select id="assistantInstantSelect" aria-label="Instant" value={assistantSettings.instant} disabled={assistantControlsDisabled} onChange={(event) => void changeInstant(event.target.value as AssistantSettings['instant'])}>
                {instantOptions(assistantCapabilities).map((option) => <option key={option.value} value={option.value} disabled={!option.available && !localInstantCanRetry(startupState, option.value)} title={option.reason}>{option.label}{option.available ? '' : ` — ${option.reason}`}</option>)}
              </select></label>
              {selectedInstantRetryable ? <button type="button" aria-label="Retry local profile" onClick={() => void changeInstant(assistantSettings.instant)}>Retry local profile</button> : null}
              <label><strong>Deep model</strong><select aria-label="Deep model" value={assistantSettings.deep} disabled={assistantControlsDisabled} onChange={(event) => {
                const value = event.target.value as AssistantSettings['deep']
                if (deepOptions(assistantCapabilities).find((option) => option.value === value)?.available) {
                  void persistAssistantSettings({ ...assistantSettingsRef.current, deep: value })
                }
              }}>
                {deepOptions(assistantCapabilities).map((option) => <option key={option.value} value={option.value} disabled={!option.available} title={option.reason}>{option.label}</option>)}
              </select></label>
              <label><strong>Review model</strong><select aria-label="Review model" value={assistantSettings.review} disabled={assistantControlsDisabled} onChange={(event) => {
                const value = event.target.value as AssistantSettings['review']
                if (reviewOptions(assistantCapabilities).find((option) => option.value === value)?.available) {
                  void persistAssistantSettings({ ...assistantSettingsRef.current, review: value })
                }
              }}>
                {reviewOptions(assistantCapabilities).map((option) => <option key={option.value} value={option.value} disabled={!option.available} title={option.reason}>{option.label}</option>)}
              </select></label>
              <label><input type="checkbox" checked={assistantSettings.deepEnabled} disabled={assistantControlsDisabled || (!assistantSettings.deepEnabled && !canEnableDeep)} onChange={(event) => void persistAssistantSettings({ ...assistantSettingsRef.current, deepEnabled: event.target.checked })} /> Deep</label>
              <label><input type="checkbox" checked={assistantSettings.reviewEnabled} disabled={assistantControlsDisabled || (!assistantSettings.reviewEnabled && !canEnableReview)} onChange={(event) => void persistAssistantSettings({ ...assistantSettingsRef.current, reviewEnabled: event.target.checked })} /> Review</label>
              <label><input type="checkbox" checked={assistantSettings.completion} disabled={assistantControlsDisabled} onChange={(event) => {
                if (!event.target.checked) clearCompletion()
                void persistAssistantSettings({ ...assistantSettingsRef.current, completion: event.target.checked })
              }} /> Completion</label>
              <label><input type="checkbox" checked={assistantSettings.prefetch} disabled={assistantControlsDisabled} onChange={(event) => void persistAssistantSettings({ ...assistantSettingsRef.current, prefetch: event.target.checked })} /> Prefetch</label>
              <p className="settings-panel__hint">Prefetch may transmit unaccepted predicted text when enabled.</p>
            </div>
            <UpdateSettings updates={appUpdates} installationDisabled={updateInstallationDisabled} />
          </section>
        </div>
      ) : null}

      <form className="composer" onSubmit={onSubmit}>
        <PromptComposer
          id="promptComposer"
          className="composer__input"
          aria-label="Prompt"
          value={composerValue}
          partialTranscript={partialTranscript}
          ghostSuffix={typedCompletionSuffix}
           onChange={(event) => {
             const nextValue = event.target.value
             setComposerValue(nextValue)
             setTypedCompletionSuffix('')
             setVoiceCompletionSuffix('')
              completionRevisionRef.current += 1
              const revision = completionRevisionRef.current
              if (nextValue.length === 0) {
                latestTypedCompletionRef.current = null
                void invokeTauriCommand('clear_completion').catch(() => undefined)
              } else if (assistantSettings.completion && !assistantSettingsPendingRef.current && startupStateRef.current.kind === 'ready' && capabilityIsAvailable(startupStateRef.current, 'qwen_prediction')) {
                const pending = { revision, lifecycle: completionLifecycleRef.current }
                latestTypedCompletionRef.current = pending
                void invokeTauriCommand('request_completion', { revision, prompt: nextValue }).catch(() => {
                  if (latestTypedCompletionRef.current === pending) latestTypedCompletionRef.current = null
                })
              } else {
                latestTypedCompletionRef.current = null
              }
           }}
          onKeyDown={onComposerKeyDown}
          onAcceptCompletion={acceptTypedCompletion}
            onDismissCompletion={() => { clearCompletion() }}
           partialCompletionSuffix={voiceCompletionSuffix}
          placeholder="Type a prompt..."
          rows={3}
        />
        {startupState.kind === 'error' ? (
          <p className="shell__error">Startup error: {startupState.message}</p>
        ) : null}
        {assistantSettingsLoadError !== null ? (
          <p className="shell__error">Assistant settings unavailable: {assistantSettingsLoadError}</p>
        ) : null}
        {startupState.kind === 'ready' && !voiceInputReady(startupState) ? (
          <p className="shell__error">
            Voice input unavailable: {voiceInputUnavailableReason}
          </p>
        ) : null}
        {startupState.kind === 'ready' && (selectedInstantOption?.available !== true || !selectedInstantProfileReady) ? (
          <p className="shell__error" role="status">
            Sending disabled: {selectedInstantOption?.available !== true
              ? selectedInstantOption?.reason ?? 'selected provider is unavailable'
              : 'selected local model profile is not loaded'}
          </p>
        ) : null}
        {startupState.kind === 'warming_model' ? (
          <p className="shell__loading">Model loading: {startupState.message}</p>
        ) : null}
        <div className="composer__actions">
          <label className="shell__toggle">
            <input
              type="checkbox"
              checked={autoStopOnSilence}
              onChange={(event) => setAutoStopOnSilence(event.target.checked)}
              disabled={!voiceInputReady(startupState)}
              aria-describedby={!voiceInputReady(startupState) ? 'voice-input-help' : undefined}
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
            aria-describedby={!voiceInputReady(startupState) ? 'voice-input-help' : undefined}
          >
            {micStarting ? 'Starting mic...' : micActive ? 'Stop mic' : 'Start mic'}
          </button>
          {!voiceInputReady(startupState) ? (
            <span id="voice-input-help" className="sr-only">{voiceInputUnavailableReason}</span>
          ) : null}
          <div className="composer__send-side">
            {promptState !== 'idle' &&
            startupState.kind === 'ready' &&
            startupState.promptCancellationAvailable ? (
              <button
                type="button"
                className="shell__control"
                onClick={cancelPrompt}
                disabled={promptState === 'stopping'}
              >
                {promptState === 'stopping' ? 'Stopping…' : 'Stop'}
              </button>
            ) : null}
            {runtimeStatus === 'sleeping' && wakeConfidence !== null ? (
              <div className={wakeConfidence >= 0.7 ? 'composer__wake composer__wake--high' : wakeConfidence >= 0.4 ? 'composer__wake composer__wake--medium' : 'composer__wake composer__wake--low'}>
                <span className="composer__wake-primary">wake: {(wakeConfidence * 100).toFixed(2)}%</span>
              </div>
            ) : null}
            <button
              type="button"
              className="shell__control shell__theme-button"
              onClick={toggleUiTheme}
              aria-label={themeToggleLabel}
              title={themeToggleLabel}
            >
              {uiTheme === 'dark' ? '☀' : '☾'}
            </button>
            <button
              ref={settingsButtonRef}
              type="button"
              className="shell__control shell__settings-button"
               onClick={() => flushSync(() => setSettingsOpen(true))}
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
          {promptActivity !== null ? <div className="composer__status" role="status" aria-live="polite">{promptActivity}</div> : null}
        </div>
      </form>
    </div>
  )
}

function toRuntimeStatus(runtimePhase: BackendRuntimePhase): RuntimeStatus {
  return runtimePhase
}

function getResponseProfileLabel(profile: ResponseProfile): 'Fast' | 'Quality' {
  return profile === 'fast' ? 'Fast' : 'Quality'
}

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

function parseUiTheme(value: unknown): UiTheme {
  if (value === 'light' || value === 'dark') {
    return value
  }

  return DEFAULT_UI_THEME
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

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null
}

function parseNativeAudioRequestId(payload: unknown, label: string): number {
  if (typeof payload !== 'number' || !Number.isSafeInteger(payload) || payload <= 0) {
    throw new Error(`${label} ID is invalid`)
  }
  return payload
}

function capabilityIsAvailable(
  state: StartupState,
  id: import('./types/chat').CapabilityId,
): boolean {
  return (state.kind === 'ready' || state.kind === 'warming_model') &&
    state.capabilities.some((capability) => capability.id === id && capability.state === 'available')
}

function localInstantCanRetry(
  state: StartupState,
  instant: AssistantSettings['instant'],
): boolean {
  const capabilityId = instant === 'local-fast'
    ? 'local_fast'
    : instant === 'local-quality'
      ? 'local_quality'
      : null
  return capabilityId !== null && state.kind === 'ready' &&
    state.capabilities.some((capability) => capability.id === capabilityId && capability.state === 'failed')
}

function hasAvailableCapabilities(
  state: StartupState,
  ids: readonly import('./types/chat').CapabilityId[],
): boolean {
  return ids.every((id) => capabilityIsAvailable(state, id))
}

function capabilityMap(state: StartupState): import('./lib/assistantSettings').AssistantCapabilities {
  const available = (id: import('./types/chat').CapabilityId): boolean => capabilityIsAvailable(state, id)
  return { localFast: available('local_fast'), localQuality: available('local_quality'), custom: available('custom_provider'), openCode: available('opencode'), qwenPrediction: available('qwen_prediction'), deep: available('deep'), review: available('review') }
}

function capabilityLabel(id: import('./types/chat').CapabilityId): string {
  return id.split('_').map((part) => part[0]?.toUpperCase() + part.slice(1)).join(' ')
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

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { FormEvent, KeyboardEvent } from 'react'
import { flushSync } from 'react-dom'
import { ChatBubble } from './components/ChatBubble'
import { AnswerStage } from './components/AnswerStage'
import type { AnswerSource, AnswerStageStatusEntry, AnswerPriorVersion } from './components/AnswerStage'
import { PromptComposer } from './components/PromptComposer'
import { UserNoticeToast } from './components/UserNoticeToast'
import { playCue } from './lib/audioCues'
import { shouldSubmitComposer } from './lib/composer'
import { listAudioInputDevices, startLiveAudioSource } from './lib/liveAudioSource'
import type { AudioInputDevice, LiveAudioSource } from './lib/liveAudioSource'
import { executePrompt } from './lib/promptExecution'
import { loadAppSnapshot } from './lib/appSnapshot'
import type { AppSnapshot, Capability, ModelOption, Turn, ValidatedPreferences } from './lib/appSnapshot'
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
import { getTauriInternals, invokeTauriCommand } from './lib/tauri'
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

type PromptProjection = {
  id: string
  userId: string
  assistantId: string
  text: string
  error: string | null
  terminal: boolean
  tts: boolean
  corrected: boolean
  stages: AnswerStageStatusEntry[]
  priorVersions: AnswerPriorVersion[]
  sources: AnswerSource[]
  ttsFirstLineSpoken: boolean
}

function App() {
  const [startupState, setStartupState] = useState<StartupState>({ kind: 'loading' })
  const [runtimeStatus, setRuntimeStatus] = useState<RuntimeStatus>('initializing')
  const [composerValue, setComposerValue] = useState('')
  const [partialTranscript, setPartialTranscript] = useState('')
  const [typedCompletionSuffix, setTypedCompletionSuffix] = useState('')
  const [voiceCompletionSuffix, setVoiceCompletionSuffix] = useState('')
  const [autoStopOnSilence, setAutoStopOnSilence] = useState(true)
  const [ttsEnabled, setTtsEnabled] = useState(false)
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
  const [snapshotHydrationState, setSnapshotHydrationState] = useState<'unavailable' | 'pending' | 'failed' | 'succeeded'>(
    () => getTauriInternals() === null ? 'unavailable' : 'pending',
  )
  const [snapshotControlsFresh, setSnapshotControlsFresh] = useState(() => getTauriInternals() === null)
  const [snapshotControlsInvalidated, setSnapshotControlsInvalidated] = useState(false)
  const snapshotHydrationStateRef = useRef(snapshotHydrationState)
  const snapshotControlsFreshRef = useRef(getTauriInternals() === null)
  const snapshotControlsInvalidatedRef = useRef(false)
  const snapshotHydrationRevisionRef = useRef(0)
  const [assistantSettingsLoadError, setAssistantSettingsLoadError] = useState<string | null>(null)
  const [assistantSettings, setAssistantSettings] = useState<AssistantSettings>(DEFAULT_ASSISTANT_SETTINGS)
  const [snapshotModelOptions, setSnapshotModelOptions] = useState<readonly ModelOption[] | null>(null)
  const snapshotModelOptionsRef = useRef<readonly ModelOption[] | null>(null)
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
  const [responseAnnouncement, setResponseAnnouncement] = useState('')
  const [promptActivity, setPromptActivity] = useState<string | null>(null)
  const [promptState, setPromptState] = useState<'idle' | 'executing' | 'stopping' | 'reconciling'>('idle')
  const [resetPending, setResetPending] = useState(false)
  const promptRef = useRef<PromptProjection | null>(null)
  const promptProjectionRevisionRef = useRef(0)
  const visibleMessages = useMemo(
    () => messages.filter(isTranscriptMessage),
    [messages],
  )
  const conversationRef = useRef<HTMLElement | null>(null)
  const settingsButtonRef = useRef<HTMLButtonElement | null>(null)
  const settingsPanelRef = useRef<HTMLElement | null>(null)
  const settingsCloseButtonRef = useRef<HTMLButtonElement | null>(null)
  const liveAudioSourceRef = useRef<LiveAudioSource | null>(null)
  const audioInputDeviceIdRef = useRef(audioInputDeviceId)
  const liveAudioSessionIdRef = useRef(0)
  const liveAudioInFlightFramesRef = useRef(0)
  const initialSnapshotPreferencesRef = useRef<ValidatedPreferences | null>(null)
  const snapshotCapabilitiesRef = useRef<readonly import('./types/chat').StartupCapability[] | null>(null)
  const snapshotResponseProfileRef = useRef<ResponseProfile | null>(null)
  const snapshotSessionIdRef = useRef<string | null>(null)
  const uiTextSizePersistedRevisionRef = useRef(0)
  const uiThemePersistedRevisionRef = useRef(0)
  const uiTextSizePersistedValueRef = useRef<UiTextSize>(DEFAULT_UI_TEXT_SIZE)
  const uiThemePersistedValueRef = useRef<UiTheme>(DEFAULT_UI_THEME)
  const uiTextSizePersistedInitializedRef = useRef(false)
  const uiThemePersistedInitializedRef = useRef(false)
  const uiTextSizePendingRevisionRef = useRef<number | null>(null)
  const uiThemePendingRevisionRef = useRef<number | null>(null)
  const isSwitchingResponseProfileRef = useRef(false)
  const appActiveRef = useRef(true)
  const autoStopOnSilenceRef = useRef(true)
  const micAutoStartedRef = useRef(false)
  const micStartInFlightRef = useRef(false)
  const runtimeStatusRef = useRef<RuntimeStatus>('initializing')
  const runtimeActivityRevisionRef = useRef(0)
  const snapshotRuntimeHydratedRef = useRef(false)
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
  const transcriptRevisionRef = useRef(0)
  const canonicalTranscriptRef = useRef<readonly Turn[]>([])
  const ttsGenerationRef = useRef(0)
  const ttsEnabledRef = useRef(false)
  const ttsSourceRef = useRef<AudioBufferSourceNode | null>(null)
  const ttsPersistedRevisionRef = useRef(0)
  const ttsPersistedValueRef = useRef(false)
  const ttsPersistedInitializedRef = useRef(false)
  const ttsPendingRevisionRef = useRef<number | null>(null)
  const uiTextSizeWriteRevisionRef = useRef(0)
  const uiThemeWriteRevisionRef = useRef(0)
  const ttsWriteRevisionRef = useRef(0)
  const uiTextSizeWriteChainRef = useRef<Promise<void>>(Promise.resolve())
  const uiThemeWriteChainRef = useRef<Promise<void>>(Promise.resolve())
  const ttsWriteChainRef = useRef<Promise<void>>(Promise.resolve())
  const wakeConfidenceRef = useRef<number | null>(null)
  const pendingWakeConfidenceRef = useRef<number | null>(null)
  const wakeConfidenceHoldRef = useRef<number | null>(null)

  const publishRuntimeStatus = useCallback((nextStatus: RuntimeStatus, claimActivity: boolean): RuntimeStatus => {
    if (claimActivity) runtimeActivityRevisionRef.current += 1
    const previousStatus = runtimeStatusRef.current
    runtimeStatusRef.current = nextStatus
    setRuntimeStatus(nextStatus)
    return previousStatus
  }, [])

  const recoverOrphanedRequest = useCallback(async (
    initialSnapshot: AppSnapshot,
    transcriptRevision: number,
    isCurrent: () => boolean,
  ): Promise<void> => {
    let latest = initialSnapshot
    let latestRuntimeActivityRevision: number | null = null
    for (let attempt = 0; attempt < 3 && latest.activeRequest !== null; attempt += 1) {
      if (!appActiveRef.current || !isCurrent() || transcriptRevision !== transcriptRevisionRef.current) return
      await invokeTauriCommand('cancel_prompt', { requestId: latest.activeRequest.requestId }).catch(() => undefined)
      if (!appActiveRef.current || !isCurrent() || transcriptRevision !== transcriptRevisionRef.current) return
      try {
        const runtimeActivityRevision = runtimeActivityRevisionRef.current
        latest = await loadAppSnapshot()
        latestRuntimeActivityRevision = runtimeActivityRevision
      } catch {
        if (attempt < 2) await new Promise((resolve) => window.setTimeout(resolve, 250 * (attempt + 1)))
      }
      if (!appActiveRef.current || !isCurrent() || transcriptRevision !== transcriptRevisionRef.current) return
      if (latest.activeRequest !== null && attempt < 2) {
        await new Promise((resolve) => window.setTimeout(resolve, 250 * (attempt + 1)))
      }
    }
    if (!appActiveRef.current || !isCurrent() || transcriptRevision !== transcriptRevisionRef.current) return
    if (latest.activeRequest !== null) {
      setPromptActivity('Session recovery required. Use Reset Session to retry.')
      setPromptState('reconciling')
      return
    }
    canonicalTranscriptRef.current = latest.transcript
    snapshotSessionIdRef.current = latest.sessionId
    setMessages((current) => replaceCommittedTranscript(current, latest.transcript))
    snapshotRuntimeHydratedRef.current = true
    if (latestRuntimeActivityRevision === runtimeActivityRevisionRef.current) {
      publishRuntimeStatus(toRuntimeStatus(latest.runtimePhase), false)
    }
    setPromptActivity(null)
    setPromptState('idle')
  }, [publishRuntimeStatus])

  const cancelTts = (): void => {
    ttsGenerationRef.current += 1
    try {
      ttsSourceRef.current?.stop()
    } catch {
      // An already-ended source is stale by definition.
    }
    ttsSourceRef.current = null
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
        expectedPartialSessionIdRef.current = parsed.sessionId
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

  const synchronizeHydratedRuntimeStatus = useCallback((nextStatus: RuntimeStatus): void => {
    publishRuntimeStatus(nextStatus, false)
    if (nextStatus === 'listening') {
      partialTranscriptionRef.current = null
      partialTranscriptionAllowedRef.current = true
      return
    }

    partialTranscriptionAllowedRef.current = false
    if (partialTranscriptionRef.current !== null) {
      partialTranscriptionRef.current = { ...partialTranscriptionRef.current, active: false }
    }
    setPartialTranscript('')
    setVoiceCompletionSuffix('')
  }, [publishRuntimeStatus])

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

  const adoptSnapshotPreferences = useCallback((preferences: ValidatedPreferences): void => {
    setAssistantSettingsLoadError(null)
    initialSnapshotPreferencesRef.current = preferences
    if (uiTextSizePersistedRevisionRef.current === 0) {
      uiTextSizePersistedValueRef.current = preferences.uiTextSize
      uiTextSizePersistedInitializedRef.current = true
      if (uiTextSizePendingRevisionRef.current === null) setUiTextSize(preferences.uiTextSize)
    }
    if (uiThemePersistedRevisionRef.current === 0) {
      uiThemePersistedValueRef.current = preferences.uiTheme
      uiThemePersistedInitializedRef.current = true
      if (uiThemePendingRevisionRef.current === null) setUiTheme(preferences.uiTheme)
    }
    if (ttsPersistedRevisionRef.current === 0) {
      ttsPersistedValueRef.current = preferences.ttsEnabled
      ttsPersistedInitializedRef.current = true
      if (ttsPendingRevisionRef.current === null) {
        ttsEnabledRef.current = preferences.ttsEnabled
        setTtsEnabled(preferences.ttsEnabled)
      }
    }
    if (assistantSettingsWriteRevisionRef.current === 0) {
      const settings: AssistantSettings = {
        instant: preferences.instant,
        deep: preferences.deep,
        review: preferences.review,
        deepEnabled: preferences.deepEnabled,
        reviewEnabled: preferences.reviewEnabled,
        prefetch: preferences.prefetch,
        completion: preferences.completion,
      }
      assistantSettingsRef.current = settings
      persistedAssistantSettingsRef.current = settings
      setAssistantSettings(settings)
      assistantSettingsPendingRef.current = false
      setAssistantSettingsPending(false)
    }
  }, [])

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
    if (snapshotHydrationStateRef.current !== 'pending') return undefined
    let active = true
    const transcriptRevision = transcriptRevisionRef.current
    const runtimeActivityRevision = runtimeActivityRevisionRef.current

    void loadAppSnapshot().then(async (snapshot) => {
      if (!active || transcriptRevision !== transcriptRevisionRef.current) return
      snapshotHydrationRevisionRef.current += 1

      canonicalTranscriptRef.current = snapshot.transcript
      const activeRequestMessage: ChatMessage[] = snapshot.activeRequest === null ? [] : [{
        id: `recovered-user-${snapshot.activeRequest.executionId.sessionId}-${snapshot.activeRequest.executionId.generation}`,
        role: 'user',
        content: snapshot.activeRequest.prompt,
      }]
      setMessages((current) => [
        ...replaceCommittedTranscript(current, snapshot.transcript),
        ...activeRequestMessage,
      ])
      snapshotRuntimeHydratedRef.current = true
      if (runtimeActivityRevision === runtimeActivityRevisionRef.current) {
        synchronizeHydratedRuntimeStatus(toRuntimeStatus(snapshot.runtimePhase))
      }
      adoptSnapshotPreferences(snapshot.preferences)
      const snapshotCapabilities = toStartupCapabilities(snapshot.capabilities)
      snapshotCapabilitiesRef.current = snapshotCapabilities
      snapshotResponseProfileRef.current = snapshot.preferences.responseProfile
      snapshotSessionIdRef.current = snapshot.sessionId
      snapshotModelOptionsRef.current = snapshot.modelOptions
      setSnapshotModelOptions(snapshot.modelOptions)
      const controlledStartupState = applySnapshotControlState(
        startupStateRef.current,
        snapshotCapabilities,
        snapshot.preferences.responseProfile,
      )
      startupStateRef.current = controlledStartupState
      setStartupState(controlledStartupState)
      snapshotHydrationStateRef.current = 'succeeded'
      setSnapshotHydrationState('succeeded')
      snapshotControlsFreshRef.current = true
      setSnapshotControlsFresh(true)
      snapshotControlsInvalidatedRef.current = false
      setSnapshotControlsInvalidated(false)
      if (snapshot.activeRequest === null || transcriptRevision !== transcriptRevisionRef.current) return

      setPromptState('stopping')
      setPromptActivity('Recovering interrupted response…')
      await recoverOrphanedRequest(
        snapshot,
        transcriptRevision,
        () => active && !resetPendingRef.current,
      )
    }).catch(() => {
      if (active) {
        snapshotHydrationStateRef.current = 'failed'
        setSnapshotHydrationState('failed')
        snapshotControlsFreshRef.current = true
        setSnapshotControlsFresh(true)
        snapshotControlsInvalidatedRef.current = false
        setSnapshotControlsInvalidated(false)
        const currentStartupState = startupStateRef.current
        if (
          ttsWriteRevisionRef.current === 0 &&
          (currentStartupState.kind === 'ready' || currentStartupState.kind === 'warming_model')
        ) {
          ttsEnabledRef.current = currentStartupState.ttsEnabled
          ttsPersistedValueRef.current = currentStartupState.ttsEnabled
          ttsPersistedInitializedRef.current = true
          setTtsEnabled(currentStartupState.ttsEnabled)
        }
      }
    })

    return () => { active = false }
  }, [adoptSnapshotPreferences, recoverOrphanedRequest, synchronizeHydratedRuntimeStatus])

  useEffect(() => {
    if (snapshotHydrationState !== 'failed') return undefined
    let active = true

    void invokeTauriCommand('get_ui_text_size')
      .then((payload) => {
        if (!active || uiTextSizePersistedRevisionRef.current !== 0) {
          return
        }

        const textSize = parseUiTextSize(payload)
        uiTextSizePersistedValueRef.current = textSize
        uiTextSizePersistedInitializedRef.current = true
        if (uiTextSizePendingRevisionRef.current === null) setUiTextSize(textSize)
      })
      .catch(() => undefined)

    return () => {
      active = false
    }
  }, [snapshotHydrationState])

  useEffect(() => {
    if (snapshotHydrationState !== 'failed') return undefined
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
  }, [addNotice, snapshotHydrationState])

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
    if (snapshotHydrationState !== 'failed') return undefined
    let active = true

    void invokeTauriCommand('get_ui_theme')
      .then((payload) => {
        if (!active || uiThemePersistedRevisionRef.current !== 0) {
          return
        }

        const theme = parseUiTheme(payload)
        uiThemePersistedValueRef.current = theme
        uiThemePersistedInitializedRef.current = true
        if (uiThemePendingRevisionRef.current === null) setUiTheme(theme)
      })
      .catch(() => undefined)

    return () => {
      active = false
    }
  }, [snapshotHydrationState])

  const persistUiTextSize = async (nextTextSize: UiTextSize, previousTextSize: UiTextSize): Promise<void> => {
    const revision = ++uiTextSizeWriteRevisionRef.current
    uiTextSizePendingRevisionRef.current = revision
    setUiTextSize(nextTextSize)

    try {
      const write = uiTextSizeWriteChainRef.current.then(() =>
        invokeTauriCommand('set_ui_text_size', { textSize: nextTextSize }),
      )
      uiTextSizeWriteChainRef.current = write.then(() => undefined, () => undefined)
      const payload = await write
      const persistedTextSize = parseUiTextSize(payload)
      uiTextSizePersistedRevisionRef.current = revision
      uiTextSizePersistedValueRef.current = persistedTextSize
      uiTextSizePersistedInitializedRef.current = true
      if (revision === uiTextSizeWriteRevisionRef.current) {
        uiTextSizePendingRevisionRef.current = null
        setUiTextSize(persistedTextSize)
      }
    } catch (error) {
      if (revision !== uiTextSizeWriteRevisionRef.current) return
      uiTextSizePendingRevisionRef.current = null
      setUiTextSize(uiTextSizePersistedInitializedRef.current
        ? uiTextSizePersistedValueRef.current
        : initialSnapshotPreferencesRef.current?.uiTextSize ?? previousTextSize)
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
    uiThemePendingRevisionRef.current = revision
    setUiTheme(nextTheme)

    try {
      const write = uiThemeWriteChainRef.current.then(() =>
        invokeTauriCommand('set_ui_theme', { theme: nextTheme }),
      )
      uiThemeWriteChainRef.current = write.then(() => undefined, () => undefined)
      const payload = await write
      const persistedTheme = parseUiTheme(payload)
      uiThemePersistedRevisionRef.current = revision
      uiThemePersistedValueRef.current = persistedTheme
      uiThemePersistedInitializedRef.current = true
      if (revision === uiThemeWriteRevisionRef.current) {
        uiThemePendingRevisionRef.current = null
        setUiTheme(persistedTheme)
      }
    } catch (error) {
      if (revision !== uiThemeWriteRevisionRef.current) return
      uiThemePendingRevisionRef.current = null
      setUiTheme(uiThemePersistedInitializedRef.current
        ? uiThemePersistedValueRef.current
        : initialSnapshotPreferencesRef.current?.uiTheme ?? previousTheme)
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

  const applyStartupState = useCallback((
    nextState: StartupState,
    syncRuntimeStatus = true,
    startupSnapshotRevision = snapshotHydrationRevisionRef.current,
  ): StartupState => {
    const isInitialStartupHydration = startupStateRef.current.kind === 'loading'
    const preserveSnapshotControls =
      snapshotHydrationStateRef.current === 'succeeded' &&
      startupSnapshotRevision < snapshotHydrationRevisionRef.current &&
      snapshotCapabilitiesRef.current !== null &&
      snapshotResponseProfileRef.current !== null
    const publishedState = preserveSnapshotControls
      ? applySnapshotControlState(
        nextState,
        snapshotCapabilitiesRef.current ?? [],
        snapshotResponseProfileRef.current ?? 'fast',
      )
      : nextState
    if (
      snapshotHydrationStateRef.current === 'succeeded' &&
      !preserveSnapshotControls &&
      (publishedState.kind === 'ready' || publishedState.kind === 'warming_model')
    ) {
      snapshotCapabilitiesRef.current = publishedState.capabilities
      snapshotResponseProfileRef.current = publishedState.selectedResponseProfile
    }
    startupStateRef.current = publishedState
    setStartupState(publishedState)
    const startupTtsStateIsFresh =
      snapshotHydrationStateRef.current !== 'succeeded' ||
      startupSnapshotRevision === snapshotHydrationRevisionRef.current
    if (
      ttsWriteRevisionRef.current === 0 &&
      startupTtsStateIsFresh &&
      (nextState.kind === 'ready' || nextState.kind === 'warming_model')
    ) {
      ttsPersistedValueRef.current = nextState.ttsEnabled
      ttsPersistedInitializedRef.current = true
    }
    if (
      snapshotHydrationStateRef.current !== 'pending' &&
      (
        snapshotHydrationStateRef.current !== 'succeeded' ||
        startupSnapshotRevision === snapshotHydrationRevisionRef.current
      ) &&
      ttsWriteRevisionRef.current === 0
    ) {
      const nextTtsEnabled = nextState.kind === 'ready' || nextState.kind === 'warming_model'
        ? nextState.ttsEnabled
        : false
      ttsEnabledRef.current = nextTtsEnabled
      setTtsEnabled(nextTtsEnabled)
    }

    if (syncRuntimeStatus) {
      const nextRuntimeStatus = startupStateToRuntimeStatus(nextState)
      if (isInitialStartupHydration) {
        synchronizeHydratedRuntimeStatus(nextRuntimeStatus)
      } else {
        publishRuntimeStatus(nextRuntimeStatus, true)
      }
    }
    return publishedState
  }, [publishRuntimeStatus, synchronizeHydratedRuntimeStatus])

  useEffect(() => {
    let active = true

    void (async () => {
      while (active) {
        const startupSnapshotRevision = snapshotHydrationRevisionRef.current
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
        const syncRuntimeStatus = !hydrated && !snapshotRuntimeHydratedRef.current
        const publishedState = applyStartupState(nextState, syncRuntimeStatus, startupSnapshotRevision)

        if (publishedState.kind === 'ready') {
          const newlyUnavailableCapabilities = publishedState.capabilities.filter(({ id, state }) => {
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
  }, [addNotice, applyStartupState])

  useEffect(() => {
    appActiveRef.current = true
    return () => {
      appActiveRef.current = false
      const activePrompt = promptRef.current
      if (activePrompt !== null) activePrompt.terminal = true
      promptRef.current = null
      cancelTts()
      if (wakeConfidenceHoldRef.current !== null) {
        window.clearTimeout(wakeConfidenceHoldRef.current)
      }
      liveAudioSessionIdRef.current += 1
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
  const instantAssistantOptions = useMemo(
    () => applySnapshotModelOptions(instantOptions(assistantCapabilities), snapshotModelOptions),
    [assistantCapabilities, snapshotModelOptions],
  )
  const deepAssistantOptions = useMemo(
    () => applySnapshotModelOptions(deepOptions(assistantCapabilities), snapshotModelOptions),
    [assistantCapabilities, snapshotModelOptions],
  )
  const reviewAssistantOptions = useMemo(
    () => applySnapshotModelOptions(reviewOptions(assistantCapabilities), snapshotModelOptions),
    [assistantCapabilities, snapshotModelOptions],
  )
  const selectedInstantOption = instantAssistantOptions
    .find((option) => option.value === assistantSettings.instant)
  const selectedInstantRetryable = localInstantCanRetry(startupState, assistantSettings.instant)
  const selectedInstantProfileReady = instantMatchesResponseProfile(startupState, assistantSettings.instant)
  const canSend = useMemo(
    () =>
      startupState.kind === 'ready' &&
      selectedInstantOption?.available === true &&
      selectedInstantProfileReady &&
      !assistantSettingsPending &&
      snapshotControlsFresh &&
      !resetPending &&
      promptState === 'idle' &&
      runtimeStatus === 'sleeping' &&
      composerValue.trim().length > 0,
    [assistantSettingsPending, composerValue, promptState, resetPending, runtimeStatus, selectedInstantOption?.available, selectedInstantProfileReady, snapshotControlsFresh, startupState.kind],
  )

  const voiceSnapshotControlsUsable = snapshotControlsFresh || (
    !snapshotControlsInvalidated && snapshotHydrationState === 'pending'
  )
  const voiceSnapshotControlsUsableNow = (): boolean => snapshotControlsFreshRef.current || (
    !snapshotControlsInvalidatedRef.current && snapshotHydrationStateRef.current === 'pending'
  )
  const canToggleMic = micActive || (voiceSnapshotControlsUsable && voiceInputReady(startupState) && !micStarting && !resetPending)
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
  const assistantControlsDisabled = startupState.kind !== 'ready' || assistantSettingsPending || isSwitchingResponseProfile || !snapshotControlsFresh
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
    const option = instantAssistantOptions.find((item) => item.value === instant)
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

  const applyRuntimeStatus = (nextStatus: RuntimeStatus): RuntimeStatus => {
    const previousStatus = publishRuntimeStatus(nextStatus, true)

    if (nextStatus === previousStatus) {
      return previousStatus
    }

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

  const applyTransition = (
    previousStatus: RuntimeStatus,
    event: Parameters<typeof transitionRuntimeStatus>[1],
  ): RuntimeStatus => applyRuntimeStatus(transitionRuntimeStatus(previousStatus, event))

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
    applyRuntimeStatus('error')
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
    runtimeActivityRevisionRef.current += 1

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
      ? applySnapshotModelOptions(
        instantOptions(capabilityMap(currentStartupState)),
        snapshotModelOptionsRef.current,
      ).find((option) => option.value === currentSettings.instant)
      : undefined
    if (
      currentStartupState.kind !== 'ready' ||
      selectedInstant?.available !== true ||
      !instantMatchesResponseProfile(currentStartupState, currentSettings.instant) ||
      assistantSettingsPendingRef.current ||
      !snapshotControlsFreshRef.current ||
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
    const promptRuntimeActivityRevision = runtimeActivityRevisionRef.current
    const expectedCanonicalSessionId = snapshotSessionIdRef.current
    const priorCanonicalExecutions = new Set(
      canonicalTranscriptRef.current.map((turn) => executionKey(turn)),
    )
    clearPartialTranscript()
    cancelTts()
    setResponseAnnouncement('')

    // eslint-disable-next-line react-hooks/purity
    const requestId = `request-${Date.now()}-${Math.random().toString(36).slice(2)}`
    const projectionRevision = ++promptProjectionRevisionRef.current
    const userId = `user-${projectionRevision}-${requestId}`
    const assistantId = `assistant-${projectionRevision}-${requestId}`
    const promptProjection: PromptProjection = {
      id: requestId,
      userId,
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
    promptRef.current = promptProjection
    setPromptState('executing')
    setPromptActivity('Executing prompt…')
    setMessages((currentMessages) => [
      ...currentMessages,
      {
        id: userId,
        role: 'user',
        content: prompt,
      },
    ])

    if (source === 'typed') {
      setComposerValue('')
      clearCompletionDisplay()
    }

    const handleEvent = (event: import('./types/chat').PromptExecutionEvent): void => {
      if (!appActiveRef.current) return
      const active = promptRef.current
      if (active !== promptProjection || active.id !== event.requestId || active.terminal) {
        return
      }
      if (event.kind === 'text') {
        active.text += event.text
        if (
          snapshotHydrationStateRef.current !== 'succeeded' &&
          active.tts &&
          ttsEnabledRef.current &&
          !active.ttsFirstLineSpoken &&
          active.text.includes('\n')
        ) {
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
        if (
          snapshotHydrationStateRef.current !== 'succeeded' &&
          active.tts &&
          ttsEnabledRef.current
        ) {
          void synthesizeAndPlayAssistantReply(event.correction.replace(/^Correction:\s*/u, ''))
        }
      }
      if (event.kind === 'stage') {
        active.stages = active.stages.map((stage) => stage.stage === event.stage
          ? { stage: event.stage, status: event.status, ...(event.detail === undefined ? {} : { detail: event.detail }) }
          : stage)
        setMessages((current) => current.map((message) => message.id === active.assistantId
          ? { ...message, answerStage: { stages: active.stages, priorVersions: active.priorVersions, sources: active.sources } } : message))
        setPromptActivity(`${event.stage[0]?.toUpperCase()}${event.stage.slice(1)} stage ${event.status}`)
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
    let result: {
      readonly requestId: string
      readonly runtimePhase: BackendRuntimePhase | null
      readonly outcome: 'completed' | 'cancelled' | 'error'
      readonly errorMessage: string | null
    }
    let commandRejected = false
    try {
      result = await executePrompt(requestId, prompt, handleEvent, source)
    } catch (error) {
      const message = error instanceof Error
        ? error.message
        : typeof error === 'string' && error.trim().length > 0
          ? error
          : 'Prompt execution failed'
      commandRejected = true
      result = {
        requestId,
        runtimePhase: null,
        outcome: 'error',
        errorMessage: message,
      }
      recordRuntimeDiagnostic('execution', `Execution error: ${message}`)
    }

    {
      if (!appActiveRef.current) return
      const active = promptRef.current
      if (
        active === null ||
        active !== promptProjection ||
        active.id !== requestId ||
        result.requestId !== requestId ||
        active.terminal
      ) {
        return
      }
      if (result.outcome !== 'completed') cancelTts()
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
      if (result.outcome !== 'completed') {
        setResponseAnnouncement('')
        setMessages((current) => current.filter(
          (message) => message.id !== active.userId && message.id !== active.assistantId,
        ))
      }
      let reconciledTurn: Turn | undefined
      let reconciledRuntimePhase: BackendRuntimePhase | null = null
      let reconciledRuntimeActivityRevision: number | null = null
      if (snapshotHydrationStateRef.current === 'succeeded') {
        setPromptState('reconciling')
        setPromptActivity('Synchronizing response…')
        const transcriptRevision = transcriptRevisionRef.current
        let snapshot: Awaited<ReturnType<typeof loadAppSnapshot>> | null = null
        for (let attempt = 0; attempt < 3 && snapshot === null; attempt += 1) {
          if (!appActiveRef.current || promptRef.current !== active || resetPendingRef.current || transcriptRevision !== transcriptRevisionRef.current) return
          try {
            const runtimeActivityRevision = runtimeActivityRevisionRef.current
            const candidate = await loadAppSnapshot()
            if (candidate.activeRequest === null) {
              snapshot = candidate
              reconciledRuntimeActivityRevision = runtimeActivityRevision
            }
          } catch {
            // A bounded retry keeps terminal settlement tied to canonical state.
          }
          if (snapshot === null && attempt < 2) {
            await new Promise((resolve) => window.setTimeout(resolve, 250 * (attempt + 1)))
          }
        }
        if (!appActiveRef.current || promptRef.current !== active || resetPendingRef.current || transcriptRevision !== transcriptRevisionRef.current) return
        if (snapshot === null) {
          setPromptActivity('Session recovery required. Use Reset Session to retry.')
          return
        }
        setMessages((current) => replaceCommittedTranscript(current, snapshot.transcript))
        reconciledTurn = findNewTurnForRequest(
          snapshot.transcript,
          requestId,
          expectedCanonicalSessionId,
          priorCanonicalExecutions,
        )
        canonicalTranscriptRef.current = snapshot.transcript
        snapshotSessionIdRef.current = snapshot.sessionId
        reconciledRuntimePhase = snapshot.runtimePhase
      }
      promptRef.current = null
      setPromptState('idle')
      setPromptActivity(null)
      if (
        reconciledRuntimePhase === null &&
        result.runtimePhase !== null &&
        promptRuntimeActivityRevision === runtimeActivityRevisionRef.current
      ) {
        applyRuntimeStatus(toRuntimeStatus(result.runtimePhase))
      } else if (
        reconciledRuntimePhase !== null &&
        reconciledRuntimeActivityRevision === runtimeActivityRevisionRef.current
      ) {
        applyRuntimeStatus(toRuntimeStatus(reconciledRuntimePhase))
      }
      const canonicalSettlementRequired = snapshotHydrationStateRef.current === 'succeeded'
      const canonicalCanConfirmCompletion = result.outcome === 'completed' || commandRejected
      const effectiveOutcome = reconciledTurn !== undefined && canonicalCanConfirmCompletion
        ? 'completed'
        : canonicalSettlementRequired && result.outcome === 'completed'
          ? 'error'
          : result.outcome
      const terminalText = reconciledTurn?.answer.content.text ?? active.text
      if (effectiveOutcome === 'completed' && terminalText.trim().length > 0) {
        setResponseAnnouncement(formatResponseAnnouncement(
          reconciledTurn?.answer.content.kind ?? 'text',
          terminalText,
        ))
      }
      if (effectiveOutcome !== 'completed') {
        addNotice({
          tone: effectiveOutcome === 'error' ? 'error' : 'info',
          title: effectiveOutcome === 'error' ? 'Response failed' : 'Response cancelled',
          message:
            effectiveOutcome === 'error'
              ? result.outcome === 'completed'
                ? 'The completed response could not be synchronized. Try again.'
                : result.errorMessage ?? active.error ?? 'The response could not be completed. Try again.'
              : 'The response was cancelled.',
        })
      } else if (terminalText.trim().length === 0) {
        addNotice({
          tone: 'info',
          title: 'No response',
          message: 'No response was returned. Try again.',
        })
      } else if (
        appActiveRef.current &&
        active.tts &&
        ttsEnabledRef.current &&
        !(active.corrected && snapshotHydrationStateRef.current !== 'succeeded') &&
        !active.ttsFirstLineSpoken
      ) {
        void synthesizeAndPlayAssistantReply(terminalText)
      }
      if (
        commandRejected &&
        snapshotHydrationStateRef.current !== 'succeeded' &&
        promptRuntimeActivityRevision === runtimeActivityRevisionRef.current
      ) {
        applyTransition(executingStatus, 'fail')
      }
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
    let audioContext: AudioContext | null = null
    cancelTts()
    const generation = ttsGenerationRef.current

    try {
      const speechText = firstNonEmptyCompletedLine(text) ?? ''
      if (!isValidTtsFirstLine(speechText)) return
      const payload = await invokeTauriCommand('synthesize_local_tts', { text: speechText })
       if (generation !== ttsGenerationRef.current || !ttsEnabledRef.current) return

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
       if (generation !== ttsGenerationRef.current || !ttsEnabledRef.current) return
      ttsSourceRef.current = source
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
      if (generation !== ttsGenerationRef.current) return
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
      if (ttsSourceRef.current !== null && generation === ttsGenerationRef.current) ttsSourceRef.current = null
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
    if (!voiceSnapshotControlsUsableNow()) return
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

    if (liveAudioSessionId !== liveAudioSessionIdRef.current || !voiceSnapshotControlsUsableNow()) return

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

    if (liveAudioSessionId !== liveAudioSessionIdRef.current || !voiceSnapshotControlsUsableNow()) return
    clearPartialTranscript()
    maybeRunVoiceTranscript(runtimePhase)
  }

  const stopLiveAudio = (content: string | null = null): void => {
    liveAudioSessionIdRef.current += 1
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
    if (resetPendingRef.current || !voiceSnapshotControlsUsableNow() || !voiceInputReady(startupStateRef.current) || liveAudioSourceRef.current !== null || micStarting || micStartInFlightRef.current) {
      return
    }
    micStartInFlightRef.current = true

    const liveAudioSessionId = liveAudioSessionIdRef.current + 1
    liveAudioSessionIdRef.current = liveAudioSessionId

    setMicStarting(true)

    try {
      const liveAudioSource = await startLiveAudioSource({
        ...(audioInputDeviceIdRef.current === null
          ? {}
          : { deviceId: audioInputDeviceIdRef.current }),
        onFrame: async (frame) => {
          if (
            liveAudioSessionId !== liveAudioSessionIdRef.current ||
            !voiceSnapshotControlsUsableNow() ||
            !voiceInputReady(startupStateRef.current)
          ) {
            return
          }

          runtimeActivityRevisionRef.current += 1
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

            if (
              liveAudioSessionId !== liveAudioSessionIdRef.current ||
              !voiceSnapshotControlsUsableNow() ||
              !voiceInputReady(startupStateRef.current)
            ) {
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
            if (
              liveAudioSessionId !== liveAudioSessionIdRef.current ||
              !voiceSnapshotControlsUsableNow() ||
              !voiceInputReady(startupStateRef.current)
            ) {
              return
            }

            enterRuntimeError()
            throw error
          } finally {
            liveAudioInFlightFramesRef.current = Math.max(0, liveAudioInFlightFramesRef.current - 1)
          }
        },
        onError: reportLiveAudioError,
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

      if (
        !appActiveRef.current ||
        liveAudioSessionId !== liveAudioSessionIdRef.current ||
        !voiceSnapshotControlsUsableNow() ||
        (startupStateRef.current.kind === 'ready' && !voiceInputReady(startupStateRef.current))
      ) {
        liveAudioSource.stop()
        if (appActiveRef.current && voiceSnapshotControlsUsableNow() && voiceInputReady(startupStateRef.current)) {
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
        if (appActiveRef.current && voiceSnapshotControlsUsableNow() && voiceInputReady(startupStateRef.current)) {
          micAutoStartedRef.current = false
          queueMicrotask(() => void startMic())
        }
        return
      }

      setMicStarting(false)
      micStartInFlightRef.current = false
      reportLiveAudioError(error)
    } finally {
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

  const refreshSnapshotControlsAfterProfileTransition = async (profile: ResponseProfile): Promise<boolean> => {
    if (snapshotHydrationStateRef.current !== 'succeeded') return true
    const hydrationRevision = snapshotHydrationRevisionRef.current
    snapshotControlsFreshRef.current = false
    setSnapshotControlsFresh(false)
    snapshotControlsInvalidatedRef.current = true
    setSnapshotControlsInvalidated(true)
    try {
      const snapshot = await loadAppSnapshot()
      if (
        !appActiveRef.current ||
        !isSwitchingResponseProfileRef.current ||
        resetPendingRef.current ||
        hydrationRevision !== snapshotHydrationRevisionRef.current ||
        snapshot.preferences.responseProfile !== profile
      ) {
        return false
      }
      snapshotHydrationRevisionRef.current += 1
      const capabilities = toStartupCapabilities(snapshot.capabilities)
      snapshotCapabilitiesRef.current = capabilities
      snapshotResponseProfileRef.current = snapshot.preferences.responseProfile
      snapshotModelOptionsRef.current = snapshot.modelOptions
      setSnapshotModelOptions(snapshot.modelOptions)
      snapshotControlsFreshRef.current = true
      setSnapshotControlsFresh(true)
      snapshotControlsInvalidatedRef.current = false
      setSnapshotControlsInvalidated(false)
      const controlledState = applySnapshotControlState(
        startupStateRef.current,
        capabilities,
        snapshot.preferences.responseProfile,
      )
      startupStateRef.current = controlledState
      setStartupState(controlledState)
      return true
    } catch (error) {
      addNotice({
        tone: 'error',
        title: 'Profile state unavailable',
        message: toDisplayErrorMessage(error),
      })
      return false
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
    if (
      snapshotControlsFreshRef.current &&
      currentState.selectedResponseProfile === profile &&
      !localInstantCanRetry(currentState, profileInstant)
    ) {
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
    applyRuntimeStatus('initializing')

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
      if (!await refreshSnapshotControlsAfterProfileTransition(profile)) {
        micAutoStartedRef.current = false
        stopLiveAudio()
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
    ttsPendingRevisionRef.current = revision
    const previousEnabled = ttsEnabledRef.current
    ttsEnabledRef.current = enabled
    setTtsEnabled(enabled)
    if (!enabled) cancelTts()

    try {
      const write = ttsWriteChainRef.current.then(() =>
        invokeTauriCommand('set_tts_enabled', { enabled }),
      )
      ttsWriteChainRef.current = write.then(() => undefined, () => undefined)
      const payload = await write
      const persistedEnabled =
        typeof payload === 'object' &&
        payload !== null &&
        'enabled' in payload &&
        typeof payload.enabled === 'boolean'
          ? payload.enabled
          : enabled
      ttsPersistedRevisionRef.current = revision
      ttsPersistedValueRef.current = persistedEnabled
      ttsPersistedInitializedRef.current = true
      if (revision !== ttsWriteRevisionRef.current) return
      if (revision === ttsWriteRevisionRef.current) {
        ttsPendingRevisionRef.current = null
        ttsEnabledRef.current = ttsPersistedValueRef.current
        setTtsEnabled(ttsPersistedValueRef.current)
      }
    } catch (error) {
      if (revision !== ttsWriteRevisionRef.current) return
      const message = toDisplayErrorMessage(error)

      ttsPendingRevisionRef.current = null
      const rollbackEnabled = ttsPersistedInitializedRef.current
        ? ttsPersistedValueRef.current
        : initialSnapshotPreferencesRef.current?.ttsEnabled ?? previousEnabled
      ttsEnabledRef.current = rollbackEnabled
      setTtsEnabled(rollbackEnabled)
      addNotice({
        tone: 'error',
        title: 'TTS setting failed',
        message,
      })
      recordRuntimeDiagnostic('tts', `TTS toggle error: ${message}`)
    }
  }

  useEffect(() => {
    if (!voiceSnapshotControlsUsable) {
      micAutoStartedRef.current = false
      if (micActive || micStarting || micStartInFlightRef.current) stopLiveAudio()
      return
    }
    if (startupState.kind === 'ready' && !voiceInputReady(startupState)) {
      micAutoStartedRef.current = false
      if (micActive || micStarting || micStartInFlightRef.current) stopLiveAudio()
      return
    }
    if (!voiceInputReady(startupState)) return
    if (
      resetPending ||
      resetPendingRef.current ||
      micActive ||
      micStarting ||
      micAutoStartedRef.current
    ) {
      return
    }

    micAutoStartedRef.current = true
    void startMic()
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [micActive, micStarting, resetPending, startupState, voiceSnapshotControlsUsable])

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

  const selectedDeepOption = deepAssistantOptions.find((option) => option.value === assistantSettings.deep)
  const selectedReviewOption = reviewAssistantOptions.find((option) => option.value === assistantSettings.review)
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
    const transcriptRevision = ++transcriptRevisionRef.current
    setResetPending(true)
    if (liveAudioSourceRef.current !== null || micStarting || micStartInFlightRef.current) {
      micAutoStartedRef.current = false
    }
    stopLiveAudio()
    const resetRuntimeActivityRevision = runtimeActivityRevisionRef.current
    const activePrompt = promptRef.current
    cancelTts()
    clearPartialTranscript()
    clearCompletion()
    setResponseAnnouncement('')

    if (getTauriInternals() === null) {
      if (activePrompt !== null) activePrompt.terminal = true
      promptRef.current = null
      canonicalTranscriptRef.current = []
      setMessages((current) => replaceCommittedTranscript(current, []))
      setPromptActivity(null)
      setPromptState('idle')
      if (resetRuntimeActivityRevision === runtimeActivityRevisionRef.current) {
        applyRuntimeStatus('sleeping')
      }
      resetPendingRef.current = false
      setResetPending(false)
      return
    }

    snapshotControlsFreshRef.current = false
    setSnapshotControlsFresh(false)
    snapshotControlsInvalidatedRef.current = true
    setSnapshotControlsInvalidated(true)

    if (activePrompt !== null && !activePrompt.terminal) {
      activePrompt.terminal = true
      setPromptState('stopping')
      try {
        await invokeTauriCommand('cancel_prompt', { requestId: activePrompt.id })
      } catch {
        // reset_session remains authoritative for backend prompt settlement.
      }
      if (!appActiveRef.current) return
    }
    try {
      await waitForInFlightLiveAudioFrames()
      if (!appActiveRef.current) return
      await invokeTauriCommand('reset_session')
      if (!appActiveRef.current) return
      promptRef.current = null
      canonicalTranscriptRef.current = []
      setMessages((current) => replaceCommittedTranscript(current, []))
      setPromptActivity('Recovering session state…')
      setPromptState('reconciling')
      let resetSnapshot: AppSnapshot | null = null
      for (let attempt = 0; attempt < 3 && resetSnapshot === null; attempt += 1) {
        if (!appActiveRef.current || transcriptRevision !== transcriptRevisionRef.current) return
        try {
          resetSnapshot = await loadAppSnapshot()
        } catch {
          if (attempt < 2) await new Promise((resolve) => window.setTimeout(resolve, 250 * (attempt + 1)))
        }
      }
      if (resetSnapshot === null) {
        setPromptActivity('Session recovery required. Use Reset Session to retry.')
        setPromptState('reconciling')
        return
      }
      if (appActiveRef.current && transcriptRevision === transcriptRevisionRef.current) {
        snapshotHydrationRevisionRef.current += 1
        snapshotHydrationStateRef.current = 'succeeded'
        setSnapshotHydrationState('succeeded')
        snapshotControlsFreshRef.current = true
        setSnapshotControlsFresh(true)
        snapshotControlsInvalidatedRef.current = false
        setSnapshotControlsInvalidated(false)
        canonicalTranscriptRef.current = resetSnapshot.transcript
        snapshotSessionIdRef.current = resetSnapshot.sessionId
        snapshotModelOptionsRef.current = resetSnapshot.modelOptions
        setSnapshotModelOptions(resetSnapshot.modelOptions)
        const capabilities = toStartupCapabilities(resetSnapshot.capabilities)
        snapshotCapabilitiesRef.current = capabilities
        snapshotResponseProfileRef.current = resetSnapshot.preferences.responseProfile
        adoptSnapshotPreferences(resetSnapshot.preferences)
        const controlledState = applySnapshotControlState(
          startupStateRef.current,
          capabilities,
          resetSnapshot.preferences.responseProfile,
        )
        startupStateRef.current = controlledState
        setStartupState(controlledState)
        setMessages((current) => replaceCommittedTranscript(current, resetSnapshot.transcript))
      }
      if (!appActiveRef.current) return
      setPromptActivity(null)
      setPromptState('idle')
      if (resetRuntimeActivityRevision === runtimeActivityRevisionRef.current) {
        applyRuntimeStatus(toRuntimeStatus(resetSnapshot?.runtimePhase ?? 'sleeping'))
      }
    } catch (error) {
      addNotice({ tone: 'error', title: 'Reset failed', message: toDisplayErrorMessage(error) })
      setPromptActivity('Recovering session state…')
      let snapshot: AppSnapshot | null = null
      let recoveryRuntimeActivityRevision: number | null = null
      for (let attempt = 0; attempt < 3 && snapshot === null; attempt += 1) {
        if (!appActiveRef.current || !resetPendingRef.current || transcriptRevision !== transcriptRevisionRef.current) break
        try {
          const runtimeActivityRevision = runtimeActivityRevisionRef.current
          snapshot = await loadAppSnapshot()
          recoveryRuntimeActivityRevision = runtimeActivityRevision
        } catch {
          if (attempt < 2) await new Promise((resolve) => window.setTimeout(resolve, 250 * (attempt + 1)))
        }
      }
      if (snapshot !== null && appActiveRef.current && transcriptRevision === transcriptRevisionRef.current) {
        promptRef.current = null
        snapshotHydrationRevisionRef.current += 1
        snapshotHydrationStateRef.current = 'succeeded'
        setSnapshotHydrationState('succeeded')
        snapshotControlsFreshRef.current = true
        setSnapshotControlsFresh(true)
        snapshotControlsInvalidatedRef.current = false
        setSnapshotControlsInvalidated(false)
        canonicalTranscriptRef.current = snapshot.transcript
        snapshotSessionIdRef.current = snapshot.sessionId
        snapshotModelOptionsRef.current = snapshot.modelOptions
        setSnapshotModelOptions(snapshot.modelOptions)
        const capabilities = toStartupCapabilities(snapshot.capabilities)
        snapshotCapabilitiesRef.current = capabilities
        snapshotResponseProfileRef.current = snapshot.preferences.responseProfile
        adoptSnapshotPreferences(snapshot.preferences)
        const controlledState = applySnapshotControlState(
          startupStateRef.current,
          capabilities,
          snapshot.preferences.responseProfile,
        )
        startupStateRef.current = controlledState
        setStartupState(controlledState)
        const activeRequestMessage: ChatMessage[] = snapshot.activeRequest === null ? [] : [{
          id: `recovered-user-${snapshot.activeRequest.executionId.sessionId}-${snapshot.activeRequest.executionId.generation}`,
          role: 'user',
          content: snapshot.activeRequest.prompt,
        }]
        setMessages((current) => [
          ...replaceCommittedTranscript(current, snapshot.transcript),
          ...activeRequestMessage,
        ])
        if (snapshot.activeRequest === null) {
          if (recoveryRuntimeActivityRevision === runtimeActivityRevisionRef.current) {
            applyRuntimeStatus(toRuntimeStatus(snapshot.runtimePhase))
          }
          setPromptActivity(null)
          setPromptState('idle')
        } else {
          setPromptState('stopping')
          setPromptActivity('Recovering interrupted response…')
          await recoverOrphanedRequest(snapshot, transcriptRevision, () => resetPendingRef.current)
        }
      } else if (appActiveRef.current && transcriptRevision === transcriptRevisionRef.current) {
        setPromptActivity('Session recovery required. Use Reset Session to retry.')
        setPromptState('reconciling')
      }
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
    <div className="shell" data-ui-text-size={uiTextSize} data-ui-theme={uiTheme} data-runtime-status={runtimeStatus}>

      <main ref={conversationRef} className="conversation">
        {visibleMessages.map((message) => (
          message.answerStage && message.role === 'assistant' ? (
            <AnswerStage key={message.id} answer={message.content} className="message message--assistant" {...(message.contentKind === undefined ? {} : { contentKind: message.contentKind })} {...message.answerStage} />
          ) : <ChatBubble key={message.id} message={message} />
        ))}
      </main>

      <UserNoticeToast notice={notices[0] ?? null} />
      <div className="sr-only" role="status" aria-live="polite" aria-atomic="true" aria-label="Response announcements">
        {responseAnnouncement}
      </div>

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
                {instantAssistantOptions.map((option) => <option key={option.value} value={option.value} disabled={!option.available && !localInstantCanRetry(startupState, option.value)} title={option.reason}>{option.label}{option.available ? '' : ` — ${option.reason}`}</option>)}
              </select></label>
              {selectedInstantRetryable ? <button type="button" aria-label="Retry local profile" onClick={() => void changeInstant(assistantSettings.instant)}>Retry local profile</button> : null}
              <label><strong>Deep model</strong><select aria-label="Deep model" value={assistantSettings.deep} disabled={assistantControlsDisabled} onChange={(event) => {
                const value = event.target.value as AssistantSettings['deep']
                if (deepAssistantOptions.find((option) => option.value === value)?.available) {
                  void persistAssistantSettings({ ...assistantSettingsRef.current, deep: value })
                }
              }}>
                {deepAssistantOptions.map((option) => <option key={option.value} value={option.value} disabled={!option.available} title={option.reason}>{option.label}</option>)}
              </select></label>
              <label><strong>Review model</strong><select aria-label="Review model" value={assistantSettings.review} disabled={assistantControlsDisabled} onChange={(event) => {
                const value = event.target.value as AssistantSettings['review']
                if (reviewAssistantOptions.find((option) => option.value === value)?.available) {
                  void persistAssistantSettings({ ...assistantSettingsRef.current, review: value })
                }
              }}>
                {reviewAssistantOptions.map((option) => <option key={option.value} value={option.value} disabled={!option.available} title={option.reason}>{option.label}</option>)}
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
            {(promptState === 'executing' || promptState === 'stopping') &&
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

function formatResponseAnnouncement(kind: 'text' | 'refusal', text: string): string {
  const boundedText = text.trim().slice(0, 240)
  if (boundedText.length === 0) return ''
  return `${kind === 'refusal' ? 'Assistant refusal' : 'Assistant response'}: ${boundedText}`
}

function projectCommittedTranscript(transcript: readonly Turn[]): readonly ChatMessage[] {
  return transcript.flatMap((turn) => [
    { id: turn.user.id, role: 'user' as const, content: turn.user.content.text },
    {
      id: turn.answer.id,
      role: 'assistant' as const,
      content: turn.answer.content.text,
      contentKind: turn.answer.content.kind,
      answerStage: {
        stages: [{ stage: turn.finalStage, status: 'completed' as const }],
        priorVersions: [],
      },
    },
  ])
}

function replaceCommittedTranscript(
  current: readonly ChatMessage[],
  transcript: readonly Turn[],
): readonly ChatMessage[] {
  return [
    ...current.filter((message) => message.role === 'system'),
    ...projectCommittedTranscript(transcript),
  ]
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

const CAPABILITY_IDS = new Set<import('./types/chat').CapabilityId>([
  'custom_provider', 'opencode', 'local_fast', 'local_quality', 'qwen_prediction',
  'wake_word', 'vad', 'parakeet', 'tts', 'deep', 'review',
])

function toStartupCapabilities(capabilities: readonly Capability[]): readonly import('./types/chat').StartupCapability[] {
  return capabilities
    .filter((capability): capability is Capability & { readonly id: import('./types/chat').CapabilityId } =>
      CAPABILITY_IDS.has(capability.id as import('./types/chat').CapabilityId))
    .map((capability) => ({
      id: capability.id,
      state: capability.state,
      reason: capability.reason,
      actualProvider: capability.actualProvider,
    }))
}

function applySnapshotControlState(
  state: StartupState,
  capabilities: readonly import('./types/chat').StartupCapability[],
  responseProfile: ResponseProfile,
): StartupState {
  if (state.kind !== 'ready' && state.kind !== 'warming_model') return state
  return {
    ...state,
    capabilities,
    selectedResponseProfile: responseProfile,
  }
}

function applySnapshotModelOptions<T extends string>(
  fallback: readonly import('./lib/assistantSettings').AssistantOption<T>[],
  snapshotOptions: readonly ModelOption[] | null,
): import('./lib/assistantSettings').AssistantOption<T>[] {
  if (snapshotOptions === null) return [...fallback]
  return fallback.map((option) => {
    const snapshotOption = snapshotOptions.find((candidate) => candidate.id === option.value)
    if (snapshotOption === undefined) {
      return { ...option, available: false, reason: 'Unavailable in the current app snapshot' }
    }
    const available = option.available && snapshotOption.available
    if (available) {
      const availableOption = { ...option }
      delete availableOption.reason
      return { ...availableOption, label: snapshotOption.label, available: true }
    }
    return {
      ...option,
      label: snapshotOption.label,
      available: false,
      reason: snapshotOption.unavailableReason ?? option.reason ?? 'Unavailable',
    }
  })
}

function findNewTurnForRequest(
  transcript: readonly Turn[],
  requestId: string,
  expectedSessionId: string | null,
  priorExecutions: ReadonlySet<string>,
): Turn | undefined {
  if (expectedSessionId === null) return undefined
  for (let index = transcript.length - 1; index >= 0; index -= 1) {
    const turn = transcript[index]
    if (
      turn?.requestId === requestId &&
      turn.executionId.sessionId === expectedSessionId &&
      !priorExecutions.has(executionKey(turn))
    ) {
      return turn
    }
  }
  return undefined
}

function executionKey(turn: Turn): string {
  return `${turn.executionId.sessionId}:${turn.executionId.generation}`
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

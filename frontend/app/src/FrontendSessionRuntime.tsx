import { memo, useCallback, useEffect, useLayoutEffect } from 'react'
import { FrontendTiming, reportAppBenchmarkStage } from './lib/frontendTiming'
import { useSessionView } from './FrontendSession'
import type { FrontendSession } from './FrontendSession'
import { SessionAuthority, cancelPromptProjectionFrame, sessionAuthority, useSessionControls, useSessionIntents } from './SessionAuthority'
import type { PromptProjection } from './SessionAuthority'
import { applySnapshotModelOptions, capabilityIsAvailable, capabilityMap, generationIneligibility, instantMatchesResponseProfile } from './sessionPolicy'
import type { ChangeEvent, FormEvent, KeyboardEvent, ReactNode } from 'react'
import { SETTINGS_DIALOG_ID, useSettingsNavigation } from './lib/useSettingsNavigation'
import { persistAudioInputDeviceId } from './lib/audioInputPreference'
import { useAndroidVoiceAdapter } from './lib/useAndroidVoiceAdapter'
import { ResponseAnnouncements, SessionTranscript } from './SessionTranscript'
import { PromptComposer } from './components/PromptComposer'
import { AndroidVoiceControlsProvider, AndroidComposerVoiceControl, AndroidVoiceSecondaryControls } from './components/AndroidVoiceControls'
import { SpeechSettings } from './components/SpeechSettings'
import type { AndroidVoiceContext, AndroidVoiceStatus } from './lib/androidVoice'
import { NewVoiceShortcutSettings } from './components/NewVoiceShortcutSettings'
import { isConfirmedNewVoiceContext, NEW_VOICE_SHORTCUT_EVENT } from './lib/newVoiceShortcut'
import { canonicalMessageId, replaceCommittedTranscript, settleCanonicalAnswerStages } from './committedTranscript'
import { InstantModelSelect } from './components/InstantModelSelect'
import { ControlIcon } from './components/ControlIcon'
import { PhoneActionOverflow } from './components/PhoneActionOverflow'
import headerIcon from '../../../apps/desktop-tauri/src-tauri/icons/icon.svg?url'
import { UserNoticeToast } from './components/UserNoticeToast'
import { UpdateSettings } from './components/UpdateSettings'
import { ChatGptAuth } from './components/ChatGptAuth'
import { createNativeAudioLifecycle, playCueWithFallback, prepareVoiceCues } from './lib/audioCues'
import { shouldSubmitComposer } from './lib/composer'
import { listAudioInputDevices, startLiveAudioSource } from './lib/liveAudioSource'
import { executePrompt } from './lib/promptExecution'
import { mergeAnswerSources } from './lib/answerSources'
import { appendCanonicalTurn, canonicalExecutionKey, isExpectedCanonicalTurn, loadAppSnapshot, parseAppSnapshot, parseAndroidConversationPage, parseAndroidHeadPage } from './lib/appSnapshot'
import type { AppSnapshot, Capability, Turn, ValidatedPreferences } from './lib/appSnapshot'
import {
  ingestAudioFrame,
  invokeRuntimeControl,
} from './lib/runtimeControl'
import type { RuntimeControlArgs } from './lib/runtimeControl'
import {
  DEFAULT_CUE_ASSET_PATHS,
  DEFAULT_INITIAL_SILENCE_TIMEOUT_MS,
  DEFAULT_SILENCE_TIMEOUT_MS,
  isStartupStateSettled,
  loadStartupState,
  parseStartupState,
} from './lib/startupState'
import { buildAndroidStartupState, cacheAndroidSnapshot, getAndroidPreferencePublicationRevision, getTauriInternals, invokeTauriCommand, isAndroidRuntime, isBrowserPreviewRuntime, isNativeTauriRuntime } from './lib/tauri'
import { teardownBrowserPreviewRuntime } from './lib/browserRuntime'
import { useAppUpdates } from './lib/useAppUpdates'
import type { UpdateState } from './lib/useAppUpdates'
import { instantOptions, parseAssistantSettings, serializeAssistantSettings } from './lib/assistantSettings'
import type { AssistantSettings } from './lib/assistantSettings'
import { acceptsPartialTranscriptionEvent, parsePartialTranscriptionEvent } from './lib/partialTranscription'
import { parseCompletionEvent } from './lib/completionEvents'
import { createVoiceTelemetryRecorder } from './lib/voiceTelemetry'
import { WakeDiagnostics } from './components/WakeDiagnostics'
import { WakeClipSettings } from './components/WakeClipSettings'
import { CaptureHistory } from './components/CaptureHistory'
import { createWakeClipBuffer } from './lib/wakeClipBuffer'
import {
  createVoiceActivityState,
  syncVoiceActivityState,
  updateVoiceActivityState,
} from './lib/voiceActivity'
import { cueForTransition, transitionRuntimeStatus } from './state/runtimeMachine'
import { firstNonEmptyStreamingLine, isValidTtsFirstLine } from './lib/ttsValidation'
import type {
  BackendRuntimePhase,
  ChatMessage,
  CueAssetPaths,
  PromptExecutionResult,
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
const ASSISTANT_SETTINGS_WRITE_FAILURE_TITLE = 'Assistant settings failed'
const UI_TEXT_SIZE_STEPS: readonly UiTextSize[] = ['small', 'medium', 'large', 'extra_large']
const DEFAULT_UI_TEXT_SIZE: UiTextSize = 'medium'
const DEFAULT_UI_THEME: UiTheme = 'dark'

const voiceInputReady = (state: StartupState): boolean =>
  state.kind === 'ready' &&
  state.voiceInputAvailable &&
  hasAvailableCapabilities(state, ['wake_word', 'vad', 'parakeet'])

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

function appUpdateBlocksRuntime(state: UpdateState): boolean {
  return state.kind === 'snapshot' && (
    state.snapshot.phase === 'installing' ||
    state.snapshot.phase === 'restart_required'
  )
}

function useFrontendSessionRuntime(session: SessionAuthority) {
  useSessionControls(session)
  const timingRef = session.operationRef('frontendTiming', () => new FrontendTiming())
  useLayoutEffect(() => { timingRef.current.startup('SHELL_DOM_COMMITTED') }, [timingRef])
  const supportsDesktopRuntime = !isAndroidRuntime && !isBrowserPreviewRuntime()
  const appUpdates = useAppUpdates()
  const updateRuntimeBlocked = appUpdateBlocksRuntime(appUpdates.state)
  const [startupState, setStartupState] = session.fieldState('startupState')
  const [runtimeStatus, setRuntimeStatus] = session.fieldState('runtimeStatus')
  const [composerValue, setComposerValue] = session.fieldState('composerValue')
  const composerValueRef = session.fieldRef('composerValue')
  const composerEditRevisionRef = session.selection.composerRevision
  const androidDraftWriteChainRef = session.selection.draftWrites
  const androidDraftPendingRef = session.selection.pendingDraft
  const androidDraftWriteFailureRef = session.selection.draftFailure
  const persistAndroidDraftRef = session.operationRef<(text: string) => void>('persistDraft', () => () => undefined)
  const shortcutDraftRef = session.selection.shortcutDraft
  const [, setShortcutStaging] = session.fieldState('shortcutStaging')
  const androidSelectionRecoveryRef = session.selection.recovery
  const [androidAssistant, setAndroidAssistant] = session.fieldState('androidAssistant')
  const [, setAndroidConversationId] = session.fieldState('androidConversationId')
  const [conversationPagePending, setConversationPagePending] = session.fieldState('conversationPagePending')
  const [headPagePending, setHeadPagePending] = session.fieldState('headPagePending')
  const [androidVoiceActive, setAndroidVoiceActive] = session.fieldState('androidVoiceActive')
  const androidVoiceActiveRef = session.fieldRef('androidVoiceActive')
  const [androidVoiceStatus, setAndroidVoiceStatus] = session.fieldState('androidVoiceStatus')
  const androidVoiceStatusRef = session.fieldRef('androidVoiceStatus')
  const [, setTtsEnabled] = session.fieldState('ttsEnabled')
  const ttsEnabledRef = session.fieldRef('ttsEnabled')
  const ttsGenerationRef = session.audio.speechRevision
  const ttsPendingRevisionRef = session.preferences.tts.pending
  const [, setAndroidSpeechStatusError] = session.fieldState('androidSpeechStatusError')
  const onAndroidSpeechStatus = useCallback((status: AndroidVoiceStatus | null, statusError: string | null): void => {
    setAndroidVoiceStatus(status)
    setAndroidSpeechStatusError(statusError)
    if (status !== null && ttsPendingRevisionRef.current === null) {
      ttsEnabledRef.current = status.ttsEnabled
      setTtsEnabled(status.ttsEnabled)
    }
  }, [setAndroidSpeechStatusError, setAndroidVoiceStatus, setTtsEnabled, ttsEnabledRef, ttsPendingRevisionRef])
  const androidVoiceStopRef = session.operationRef<(() => void) | null>('voiceStop', () => null)
  const androidVoiceInputEpochRef = session.operationRef('voiceInputRevision', () => 0)
  const pendingAndroidVoiceRequestRef = session.operationRef<{ readonly requestId: string; readonly text: string; readonly sessionId: string | null; readonly selectedHead: string | null; readonly composerRevision: number; readonly inputEpoch: number } | null>('voicePending', () => null)
  const drainAndroidVoiceRequestRef = session.operationRef<() => void>('drainVoice', () => () => undefined)
  const onAndroidVoiceActivity = useCallback((active: boolean): void => {
    setAndroidVoiceActive(active)
  }, [setAndroidVoiceActive])
  const onAndroidVoiceInputCancelled = useCallback((): void => {
    androidVoiceInputEpochRef.current += 1
    pendingAndroidVoiceRequestRef.current = null
  }, [androidVoiceInputEpochRef, pendingAndroidVoiceRequestRef])
  const androidAssistantRef = session.fieldRef('androidAssistant')
  const [, setPartialTranscript] = session.fieldState('partialTranscript')
  const [, setTypedCompletionSuffix] = session.fieldState('typedCompletionSuffix')
  const [, setVoiceCompletionSuffix] = session.fieldState('voiceCompletionSuffix')
  const [, setAutoStopOnSilence] = session.fieldState('autoStopOnSilence')
  const [preparedCueAssetPaths, setPreparedCueAssetPaths] = session.fieldState('preparedCueAssetPaths')
  const [, setTtsPlaying] = session.fieldState('ttsPlaying')
  const [, setPendingTtsCommands] = session.fieldState('pendingTtsCommands')
  const [, setWakeConfidence] = session.fieldState('wakeConfidence')
  const [isSwitchingResponseProfile, setIsSwitchingResponseProfile] = session.fieldState('isSwitchingResponseProfile')
  const [micStarting, setMicStarting] = session.fieldState('micStarting')
  const [micActive, setMicActive] = session.fieldState('micActive')
  const [, setAudioInputDevices] = session.fieldState('audioInputDevices')
  const [, setAudioInputDeviceId] = session.fieldState('audioInputDeviceId')
  const { settingsOpen, settingsButtonRef, settingsPanelRef, settingsCloseButtonRef, openSettings, closeSettings, onSettingsKeyDown } = useSettingsNavigation()
  const [voiceNotesOpen, setVoiceNotesOpen] = session.fieldState('voiceNotesOpen')
  const [assistantSettingsPending, setAssistantSettingsPending] = session.fieldState('assistantSettingsPending')
  const [snapshotHydrationState, setSnapshotHydrationState] = session.fieldState('snapshotHydrationState')
  const [snapshotControlsFresh, setSnapshotControlsFresh] = session.fieldState('snapshotControlsFresh')
  const [snapshotControlsInvalidated, setSnapshotControlsInvalidated] = session.fieldState('snapshotControlsInvalidated')
  const snapshotHydrationStateRef = session.fieldRef('snapshotHydrationState')
  const snapshotControlsFreshRef = session.fieldRef('snapshotControlsFresh')
  const snapshotControlsInvalidatedRef = session.fieldRef('snapshotControlsInvalidated')
  const snapshotHydrationRevisionRef = session.operationRef('hydrationRevision', () => 0)
  const snapshotControlRequestRevisionRef = session.operationRef('controlsRevision', () => 0)
  const refreshAndroidSnapshotControlsRef = session.operationRef<() => void>('refreshControls', () => () => undefined)
  const observeDesktopStartupRef = session.operationRef<(state: StartupState) => void>('observeDesktopStartup', () => () => undefined)
  const [, setAssistantSettingsLoadError] = session.fieldState('assistantSettingsLoadError')
  const [, setAssistantSettingsWriteError] = session.fieldState('assistantSettingsWriteError')
  const [, setUiThemeWriteError] = session.fieldState('uiThemeWriteError')
  const [customRetryPending, setCustomRetryPending] = session.fieldState('customRetryPending')
  const [assistantSettings, setAssistantSettings] = session.fieldState('assistantSettings')
  const [, setSnapshotModelOptions] = session.fieldState('snapshotModelOptions')
  const snapshotModelOptionsRef = session.fieldRef('snapshotModelOptions')
  const assistantSettingsRef = session.fieldRef('assistantSettings')
  const assistantSettingsPendingRef = session.fieldRef('assistantSettingsPending')
  const persistedAssistantSettingsRef = session.preferences.assistant.persisted
  const assistantSettingsWriteRevisionRef = session.preferences.assistant.revision
  const instantSelectionRevisionRef = session.preferences.instantRevision
  const [uiTextSize, setUiTextSize] = session.fieldState('uiTextSize')
  const [uiTheme, setUiTheme] = session.fieldState('uiTheme')
  const [notices, setNotices] = session.fieldState('notices')
  const [noticeInteraction, setNoticeInteraction] = session.fieldState('noticeInteraction')
  const [, setMessages] = session.fieldState('messages')
  const [, setResponseAnnouncement] = session.fieldState('responseAnnouncement')
  const [, setPromptActivity] = session.fieldState('promptActivity')
  const [promptState, setPromptState] = session.fieldState('promptState')
  const [resetPending, setResetPending] = session.fieldState('resetPending')
  const promptRef = session.prompt.active
  const promptProjectionRevisionRef = session.prompt.revision
  const liveAudioSourceRef = session.audio.liveSource
  const liveAudioStartAbortRef = session.audio.startAbort
  const audioInputDeviceIdRef = session.fieldRef('audioInputDeviceId')
  const liveAudioSessionIdRef = session.audio.inputRevision
  const liveAudioInFlightFramesRef = session.audio.inFlightFrames
  const markSilenceInFlightRef = session.audio.silenceInFlight
  const initialSnapshotPreferencesRef = session.operationRef<ValidatedPreferences | null>('initialPreferences', () => null)
  const snapshotCapabilitiesRef = session.operationRef<readonly import('./types/chat').StartupCapability[] | null>('acceptedCapabilities', () => null)
  const snapshotResponseProfileRef = session.operationRef<ResponseProfile | null>('acceptedResponseProfile', () => null)
  const snapshotSessionIdRef = session.selection.canonicalSession
  const uiTextSizePersistedRevisionRef = session.preferences.textSize.persistedRevision
  const uiThemePersistedRevisionRef = session.preferences.theme.persistedRevision
  const uiTextSizePersistedValueRef = session.preferences.textSize.persisted
  const uiThemePersistedValueRef = session.preferences.theme.persisted
  const uiTextSizePersistedInitializedRef = session.preferences.textSize.initialized
  const uiThemePersistedInitializedRef = session.preferences.theme.initialized
  const uiTextSizePendingRevisionRef = session.preferences.textSize.pending
  const uiThemePendingRevisionRef = session.preferences.theme.pending
  const isSwitchingResponseProfileRef = session.fieldRef('isSwitchingResponseProfile')
  const appActiveRef = session.activeRef
  const autoStopOnSilenceRef = session.fieldRef('autoStopOnSilence')
  const micAutoStartedRef = session.operationRef('micAutoStarted', () => false)
  const micStartInFlightRef = session.operationRef('micStartInFlight', () => false)
  const runtimeStatusRef = session.fieldRef('runtimeStatus')
  const runtimeActivityRevisionRef = session.operationRef('runtimeRevision', () => 0)
  const snapshotRuntimeHydratedRef = session.operationRef('runtimeHydrated', () => false)
  const startupStateRef = session.fieldRef('startupState')
  const voiceActivityStateRef = session.operationRef('voiceActivity', createVoiceActivityState)
  const voiceTelemetryRef = session.operationRef<ReturnType<typeof createVoiceTelemetryRecorder> | null>('voiceTelemetry', () => null)
  const partialTranscriptionRef = session.operationRef<{
    sessionId: number
    revision: number
    active: boolean
  } | null>('partialTranscription', () => null)
  const partialTranscriptionAllowedRef = session.operationRef('partialAllowed', () => false)
  const expectedPartialSessionIdRef = session.operationRef('partialSessionRevision', () => 0)
  const completionRevisionRef = session.operationRef('completionRevision', () => 0)
  const latestTypedCompletionRef = session.operationRef<{ revision: number; lifecycle: number } | null>('typedCompletion', () => null)
  const completionLifecycleRef = session.operationRef('completionLifecycle', () => 0)
  const resetPendingRef = session.fieldRef('resetPending')
  const transcriptRevisionRef = session.selection.contextRevision
  const canonicalTranscriptRef = session.selection.transcript
  const restoredDraftAnnouncementRef = session.operationRef<string | null>('restoredDraftAnnouncement', () => null)
  const adoptAndroidConversation = useCallback((snapshot: AppSnapshot, composerRevision: number, replaceHistory: boolean): void => {
    if (!isAndroidRuntime || snapshot.assistant === undefined) return
    if (shortcutDraftRef.current?.needsNew === true) return
    const prior = androidAssistantRef.current
    const assistant = prior?.revision === snapshot.assistant.revision && prior.conversationOffset > snapshot.assistant.conversationOffset
      ? { ...snapshot.assistant, conversationOffset: prior.conversationOffset, conversations: Array.from(new Map([...prior.conversations, ...snapshot.assistant.conversations].map((item) => [item.id, item])).values()) }
      : snapshot.assistant
    const withHeads = prior?.revision === assistant.revision && snapshot.sessionId === snapshotSessionIdRef.current && prior.selectedHead === assistant.selectedHead && prior.branchCount === assistant.branchCount && prior.branchOffset > assistant.branchOffset
      ? { ...assistant, branchOffset: prior.branchOffset, branchChoices: Array.from(new Set([...prior.branchChoices, ...assistant.branchChoices])) }
      : assistant
    setAndroidConversationId(snapshot.sessionId)
    setAndroidAssistant(withHeads)
    if (composerRevision === composerEditRevisionRef.current) {
      setComposerValue(snapshot.assistant.draft)
      const restoration = `${snapshot.sessionId}:${snapshot.assistant.selectedHead}`
      if (snapshot.assistant.interrupted && snapshot.assistant.draft !== '' && restoredDraftAnnouncementRef.current !== restoration) {
        restoredDraftAnnouncementRef.current = restoration
        setResponseAnnouncement('Draft restored in Prompt')
      } else if (!snapshot.assistant.interrupted) restoredDraftAnnouncementRef.current = null
    }
    if (replaceHistory) {
      if (snapshot.sessionId !== snapshotSessionIdRef.current || prior?.selectedHead !== assistant.selectedHead) {
        timingRef.current.retireRequest()
      }
      canonicalTranscriptRef.current = snapshot.transcript
      snapshotSessionIdRef.current = snapshot.sessionId
      setMessages((current) => replaceCommittedTranscript(current, snapshot.transcript))
    }
  }, [androidAssistantRef, canonicalTranscriptRef, composerEditRevisionRef, restoredDraftAnnouncementRef, setAndroidAssistant, setAndroidConversationId, setComposerValue, setMessages, setResponseAnnouncement, shortcutDraftRef, snapshotSessionIdRef, timingRef])
  const cuePlayerRef = session.audio.cues
  const nativeAudioLifecycleRef = session.operationRef('nativeAudioLifecycle', createNativeAudioLifecycle)
  useEffect(() => {
    if (!supportsDesktopRuntime) return
    cuePlayerRef.current.stop()
    const player = nativeAudioLifecycleRef.current.createCuePlayer()
    cuePlayerRef.current = player
    const setActive = (active: boolean): void => nativeAudioLifecycleRef.current.setActive(active)
    const visibilityChanged = (): void => {
      setActive(!document.hidden)
    }
    visibilityChanged()
    document.addEventListener('visibilitychange', visibilityChanged)
    return () => {
      document.removeEventListener('visibilitychange', visibilityChanged)
      setActive(false)
      player.retire()
    }
  }, [cuePlayerRef, nativeAudioLifecycleRef, supportsDesktopRuntime])
  const cueAssetPathsRef = session.operationRef<CueAssetPaths>('cuePaths', () => DEFAULT_CUE_ASSET_PATHS)
  const fallbackCueAssetPathsRef = session.operationRef<CueAssetPaths>('fallbackCuePaths', () => DEFAULT_CUE_ASSET_PATHS)
  const ttsPersistedRevisionRef = session.preferences.tts.persistedRevision
  const ttsPersistedValueRef = session.preferences.tts.persisted
  const ttsPersistedInitializedRef = session.preferences.tts.initialized
  const uiTextSizeWriteRevisionRef = session.preferences.textSize.revision
  const uiThemeWriteRevisionRef = session.preferences.theme.revision
  const ttsWriteRevisionRef = session.preferences.tts.revision
  const wakeConfidenceRef = session.fieldRef('wakeConfidence')
  const pendingWakeConfidenceRef = session.operationRef<number | null>('wakeConfidencePending', () => null)
  const wakeConfidenceHoldRef = session.operationRef<number | null>('wakeConfidenceTimer', () => null)

  const publishRuntimeStatus = useCallback((nextStatus: RuntimeStatus, claimActivity: boolean): RuntimeStatus => {
    if (claimActivity) runtimeActivityRevisionRef.current += 1
    const previousStatus = runtimeStatusRef.current
    setRuntimeStatus(nextStatus)
    return previousStatus
  }, [runtimeActivityRevisionRef, runtimeStatusRef, setRuntimeStatus])

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
      setPromptActivity({ text: 'Session recovery required. Use New Chat to retry.', compact: false })
      setPromptState('reconciling')
      return
    }
    canonicalTranscriptRef.current = latest.transcript
    snapshotSessionIdRef.current = latest.sessionId
    if (androidDraftPendingRef.current !== null) persistAndroidDraftRef.current(composerValueRef.current)
    setMessages((current) => replaceCommittedTranscript(current, latest.transcript))
    snapshotRuntimeHydratedRef.current = true
    if (latestRuntimeActivityRevision === runtimeActivityRevisionRef.current) {
      publishRuntimeStatus(toRuntimeStatus(latest.runtimePhase), false)
    }
    setPromptActivity(null)
    setPromptState('idle')
  }, [androidDraftPendingRef, appActiveRef, canonicalTranscriptRef, composerValueRef, persistAndroidDraftRef, publishRuntimeStatus, runtimeActivityRevisionRef, setMessages, setPromptActivity, setPromptState, snapshotRuntimeHydratedRef, snapshotSessionIdRef, transcriptRevisionRef])

  const cancelTts = useCallback((): void => {
    session.audio.cancelSpeech((playbackId) => invokeTauriCommand('finish_tts_playback', { playbackId }), () => setTtsPlaying(false))
  }, [session, setTtsPlaying])

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
      if (!active) return
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
  }, [expectedPartialSessionIdRef, partialTranscriptionAllowedRef, partialTranscriptionRef, setPartialTranscript, setVoiceCompletionSuffix])

  useEffect(() => {
    const tauri = getTauriInternals()
    if (tauri?.listen === undefined) return undefined
    let active = true
    let unlisten: (() => void) | undefined
    void tauri.listen('completion-event', (event) => {
      if (!active) return
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
  }, [completionLifecycleRef, latestTypedCompletionRef, partialTranscriptionRef, setTypedCompletionSuffix, setVoiceCompletionSuffix])

  const clearPartialTranscript = useCallback((): void => {
    partialTranscriptionAllowedRef.current = false
    if (partialTranscriptionRef.current !== null) {
      partialTranscriptionRef.current = { ...partialTranscriptionRef.current, active: false }
    }
    setPartialTranscript('')
    setVoiceCompletionSuffix('')
  }, [partialTranscriptionAllowedRef, partialTranscriptionRef, setPartialTranscript, setVoiceCompletionSuffix])

  const cancelPrompt = useCallback((): void => {
    if (isAndroidRuntime && androidVoiceActiveRef.current) androidVoiceStopRef.current?.()
    const cancellation = session.stopPromptPresentation()
    if (cancellation === null) return
    androidVoiceInputEpochRef.current += 1
    pendingAndroidVoiceRequestRef.current = null
    clearPartialTranscript()
    cancelTts()
    void invokeTauriCommand('cancel_prompt', { requestId: cancellation.requestId }).catch(cancellation.recover)
  }, [androidVoiceActiveRef, androidVoiceInputEpochRef, androidVoiceStopRef, cancelTts, clearPartialTranscript, pendingAndroidVoiceRequestRef, session])

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
  }, [partialTranscriptionAllowedRef, partialTranscriptionRef, publishRuntimeStatus, setPartialTranscript, setVoiceCompletionSuffix])

  if (voiceTelemetryRef.current === null) {
    voiceTelemetryRef.current = createVoiceTelemetryRecorder()
  }

  const voiceTelemetry = voiceTelemetryRef.current


  useEffect(() => {
    if (!isAndroidRuntime) return undefined
    // Navigation is permitted without granting microphone or automatic-submission consent.
    const openVoiceEntry = (): void => { setVoiceNotesOpen(false); closeSettings() }
    window.addEventListener(NEW_VOICE_SHORTCUT_EVENT, openVoiceEntry)
    return () => window.removeEventListener(NEW_VOICE_SHORTCUT_EVENT, openVoiceEntry)
  }, [closeSettings, setVoiceNotesOpen])

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
  }, [recordRuntimeDiagnostic, setNotices])

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
        webEnabled: preferences.webEnabled,
        speechNetworkFallback: preferences.speechNetworkFallback,
        speechLanguage: preferences.speechLanguage,
        speechTtsProvider: preferences.speechTtsProvider,
        speechTtsVoice: preferences.speechTtsVoice,
      }
      assistantSettingsRef.current = settings
      persistedAssistantSettingsRef.current = settings
      setAssistantSettings(settings)
      assistantSettingsPendingRef.current = false
      setAssistantSettingsPending(false)
    }
  }, [assistantSettingsPendingRef, assistantSettingsRef, assistantSettingsWriteRevisionRef, initialSnapshotPreferencesRef, persistedAssistantSettingsRef, setAssistantSettings, setAssistantSettingsLoadError, setAssistantSettingsPending, setTtsEnabled, setUiTextSize, setUiTheme, ttsEnabledRef, ttsPendingRevisionRef, ttsPersistedInitializedRef, ttsPersistedRevisionRef, ttsPersistedValueRef, uiTextSizePendingRevisionRef, uiTextSizePersistedInitializedRef, uiTextSizePersistedRevisionRef, uiTextSizePersistedValueRef, uiThemePendingRevisionRef, uiThemePersistedInitializedRef, uiThemePersistedRevisionRef, uiThemePersistedValueRef])

  const adoptSnapshotControls = useCallback((snapshot: AppSnapshot, revision: number, preferenceRevision: number): boolean => {
    if (revision !== snapshotControlRequestRevisionRef.current) return false
    const acceptedSnapshot = isAndroidRuntime
      ? cacheAndroidSnapshot(snapshot, preferenceRevision)
      : snapshot
    snapshotHydrationRevisionRef.current += 1
    adoptSnapshotPreferences(acceptedSnapshot.preferences)
    const capabilities = toStartupCapabilities(acceptedSnapshot.capabilities)
    snapshotCapabilitiesRef.current = capabilities
    snapshotResponseProfileRef.current = acceptedSnapshot.preferences.responseProfile
    snapshotModelOptionsRef.current = acceptedSnapshot.modelOptions
    setSnapshotModelOptions(acceptedSnapshot.modelOptions)
    const controlledState = isAndroidRuntime && startupStateRef.current.kind === 'loading'
      ? parseStartupState(buildAndroidStartupState(acceptedSnapshot))
      : applySnapshotControlState(
          startupStateRef.current,
          capabilities,
          acceptedSnapshot.preferences.responseProfile,
        )
    startupStateRef.current = controlledState
    setStartupState(controlledState)
    snapshotControlsFreshRef.current = true
    setSnapshotControlsFresh(true)
    snapshotControlsInvalidatedRef.current = false
    setSnapshotControlsInvalidated(false)
    if (isAndroidRuntime) queueMicrotask(() => drainAndroidVoiceRequestRef.current())
    return true
  }, [adoptSnapshotPreferences, drainAndroidVoiceRequestRef, setSnapshotControlsFresh, setSnapshotControlsInvalidated, setSnapshotModelOptions, setStartupState, snapshotCapabilitiesRef, snapshotControlRequestRevisionRef, snapshotControlsFreshRef, snapshotControlsInvalidatedRef, snapshotHydrationRevisionRef, snapshotModelOptionsRef, snapshotResponseProfileRef, startupStateRef])

  const adoptRecoveredAndroidSelection = useCallback((snapshot: AppSnapshot, recovery: NonNullable<typeof androidSelectionRecoveryRef.current>, controlRevision: number, preferenceRevision: number): boolean => {
    if (!appActiveRef.current || androidSelectionRecoveryRef.current !== recovery || transcriptRevisionRef.current !== recovery.transcriptRevision || snapshot.activeRequest !== null || snapshot.assistant === undefined) return false
    const shortcut = shortcutDraftRef.current
    if (shortcut?.needsNew === true) {
      if (!isConfirmedNewVoiceContext(snapshot, shortcut.oldSession)) { shortcut.prepare = null; return false }
    }
    if (!adoptSnapshotControls(snapshot, controlRevision, preferenceRevision)) return false
    if (shortcut?.needsNew === true) { shortcut.needsNew = false; setShortcutStaging(false) }
    adoptAndroidConversation(snapshot, shortcut === null ? recovery.composerRevision : -1, true)
    androidSelectionRecoveryRef.current = null
    publishRuntimeStatus(toRuntimeStatus(snapshot.runtimePhase), false)
    setPromptActivity(null)
    setPromptState('idle')
    return true
  }, [adoptAndroidConversation, adoptSnapshotControls, androidSelectionRecoveryRef, appActiveRef, publishRuntimeStatus, setPromptActivity, setPromptState, setShortcutStaging, shortcutDraftRef, transcriptRevisionRef])

  useEffect(() => {
    if (notices.length === 0) {
      return undefined
    }

    if (noticeInteraction.focused || noticeInteraction.hovered) return undefined

    const timeoutId = window.setTimeout(() => {
      setNotices((currentNotices) => {
        if (currentNotices.length <= 1) setNoticeInteraction({ focused: false, hovered: false })
        return currentNotices.slice(1)
      })
    }, NOTICE_AUTO_DISMISS_MS)

    return () => window.clearTimeout(timeoutId)
  }, [noticeInteraction, notices, setNoticeInteraction, setNotices])


  useEffect(() => {
    if (snapshotHydrationStateRef.current !== 'pending') return undefined
    let active = true
    let retryTimer: number | null = null
    const runtimeActivityRevision = runtimeActivityRevisionRef.current
    const scheduleHydrationRetry = (): void => {
      if (!active || snapshotHydrationStateRef.current !== 'pending') return
      if (retryTimer !== null) window.clearTimeout(retryTimer)
      retryTimer = window.setTimeout(hydrateSnapshot, 250)
    }
    function hydrateSnapshot(): void {
      if (!active || snapshotHydrationStateRef.current !== 'pending') return
      if (resetPendingRef.current) {
        scheduleHydrationRetry()
        return
      }
      const transcriptRevision = transcriptRevisionRef.current
      const composerRevision = composerEditRevisionRef.current
      const snapshotControlRevision = ++snapshotControlRequestRevisionRef.current
      const snapshotPreferenceRevision = getAndroidPreferencePublicationRevision()
      timingRef.current.startup('INITIAL_SNAPSHOT_REQUESTED')
      void loadAppSnapshot().then(async (snapshot) => {
        if (!active || snapshotHydrationStateRef.current !== 'pending') return
        timingRef.current.startup('INITIAL_SNAPSHOT_RECEIVED')
        if (transcriptRevision !== transcriptRevisionRef.current) {
          scheduleHydrationRetry()
          return
        }
        if (!adoptSnapshotControls(snapshot, snapshotControlRevision, snapshotPreferenceRevision)) {
          scheduleHydrationRetry()
          return
        }

        canonicalTranscriptRef.current = snapshot.transcript
        const coldDraftPending = isAndroidRuntime && androidAssistantRef.current === null && androidDraftPendingRef.current !== null
        adoptAndroidConversation(snapshot, coldDraftPending ? -1 : composerRevision, false)
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
        snapshotSessionIdRef.current = snapshot.sessionId
        snapshotHydrationStateRef.current = 'succeeded'
        setSnapshotHydrationState('succeeded')
        timingRef.current.startup('INITIAL_SNAPSHOT_APPLIED')
        if (snapshot.activeRequest === null && androidDraftPendingRef.current !== null) persistAndroidDraftRef.current(composerValueRef.current)
        if (snapshot.activeRequest === null && !resetPendingRef.current && promptRef.current === null) {
          setPromptActivity(null)
          setPromptState('idle')
        }
        if (snapshot.activeRequest === null || transcriptRevision !== transcriptRevisionRef.current) return

        setPromptState('stopping')
        setPromptActivity({ text: 'Recovering interrupted response…', compact: true })
        await recoverOrphanedRequest(
          snapshot,
          transcriptRevision,
          () => active && !resetPendingRef.current,
        )
      }).catch(() => {
        if (active && isAndroidRuntime) {
          scheduleHydrationRetry()
          return
        }
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
    }
    hydrateSnapshot()

    return () => {
      active = false
      if (retryTimer !== null) window.clearTimeout(retryTimer)
    }
  }, [adoptAndroidConversation, adoptSnapshotControls, androidAssistantRef, androidDraftPendingRef, canonicalTranscriptRef, composerEditRevisionRef, composerValueRef, persistAndroidDraftRef, promptRef, recoverOrphanedRequest, resetPendingRef, runtimeActivityRevisionRef, setMessages, setPromptActivity, setPromptState, setSnapshotControlsFresh, setSnapshotControlsInvalidated, setSnapshotHydrationState, setTtsEnabled, snapshotControlRequestRevisionRef, snapshotControlsFreshRef, snapshotControlsInvalidatedRef, snapshotHydrationStateRef, snapshotRuntimeHydratedRef, snapshotSessionIdRef, startupStateRef, synchronizeHydratedRuntimeStatus, timingRef, transcriptRevisionRef, ttsEnabledRef, ttsPersistedInitializedRef, ttsPersistedValueRef, ttsWriteRevisionRef])

  useEffect(() => {
    if (!isAndroidRuntime) return undefined
    let active = true
    let refreshInFlight = false
    let refreshQueued = false
    const runRefresh = (allowRetry: boolean): void => {
      if (document.visibilityState !== 'visible' || snapshotHydrationStateRef.current !== 'succeeded') return
      if (resetPendingRef.current && shortcutDraftRef.current?.needsNew === true) return
      if (androidSelectionRecoveryRef.current !== null && resetPendingRef.current) return
      if (refreshInFlight) {
        refreshQueued = true
        return
      }
      refreshInFlight = true
      let succeeded = false
      let failed = false
      const snapshotControlRevision = ++snapshotControlRequestRevisionRef.current
      const snapshotPreferenceRevision = getAndroidPreferencePublicationRevision()
      const transcriptRevision = transcriptRevisionRef.current
      const composerRevision = composerEditRevisionRef.current
      const selectionRecovery = androidSelectionRecoveryRef.current
      snapshotControlsFreshRef.current = false
      setSnapshotControlsFresh(false)
      void loadAppSnapshot().then((snapshot) => {
        if (!active) return
        if (androidSelectionRecoveryRef.current !== null) {
          if (resetPendingRef.current || selectionRecovery !== androidSelectionRecoveryRef.current || selectionRecovery === null) return
          succeeded = adoptRecoveredAndroidSelection(snapshot, selectionRecovery, snapshotControlRevision, snapshotPreferenceRevision)
          if (succeeded && androidDraftPendingRef.current !== null) persistAndroidDraftRef.current(composerValueRef.current)
          return
        }
        succeeded = adoptSnapshotControls(snapshot, snapshotControlRevision, snapshotPreferenceRevision)
        if (succeeded && document.visibilityState === 'visible' && transcriptRevision === transcriptRevisionRef.current && promptRef.current === null && !resetPendingRef.current) {
          adoptAndroidConversation(snapshot, composerRevision, true)
        }
      }).catch(() => { failed = true }).finally(() => {
        const latestFailed = failed && snapshotControlRevision === snapshotControlRequestRevisionRef.current && (allowRetry || refreshQueued)
        const staleRecoveryQueued = refreshQueued && !snapshotControlsFreshRef.current
        const retry = active && !succeeded && (latestFailed || staleRecoveryQueued) && document.visibilityState === 'visible'
        refreshQueued = false
        refreshInFlight = false
        if (retry) runRefresh(false)
      })
    }
    refreshAndroidSnapshotControlsRef.current = () => runRefresh(true)
    const refreshControls = (): void => runRefresh(true)
    document.addEventListener('visibilitychange', refreshControls)
    window.addEventListener('focus', refreshControls)
    window.addEventListener('pageshow', refreshControls)
    return () => {
      active = false
      refreshAndroidSnapshotControlsRef.current = () => undefined
      document.removeEventListener('visibilitychange', refreshControls)
      window.removeEventListener('focus', refreshControls)
      window.removeEventListener('pageshow', refreshControls)
    }
  }, [adoptAndroidConversation, adoptRecoveredAndroidSelection, adoptSnapshotControls, androidDraftPendingRef, androidSelectionRecoveryRef, composerEditRevisionRef, composerValueRef, persistAndroidDraftRef, promptRef, refreshAndroidSnapshotControlsRef, resetPendingRef, setSnapshotControlsFresh, shortcutDraftRef, snapshotControlRequestRevisionRef, snapshotControlsFreshRef, snapshotHydrationStateRef, transcriptRevisionRef])

  useEffect(() => {
    if (isAndroidRuntime || isBrowserPreviewRuntime()) return undefined
    let active = true
    let observedVersion: string | null = null
    let availabilityRevision = 0
    let inFlight = false
    let queued = false
    const availabilityKey = (capabilities: readonly import('./types/chat').StartupCapability[]): string =>
      capabilities.map(({ id, state }) => `${id}:${state}`).sort().join('|')
    const refresh = (): void => {
      if (!active || resetPendingRef.current || isSwitchingResponseProfileRef.current) return
      if (inFlight) { queued = true; return }
      inFlight = true
      const version = availabilityRevision
      const isAlive = session.captureLifetime()
      const controlRevision = ++snapshotControlRequestRevisionRef.current
      const preferenceRevision = getAndroidPreferencePublicationRevision()
      const contextRevision = transcriptRevisionRef.current
      const runtimeRevision = runtimeActivityRevisionRef.current
      void loadAppSnapshot().then((snapshot) => {
        if (!active || !isAlive() || version !== availabilityRevision || resetPendingRef.current ||
          isSwitchingResponseProfileRef.current || contextRevision !== transcriptRevisionRef.current) return
        if (!adoptSnapshotControls(snapshot, controlRevision, preferenceRevision)) return
        // Initialization may settle, but an availability read never takes over live runtime work.
        if (runtimeRevision === runtimeActivityRevisionRef.current && runtimeStatusRef.current === 'initializing' &&
          startupStateRef.current.kind === 'ready' && promptRef.current === null && snapshot.activeRequest === null) {
          synchronizeHydratedRuntimeStatus(toRuntimeStatus(snapshot.runtimePhase))
        }
      }).catch(() => {
        // Keep stale controls fenced until a native change or foreground retry obtains fresh truth.
      }).finally(() => {
        inFlight = false
        const refreshLatest = active && queued && !snapshotControlsFreshRef.current
        queued = false
        if (refreshLatest) refresh()
      })
    }
    observeDesktopStartupRef.current = (next): void => {
      if (!active || snapshotHydrationStateRef.current !== 'succeeded' || resetPendingRef.current ||
        (next.kind !== 'ready' && next.kind !== 'warming_model')) return
      const capabilities = availabilityKey(next.capabilities)
      const version = `${next.kind}:${next.selectedResponseProfile}:${capabilities}`
      const changed = observedVersion === null
        ? !snapshotControlsFreshRef.current ||
          (snapshotCapabilitiesRef.current !== null && availabilityKey(snapshotCapabilitiesRef.current) !== capabilities) ||
          snapshotResponseProfileRef.current !== next.selectedResponseProfile ||
          (runtimeStatusRef.current === 'initializing' && next.kind === 'ready')
        : version !== observedVersion
      observedVersion = version
      if (!changed) return
      availabilityRevision += 1
      // Invalidate immediately, even when the latest read must queue behind an older generation.
      snapshotControlRequestRevisionRef.current += 1
      snapshotControlsFreshRef.current = false
      setSnapshotControlsFresh(false)
      snapshotControlsInvalidatedRef.current = true
      setSnapshotControlsInvalidated(true)
      refresh()
    }
    const resumeStaleControls = (): void => {
      if (observedVersion !== null && snapshotHydrationStateRef.current === 'succeeded' &&
        !snapshotControlsFreshRef.current && document.visibilityState === 'visible') refresh()
    }
    window.addEventListener('focus', resumeStaleControls)
    window.addEventListener('pageshow', resumeStaleControls)
    document.addEventListener('visibilitychange', resumeStaleControls)
    return () => {
      active = false
      observeDesktopStartupRef.current = () => undefined
      window.removeEventListener('focus', resumeStaleControls)
      window.removeEventListener('pageshow', resumeStaleControls)
      document.removeEventListener('visibilitychange', resumeStaleControls)
    }
  }, [adoptSnapshotControls, isSwitchingResponseProfileRef, observeDesktopStartupRef, promptRef, resetPendingRef,
    runtimeActivityRevisionRef, runtimeStatusRef, session, setSnapshotControlsFresh, setSnapshotControlsInvalidated,
    snapshotCapabilitiesRef, snapshotControlRequestRevisionRef, snapshotControlsFreshRef, snapshotControlsInvalidatedRef,
    snapshotHydrationStateRef, snapshotResponseProfileRef, startupStateRef, synchronizeHydratedRuntimeStatus, transcriptRevisionRef])

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
  }, [setUiTextSize, snapshotHydrationState, uiTextSizePendingRevisionRef, uiTextSizePersistedInitializedRef, uiTextSizePersistedRevisionRef, uiTextSizePersistedValueRef])

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
  }, [addNotice, assistantSettingsPendingRef, assistantSettingsRef, assistantSettingsWriteRevisionRef, persistedAssistantSettingsRef, setAssistantSettings, setAssistantSettingsLoadError, setAssistantSettingsPending, snapshotHydrationState])

  const voiceCuesCanPrepare = startupState.kind === 'ready' && capabilityIsAvailable(startupState, 'tts')
  useEffect(() => {
    if (!voiceCuesCanPrepare || !isNativeTauriRuntime() || preparedCueAssetPaths !== null) return
    let active = true
    let attempts = 0
    let retry: ReturnType<typeof setTimeout> | null = null
    const prepare = (): void => {
      attempts += 1
      void prepareVoiceCues()
        .then((paths) => { if (active) setPreparedCueAssetPaths(paths) })
        .catch(() => {
          if (active && attempts < 3) retry = setTimeout(prepare, 2_000)
        })
    }
    prepare()
    return () => { active = false; if (retry !== null) clearTimeout(retry) }
  }, [voiceCuesCanPrepare, preparedCueAssetPaths, setPreparedCueAssetPaths])

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
          cancelPrompt()
        }
      }
    }
    window.addEventListener('keydown', onEscape)
    return () => window.removeEventListener('keydown', onEscape)
  }, [cancelPrompt, promptRef, promptState, settingsOpen, startupState])

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
  }, [setUiTheme, snapshotHydrationState, uiThemePendingRevisionRef, uiThemePersistedInitializedRef, uiThemePersistedRevisionRef, uiThemePersistedValueRef])

  const persistUiTextSize = async (nextTextSize: UiTextSize, previousTextSize: UiTextSize): Promise<void> => {
    const revision = ++uiTextSizeWriteRevisionRef.current
    uiTextSizePendingRevisionRef.current = revision
    setUiTextSize(nextTextSize)

    try {
      const write = session.preferences.textSize.enqueue(() =>
        invokeTauriCommand('set_ui_text_size', { textSize: nextTextSize }),
      )
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
    setUiThemeWriteError(null)
    uiThemePendingRevisionRef.current = revision
    setUiTheme(nextTheme)

    try {
      const write = session.preferences.theme.enqueue(() =>
        invokeTauriCommand('set_ui_theme', { theme: nextTheme }),
      )
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
      setUiThemeWriteError(toDisplayErrorMessage(error))
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

  const changeUiTheme = (nextTheme: UiTheme): void => {
    if (nextTheme !== uiTheme) void persistUiTheme(nextTheme, uiTheme)
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

  const currentInitialSilenceTimeoutMs = (): number => {
    if (startupStateRef.current.kind === 'ready' || startupStateRef.current.kind === 'warming_model') {
      return startupStateRef.current.initialSilenceTimeoutMs
    }
    return DEFAULT_INITIAL_SILENCE_TIMEOUT_MS
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
  }, [publishRuntimeStatus, setStartupState, setTtsEnabled, snapshotCapabilitiesRef, snapshotHydrationRevisionRef, snapshotHydrationStateRef, snapshotResponseProfileRef, startupStateRef, synchronizeHydratedRuntimeStatus, ttsEnabledRef, ttsPersistedInitializedRef, ttsPersistedValueRef, ttsWriteRevisionRef])

  const startupLoadAllowed = !isAndroidRuntime || snapshotHydrationState !== 'pending'
  useEffect(() => {
    if (!startupLoadAllowed) return undefined
    let active = true

    void (async () => {
      while (active) {
        const startupSnapshotRevision = snapshotHydrationRevisionRef.current
        const startupRuntimeRevision = runtimeActivityRevisionRef.current
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
        const syncRuntimeStatus = !snapshotRuntimeHydratedRef.current && (!hydrated || (
          previousKind === 'warming_model' && runtimeStatusRef.current === 'initializing' &&
          startupRuntimeRevision === runtimeActivityRevisionRef.current
        ))
        // A startup response predating a newer canonical publication cannot invalidate that owner.
        if (startupSnapshotRevision === snapshotHydrationRevisionRef.current) observeDesktopStartupRef.current(nextState)
        const publishedState = applyStartupState(nextState, syncRuntimeStatus, startupSnapshotRevision)

        if (publishedState.kind === 'ready' && !isBrowserPreviewRuntime()) {
          const newlyUnavailableCapabilities = publishedState.capabilities.filter(({ id, state }) => {
            if (isAndroidRuntime && ['opencode', 'local_quality', 'qwen_prediction', 'deep', 'review', 'wake_word', 'vad', 'parakeet', 'tts'].includes(id)) return false
            if (['wake_word', 'vad', 'parakeet', 'tts'].includes(id)) return false
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
  }, [addNotice, applyStartupState, isSwitchingResponseProfileRef, observeDesktopStartupRef, runtimeActivityRevisionRef, runtimeStatusRef, snapshotHydrationRevisionRef, snapshotRuntimeHydratedRef, startupLoadAllowed, startupStateRef])

  useEffect(() => {
    const cuePlayer = cuePlayerRef.current
    session.activate(() => {
      if (import.meta.env.VITE_VOXGOLEM_BENCHMARK === '1') reportAppBenchmarkStage('benchmark_lifetime_retire')
      timingRef.current.retireRequest()
      cuePlayer.stop()
      const activePrompt = promptRef.current
      if (activePrompt !== null) {
        if (activePrompt.renderFrameId !== null) window.cancelAnimationFrame(activePrompt.renderFrameId)
        activePrompt.renderFrameId = null
        activePrompt.renderFrameRevision += 1
        activePrompt.terminal = true
      }
      promptRef.current = null
      cancelTts()
      if (wakeConfidenceHoldRef.current !== null) {
        window.clearTimeout(wakeConfidenceHoldRef.current)
      }
      wakeConfidenceHoldRef.current = null
      pendingWakeConfidenceRef.current = null
      partialTranscriptionAllowedRef.current = false
      partialTranscriptionRef.current = null
      completionLifecycleRef.current += 1
      latestTypedCompletionRef.current = null
      micStartInFlightRef.current = false
      micAutoStartedRef.current = false
      liveAudioSessionIdRef.current += 1
      liveAudioStartAbortRef.current?.abort()
      liveAudioStartAbortRef.current = null
      liveAudioSourceRef.current?.stop()
      liveAudioSourceRef.current = null
      if (isBrowserPreviewRuntime()) teardownBrowserPreviewRuntime()
    })
    return () => session.dispose()
  }, [cancelTts, completionLifecycleRef, cuePlayerRef, latestTypedCompletionRef, liveAudioSessionIdRef, liveAudioSourceRef, liveAudioStartAbortRef, micAutoStartedRef, micStartInFlightRef, partialTranscriptionAllowedRef, partialTranscriptionRef, pendingWakeConfidenceRef, promptRef, session, timingRef, wakeConfidenceHoldRef])

  useEffect(() => {
    if (!settingsOpen || !supportsDesktopRuntime) {
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
  }, [setAudioInputDevices, settingsOpen, supportsDesktopRuntime])


  const { instantAssistantOptions } = session.getModelsSnapshot()
  const selectedInstantOption = instantAssistantOptions
    .find((option) => option.value === assistantSettings.instant)
  const nativeConversationNeedsRecovery = ['saved_local_state_failed', 'draft_not_saved', 'storage_error'].includes(androidAssistant?.error ?? '')
  const voiceSnapshotControlsUsable = snapshotControlsFresh || (
    !snapshotControlsInvalidated && snapshotHydrationState === 'pending'
  )
  const voiceSnapshotControlsUsableNow = (): boolean => snapshotControlsFreshRef.current || (
    !snapshotControlsInvalidatedRef.current && snapshotHydrationStateRef.current === 'pending'
  )
  const fallbackCueAssetPaths = startupState.kind === 'ready'
    ? startupState.cueAssetPaths
    : DEFAULT_CUE_ASSET_PATHS
  const cueAssetPaths = preparedCueAssetPaths ?? fallbackCueAssetPaths
  useEffect(() => {
    cueAssetPathsRef.current = cueAssetPaths
    fallbackCueAssetPathsRef.current = fallbackCueAssetPaths
  }, [cueAssetPaths, cueAssetPathsRef, fallbackCueAssetPaths, fallbackCueAssetPathsRef])
  const assistantControlsDisabled = startupState.kind !== 'ready' || assistantSettingsPending || isSwitchingResponseProfile || !snapshotControlsFresh || updateRuntimeBlocked || androidVoiceActive
  const instantControlsDisabled = assistantControlsDisabled || resetPending || promptState !== 'idle' ||
    runtimeStatus === 'processing' || runtimeStatus === 'executing'
  const clearAssistantSettingsWriteFeedback = (): void => {
    setAssistantSettingsWriteError(null)
    setNotices((current) => {
      const remaining = current.filter((notice) => notice.title !== ASSISTANT_SETTINGS_WRITE_FAILURE_TITLE)
      return remaining.length === current.length ? current : remaining
    })
  }
  const persistAssistantSettings = async (next: AssistantSettings): Promise<boolean> => {
    const revision = ++assistantSettingsWriteRevisionRef.current
    clearAssistantSettingsWriteFeedback()
    assistantSettingsPendingRef.current = true
    setAssistantSettingsPending(true)
    assistantSettingsRef.current = next
    setAssistantSettings(next)
    const write = session.preferences.assistant.enqueue(async () =>
      parseAssistantSettings(await invokeTauriCommand('set_assistant_settings', { settings: serializeAssistantSettings(next) })))
    try {
      const persisted = await write
      persistedAssistantSettingsRef.current = persisted
      if (revision === assistantSettingsWriteRevisionRef.current) {
        assistantSettingsRef.current = persisted
        setAssistantSettings(persisted)
        clearAssistantSettingsWriteFeedback()
      }
      return true
    } catch (error) {
      const message = toDisplayErrorMessage(error)
      if (revision === assistantSettingsWriteRevisionRef.current) {
        assistantSettingsRef.current = persistedAssistantSettingsRef.current
        setAssistantSettings(persistedAssistantSettingsRef.current)
        setAssistantSettingsWriteError(message)
      }
      addNotice({ tone: 'error', title: ASSISTANT_SETTINGS_WRITE_FAILURE_TITLE, message })
      return false
    } finally {
       if (revision === assistantSettingsWriteRevisionRef.current) {
         assistantSettingsPendingRef.current = false
         setAssistantSettingsPending(false)
       }
    }
  }
  const changeInstant = async (instant: AssistantSettings['instant']): Promise<void> => {
    if (instantControlsDisabled || assistantSettingsPendingRef.current || isSwitchingResponseProfileRef.current ||
      resetPendingRef.current || !snapshotControlsFreshRef.current || promptRef.current !== null ||
      runtimeStatusRef.current === 'processing' || runtimeStatusRef.current === 'executing') return
    const revision = ++instantSelectionRevisionRef.current
    const option = instantAssistantOptions.find((item) => item.value === instant)
    if (!option || (!option.available && !localInstantCanRetry(startupStateRef.current, instant))) return
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


  const reportCuePlaybackError = (cueType: 'start_listening' | 'stop_listening', error: unknown): void => {
    const message = error instanceof Error ? error.message : 'Unknown cue playback error'

    console.error('[cue] playback failure', {
      cueType,
      runtimeStatus: runtimeStatusRef.current,
    })

    recordRuntimeDiagnostic(
      'cue',
      `Cue playback error: ${message} [cue=${cueType}]`,
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

       void playCueWithFallback(cueType, cueAssetPathsRef.current, fallbackCueAssetPathsRef.current, cuePlayerRef.current)
        .then((started) => {
          if (started) voiceTelemetry.record('cue_play_started', {
            details: {
              cueType,
              source: 'apply_runtime_status',
            },
          })
        })
        .catch((error: unknown) => {
          if (import.meta.env.VITE_VOXGOLEM_BENCHMARK === '1') reportAppBenchmarkStage('benchmark_cue_error')
          console.error('[cue] playback failure while applying runtime status', {
            cueType,
            runtimeStatusBefore: previousStatus,
            runtimeStatusAfter: nextStatus,
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
  ): void => {
    const previousStatus = runtimeStatusRef.current
    const nextStatus = toRuntimeStatus(runtimePhase.runtimePhase)

    applyRuntimeStatus(nextStatus)
    if (
      previousStatus === 'sleeping' ||
      typeof runtimePhase.telemetry?.transcriptionStartedMs === 'number' ||
      typeof runtimePhase.telemetry?.transcriptionCompletedMs === 'number'
    ) {
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

  }

  const syncRuntimeControl = async (
    command: 'mark_silence',
    options: {
      readonly args?: RuntimeControlArgs
      readonly fallbackEvent?: Parameters<typeof transitionRuntimeStatus>[1]
      readonly isCurrent?: () => boolean
    } = {},
  ): Promise<RuntimeControlResult | null> => {
    if (startupStateRef.current.kind !== 'ready') {
      return null
    }

    const { args, fallbackEvent, isCurrent } = options
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

      applyRuntimeControlResult(runtimePhase)
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
    nativeRequestId?: string,
  ): Promise<void> => {
    const admission = session.admitPrompt(prompt, source, nativeRequestId, updateRuntimeBlocked)
    if (admission === null) return
    const admittedAt = performance.now()
    const appActiveRef = { get current() { return admission.isCurrent() } }
    const currentSettings = admission.settings
    const executingStatus = applyTransition(runtimeStatusRef.current, 'submit_prompt')
    const promptRuntimeActivityRevision = runtimeActivityRevisionRef.current
    const expectedCanonicalSessionId = snapshotSessionIdRef.current
    const promptComposerRevision = composerEditRevisionRef.current
    const priorCanonicalExecutions = new Set(
      canonicalTranscriptRef.current.map((turn) => executionKey(turn)),
    )
    clearPartialTranscript()
    cancelTts()
    setResponseAnnouncement('')

    const requestId = nativeRequestId ?? `request-${Date.now()}-${Math.random().toString(36).slice(2)}`
    const projectionRevision = ++promptProjectionRevisionRef.current
    const userId = `user-${projectionRevision}-${requestId}`
    const assistantId = `assistant-${projectionRevision}-${requestId}`
    const requestTiming = timingRef.current.admit(assistantId, admittedAt)
    const promptProjection: PromptProjection = {
      id: requestId,
      userId,
      assistantId,
      text: '',
      error: null,
      terminal: false,
      presentationCancelled: false,
      cancelPending: false,
      tts: !isAndroidRuntime && ttsEnabledRef.current,
      corrected: false,
      stages: [
        { stage: 'instant', status: 'running' },
        ...(currentSettings.deepEnabled ? [{ stage: 'deep', status: 'queued' } as const] : []),
        ...(currentSettings.reviewEnabled ? [{ stage: 'review', status: 'queued' } as const] : []),
      ],
      priorVersions: [],
      sources: [],
      ttsFirstLineSpoken: false,
      renderFrameId: null,
      renderFrameRevision: 0,
      admittedAt,
    }
    promptRef.current = promptProjection
    setPromptState('executing')
    setPromptActivity({ text: 'Executing prompt…', compact: true })
    setMessages((currentMessages) => [
      ...currentMessages,
      {
        id: userId,
        role: 'user',
        content: prompt,
        observation: { requestId, outcome: 'pending', stageStatus: 'running', contentKind: 'text' },
      },
    ])
    session.renderPromptProjection(promptProjection)

    if (source === 'typed') {
      composerValueRef.current = ''
      setComposerValue('')
      clearCompletionDisplay()
    }

    const handleEvent = (event: import('./types/chat').PromptExecutionEvent): void => {
      if (!appActiveRef.current) return
      const active = promptRef.current
      if (active !== promptProjection || active.id !== event.requestId || active.terminal || active.presentationCancelled) {
        return
      }
      if (event.kind === 'text') {
        if (source === 'voice' && active.text.length === 0 && event.text.length > 0) {
          voiceTelemetry.record('provider_first_text')
        }
        const firstTextTimings = requestTiming.received(event.text)
        if (isAndroidRuntime && firstTextTimings !== undefined) active.customDirectTimings = firstTextTimings
        const hadCompleteLine = active.text.includes('\n')
        active.text += event.text
        if (source === 'voice' && !hadCompleteLine && active.text.includes('\n')) {
          voiceTelemetry.record('provider_first_line')
        }
        if (
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
        if (firstTextTimings !== undefined) session.flushPromptProjection(active)
        else session.schedulePromptProjection(active)
      }
      if (event.kind === 'correction') {
        cancelPromptProjectionFrame(active)
        active.corrected = true
        if (active.text.trim().length > 0) {
          active.priorVersions = [...active.priorVersions, { id: `instant-${active.priorVersions.length + 1}`, text: active.text, label: 'Before correction' }]
        }
        active.text = event.text
        const timings = requestTiming.received(event.text)
        if (isAndroidRuntime && timings !== undefined) active.customDirectTimings = timings
        active.stages = active.stages.map((stage) => stage.stage === event.stage ? { ...stage, status: 'corrected', detail: event.correction } : stage)
        session.renderPromptProjection(active)
        setPromptActivity({ text: event.correction, compact: false })
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
        const stagePrefix = event.stage === 'instant' ? 'Response' : `${event.stage[0]?.toUpperCase()}${event.stage.slice(1)} stage`
        setPromptActivity({ text: `${stagePrefix} ${event.status}`, compact: event.status !== 'failed' })
      }
      if (event.kind === 'sources') {
        active.sources = mergeAnswerSources(active.sources, event.sources)
        setMessages((current) => current.map((message) => message.id === active.assistantId
          ? { ...message, answerStage: { stages: active.stages, priorVersions: active.priorVersions, sources: active.sources } } : message))
      }
      if (event.kind === 'custom_direct_timings') {
        if (!isAndroidRuntime) {
          requestTiming.provider(event.timings)
          active.customDirectTimings = event.timings
          setMessages((current) => current.map((message) => message.id === active.assistantId
            ? { ...message, customDirectTimings: event.timings }
            : message))
        }
      }
      if (event.kind === 'reasoning') {
        setPromptActivity({ text: `Reasoning: ${event.text}`, compact: true })
      }
      const waitingStatus = event.kind === 'status' ? event.message : event.kind === 'tool' ? event.detail : undefined
      if (waitingStatus !== undefined && /^(Searching|Reading|Fallback|Partial)\b/u.test(waitingStatus)) {
        active.thinkingStatus = waitingStatus
        setMessages((current) => current.map((message) => message.id === active.assistantId && message.content.trim().length === 0
          ? { ...message, thinkingStatus: waitingStatus } : message))
      }
      if (event.kind === 'status') {
        setPromptActivity({ text: event.message, compact: true })
      }
      if (event.kind === 'tool') {
        setPromptActivity({ text: `${event.tool}: ${event.status}${event.detail ? ` - ${event.detail}` : ''}`, compact: event.status !== 'error' })
      }
      if (event.kind === 'error') {
        active.error = event.message
        setPromptActivity({ text: event.message, compact: false })
      }
    }
    if (source === 'voice') voiceTelemetry.record('provider_request_started', { details: { provider: currentSettings.instant } })
    let result: PromptExecutionResult
    let commandRejected = false
    const inlineControlRevision = snapshotControlRequestRevisionRef.current
    const inlinePreferenceRevision = getAndroidPreferencePublicationRevision()
    const inlineRuntimeRevision = runtimeActivityRevisionRef.current
    const inlineTranscriptRevision = transcriptRevisionRef.current
    const inlineAssistant = androidAssistantRef.current
    try {
      requestTiming.dispatch()
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
      if (source === 'voice') voiceTelemetry.record('provider_completed', { details: { outcome: result.outcome } })
      if (result.customDirectTimings) {
        if (!isAndroidRuntime) {
          requestTiming.provider(result.customDirectTimings)
          const stableFirstTextMs = active.customDirectTimings?.firstTextMs ?? result.customDirectTimings.firstTextMs
          active.customDirectTimings = {
            firstTextMs: stableFirstTextMs,
            completedMs: result.customDirectTimings.completedMs,
          }
        }
      }
      const acceptedCanonicalTurn = result.canonicalTurn !== undefined && isExpectedCanonicalTurn(
        result.canonicalTurn,
        requestId,
        expectedCanonicalSessionId,
        priorCanonicalExecutions,
        isAndroidRuntime,
      ) ? result.canonicalTurn : undefined
      if (acceptedCanonicalTurn !== undefined) {
        requestTiming.canonicalAnswer(canonicalMessageId(acceptedCanonicalTurn.answer))
        const timings = requestTiming.received(acceptedCanonicalTurn.answer.content.text)
        if (isAndroidRuntime && timings !== undefined) active.customDirectTimings = timings
      }
      if (result.canonicalTurn !== undefined && acceptedCanonicalTurn === undefined) {
        result = {
          requestId: result.requestId,
          runtimePhase: result.runtimePhase,
          outcome: 'error',
          errorMessage: 'The returned response did not match the active execution.',
        }
      }
      if (result.outcome !== 'completed') cancelTts()
      if (result.outcome === 'completed') {
        session.flushPromptProjection(active)
      } else {
        cancelPromptProjectionFrame(active)
        if (active.text.trim().length === 0) {
          setMessages((current) => current.filter((message) => message.id !== active.assistantId))
        }
      }
      active.terminal = true
      const observedStages = active.stages
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
      let reconciledTurn: Turn | undefined = acceptedCanonicalTurn
      let reconciledRuntimePhase: BackendRuntimePhase | null = null
      let reconciledRuntimeActivityRevision: number | null = null
      if (snapshotHydrationStateRef.current === 'succeeded') {
        setPromptState('reconciling')
        setPromptActivity({ text: 'Synchronizing response…', compact: true })
        const transcriptRevision = transcriptRevisionRef.current
        const inline = result.terminalSnapshot
        const inlineTurn = inline === undefined ? undefined : findNewTurnForRequest(
          inline.transcript, requestId, expectedCanonicalSessionId, priorCanonicalExecutions,
        )
        const acceptInline = result.outcome === 'completed' && acceptedCanonicalTurn !== undefined &&
          inline !== undefined && inline.activeRequest === null && inline.assistant !== undefined &&
          (!isAndroidRuntime || inline.runtimePhase === 'sleeping') &&
          inline.assistant.error === null && inline.assistant.interrupted === false &&
          inline.assistant.selectedHead !== null && inline.assistant.revision !== inlineAssistant?.revision &&
          inline.sessionId === expectedCanonicalSessionId && inlineTurn !== undefined &&
          JSON.stringify(inlineTurn) === JSON.stringify(acceptedCanonicalTurn) &&
          inlineControlRevision === snapshotControlRequestRevisionRef.current &&
          inlinePreferenceRevision === getAndroidPreferencePublicationRevision() &&
          inlineRuntimeRevision === runtimeActivityRevisionRef.current &&
          inlineTranscriptRevision === transcriptRevisionRef.current &&
          promptComposerRevision === composerEditRevisionRef.current &&
          inlineAssistant === androidAssistantRef.current && !resetPendingRef.current
        const snapshotControlRevision = acceptInline ? inlineControlRevision : ++snapshotControlRequestRevisionRef.current
        if (acceptInline) requestTiming.reconciliation('inline')
        const snapshotPreferenceRevision = acceptInline ? inlinePreferenceRevision : getAndroidPreferencePublicationRevision()
        let snapshot: Awaited<ReturnType<typeof loadAppSnapshot>> | null = acceptInline ? inline! : null
        if (acceptInline) reconciledRuntimeActivityRevision = inlineRuntimeRevision
        if (snapshot === null) {
          const reconciliation = await session.prompt.reconcile({
            isCurrent: () => appActiveRef.current && promptRef.current === active && !resetPendingRef.current && transcriptRevision === transcriptRevisionRef.current,
            runtimeRevision: () => runtimeActivityRevisionRef.current,
            load: loadAppSnapshot,
            requested: () => requestTiming.reconciliation('rpc'),
          })
          snapshot = reconciliation?.snapshot ?? null
          reconciledRuntimeActivityRevision = reconciliation?.runtimeRevision ?? null
        }
        if (!appActiveRef.current || promptRef.current !== active || resetPendingRef.current || transcriptRevision !== transcriptRevisionRef.current) return
        if (snapshot === null) {
          if (acceptedCanonicalTurn === undefined) {
            setPromptActivity({ text: 'Session recovery required. Use New Chat to retry.', compact: false })
            return
          }
          const transcript = appendCanonicalTurn(canonicalTranscriptRef.current, acceptedCanonicalTurn)
          setMessages((current) => replaceCommittedTranscript(current, transcript))
          canonicalTranscriptRef.current = transcript
          snapshotSessionIdRef.current = acceptedCanonicalTurn.executionId.sessionId
          if (isAndroidRuntime && !snapshotControlsFreshRef.current) {
            refreshAndroidSnapshotControlsRef.current()
          }
        } else {
          adoptSnapshotControls(snapshot, snapshotControlRevision, snapshotPreferenceRevision)
          adoptAndroidConversation(snapshot, promptComposerRevision, false)
          const snapshotTurn = findNewTurnForRequest(
            snapshot.transcript,
            requestId,
            expectedCanonicalSessionId,
            priorCanonicalExecutions,
          )
          reconciledTurn = snapshotTurn ?? acceptedCanonicalTurn
          if (reconciledTurn !== undefined) {
            requestTiming.canonicalAnswer(canonicalMessageId(reconciledTurn.answer))
            const timings = requestTiming.received(reconciledTurn.answer.content.text)
            if (isAndroidRuntime && timings !== undefined) active.customDirectTimings = timings
          }
          const transcript = appendCanonicalTurn(snapshot.transcript, snapshotTurn === undefined ? acceptedCanonicalTurn : undefined)
          setMessages((current) => replaceCommittedTranscript(current, transcript))
          canonicalTranscriptRef.current = transcript
          snapshotSessionIdRef.current = snapshot.sessionId
          reconciledRuntimePhase = snapshot.runtimePhase
        }
      }
      if (isAndroidRuntime && source === 'typed' && result.outcome !== 'completed' && composerEditRevisionRef.current === promptComposerRevision) {
        composerValueRef.current = prompt
        setComposerValue(prompt)
      }
      promptRef.current = null
      session.settlePromptAdmission(admission)
      if (isAndroidRuntime && composerEditRevisionRef.current !== promptComposerRevision) persistAndroidDraft(composerValueRef.current)
      setPromptState('idle')
      setPromptActivity(null)
      if (isAndroidRuntime) queueMicrotask(() => drainAndroidVoiceRequestRef.current())
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
      } else if (
        promptRuntimeActivityRevision === runtimeActivityRevisionRef.current
      ) {
        publishRuntimeStatus('sleeping', false)
      }
      const canonicalSettlementRequired = snapshotHydrationStateRef.current === 'succeeded'
      const canonicalCanConfirmCompletion = result.outcome === 'completed' || commandRejected
      const effectiveOutcome = reconciledTurn !== undefined && canonicalCanConfirmCompletion
        ? 'completed'
        : canonicalSettlementRequired && result.outcome === 'completed'
          ? 'error'
          : result.outcome
      const terminalText = reconciledTurn?.answer.content.text ?? active.text
      setMessages((current) => current.map((message) => message.observation?.requestId === requestId ? {
        ...message,
        observation: { requestId, outcome: effectiveOutcome, stageStatus: effectiveOutcome === 'completed' ? 'completed' : effectiveOutcome === 'error' ? 'failed' : 'cancelled',
          contentKind: message.role === 'assistant' ? effectiveOutcome === 'error' || terminalText.trim().length === 0 ? 'error' : message.contentKind ?? 'text' : 'text' },
      } : message))
      const frontendTimings = requestTiming.settle()
      if (isAndroidRuntime && frontendTimings !== undefined) {
        active.customDirectTimings = frontendTimings
        if (reconciledTurn === undefined) setMessages((current) => current.map((message) =>
          message.id === active.assistantId ? { ...message, customDirectTimings: frontendTimings } : message))
      }
      if (isAndroidRuntime && effectiveOutcome === 'completed' && composerEditRevisionRef.current === promptComposerRevision) {
        androidDraftPendingRef.current = null
        androidDraftWriteFailureRef.current = null
      }
      if (effectiveOutcome === 'completed' && reconciledTurn !== undefined) {
        const answerId = canonicalMessageId(reconciledTurn.answer)
        setMessages((current) => current.map((message) => message.id === answerId && message.role === 'assistant' ? {
          ...message,
          ...(active.customDirectTimings === undefined ? {} : { customDirectTimings: active.customDirectTimings }),
          answerStage: {
            ...message.answerStage,
            stages: settleCanonicalAnswerStages(observedStages, reconciledTurn.finalStage),
            priorVersions: [...active.priorVersions],
            sources: [...active.sources],
          },
        } : message))
      }
      if (effectiveOutcome === 'completed' && terminalText.trim().length > 0) {
        setResponseAnnouncement(formatResponseAnnouncement(
          reconciledTurn?.answer.content.kind ?? 'text',
          terminalText,
        ))
      }
      if (effectiveOutcome !== 'completed') {
        cancelTts()
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
        if (reconciledTurn === undefined) setMessages((current) => current.filter((message) => message.id !== active.assistantId))
        addNotice({
          tone: 'info',
          title: 'No response',
          message: 'No response was returned. Try again.',
        })
      } else if (
        appActiveRef.current &&
        !active.presentationCancelled &&
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

  useEffect(() => {
    drainAndroidVoiceRequestRef.current = () => {
      const pending = pendingAndroidVoiceRequestRef.current
      if (pending === null) return
      if (pending.inputEpoch !== androidVoiceInputEpochRef.current || pending.composerRevision !== composerEditRevisionRef.current) { pendingAndroidVoiceRequestRef.current = null; return }
      if (!appActiveRef.current || (document.visibilityState !== 'visible' &&
        androidVoiceStatusRef.current?.retainedSubmissionId !== pending.requestId)) return
      if (shortcutDraftRef.current?.needsNew === true || androidSelectionRecoveryRef.current !== null || promptRef.current !== null || resetPendingRef.current || !snapshotControlsFreshRef.current || assistantSettingsPendingRef.current) return
      if (pending.sessionId !== snapshotSessionIdRef.current || pending.selectedHead !== (androidAssistantRef.current?.selectedHead ?? null)) { pendingAndroidVoiceRequestRef.current = null; androidVoiceStopRef.current?.(); return }
      pendingAndroidVoiceRequestRef.current = null
      void runPrompt(pending.text, 'typed', pending.requestId)
    }
  })

  useEffect(() => { drainAndroidVoiceRequestRef.current() }, [androidVoiceStatus, drainAndroidVoiceRequestRef])

  async function synthesizeAndPlayAssistantReply(text: string): Promise<void> {
    await session.audio.speakReply(text, {
      ttsGeneration: ttsGenerationRef,
      ttsEnabledRef,
      voiceTelemetry,
      reservePlaybackId: () => invokeTauriCommand('reserve_local_tts_playback_id'),
      finishPlayback: (playbackId) => invokeTauriCommand('finish_tts_playback', { playbackId }),
      speakTts: (speechText, playbackId) => invokeTauriCommand('speak_local_tts', { text: speechText, playbackId }),
      parseRequestId: (val, ctx) => parseNativeAudioRequestId(val, ctx),
      toDisplayErrorMessage,
      addNotice,
      recordDiagnostic: (category, message) => recordRuntimeDiagnostic(category as RuntimeDiagnosticKind, message),
      setPlaying: setTtsPlaying,
      setPendingCount: setPendingTtsCommands,
    })
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
    heardSpeech = true,
  ): Promise<void> => {
    if (!voiceSnapshotControlsUsableNow()) return
    voiceTelemetry.record('silence_detected')
    if (heardSpeech) {
      voiceTelemetry.record('cue_play_requested', {
        details: {
          cueType: 'stop_listening',
          source: 'mark_silence',
        },
      })
      void playCueWithFallback('stop_listening', cueAssetPathsRef.current, fallbackCueAssetPathsRef.current, cuePlayerRef.current)
        .then((started) => { if (started) voiceTelemetry.record('cue_play_started', {
          details: {
            cueType: 'stop_listening',
            source: 'mark_silence',
          },
        }) })
        .catch((error: unknown) => reportCuePlaybackError('stop_listening', error))
    }

    if (liveAudioSessionId !== liveAudioSessionIdRef.current || !voiceSnapshotControlsUsableNow()) return

    voiceTelemetry.record('mark_silence_submitted')
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
    voiceTelemetry.record('mark_silence_resolved')
    clearPartialTranscript()
    maybeRunVoiceTranscript(runtimePhase)
  }

  const stopLiveAudio = (): void => {
    liveAudioSessionIdRef.current += 1
    markSilenceInFlightRef.current = null
    liveAudioStartAbortRef.current?.abort()
    liveAudioStartAbortRef.current = null
    liveAudioSourceRef.current?.stop()
    liveAudioSourceRef.current = null
    voiceActivityStateRef.current = createVoiceActivityState()
    resetWakeConfidence()
    setMicStarting(false)
    setMicActive(false)
    clearPartialTranscript()

  }

  const reportLiveAudioError = (error: unknown): void => {
    if (import.meta.env.VITE_VOXGOLEM_BENCHMARK === '1') reportAppBenchmarkStage('benchmark_audio_error')
    const message = error instanceof Error ? error.message : 'Live audio capture failed'
    const recovery = message === 'Native microphone processing fell behind'
      ? ' Capture stopped; select Start mic to resume.'
      : ''

    recordRuntimeDiagnostic('audio', `Live audio error: ${message}`)
    addNotice({
      tone: 'error',
      title: 'Microphone unavailable',
      message: `${message}${recovery}`,
    })
    stopLiveAudio()
  }

  const startMic = async (): Promise<void> => {
    if (updateRuntimeBlocked || resetPendingRef.current || !voiceSnapshotControlsUsableNow() || !voiceInputReady(startupStateRef.current) || liveAudioSourceRef.current !== null || micStarting || micStartInFlightRef.current) {
      return
    }
    micStartInFlightRef.current = true
    const startAbort = new AbortController()
    liveAudioStartAbortRef.current = startAbort

    const liveAudioSessionId = liveAudioSessionIdRef.current + 1
    liveAudioSessionIdRef.current = liveAudioSessionId
    const wakeClipBuffer = createWakeClipBuffer()
    let wakeClipSleeping = true

    setMicStarting(true)

    try {
      const liveAudioSource = await startLiveAudioSource({
        signal: startAbort.signal,
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
            if (wakeClipSleeping) wakeClipBuffer.push(frame)
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

            const detected = typeof status?.telemetry?.wakeDetectedMs === 'number'
            if (detected) {
              const samples = wakeClipBuffer.take()
              const confidence = status?.telemetry?.wakeConfidence
              if (typeof confidence === 'number' && Number.isFinite(confidence)) {
                void invokeTauriCommand('save_wake_clip', { samples, confidence }).catch((reason: unknown) => {
                  addNotice({ tone: 'error', title: 'Wake clip not saved', message: reason instanceof Error ? reason.message : String(reason) })
                })
              }
            }
            if (liveAudioSessionId !== liveAudioSessionIdRef.current || !voiceSnapshotControlsUsableNow() || !voiceInputReady(startupStateRef.current)) {
              wakeClipBuffer.clear()
              return
            }

            if (status !== null) {
              const nextStatus = toRuntimeStatus(status.runtimePhase)
              if (!detected && nextStatus !== 'sleeping') wakeClipBuffer.clear()
              wakeClipSleeping = nextStatus === 'sleeping'

              applyRuntimeControlResult(status)
              voiceActivityStateRef.current = syncVoiceActivityState(
                voiceActivityStateRef.current,
                  nextStatus,
                  status.lastActivityMs,
                  status.heardSpeech,
              )

              if (nextStatus === 'listening' && autoStopOnSilenceRef.current) {
                const voiceActivityUpdate = updateVoiceActivityState(
                  voiceActivityStateRef.current,
                  nowMs,
                  currentInitialSilenceTimeoutMs(),
                  currentSilenceTimeoutMs(),
                )

                voiceActivityStateRef.current = voiceActivityUpdate.state

                if (voiceActivityUpdate.shouldMarkSilence && markSilenceInFlightRef.current === null) {
                  markSilenceInFlightRef.current = liveAudioSessionId
                  void handleMarkSilence(liveAudioSessionId, frameId, voiceActivityUpdate.state.heardSpeech)
                    .catch((error: unknown) => {
                      if (liveAudioSessionId !== liveAudioSessionIdRef.current) return
                      enterRuntimeError()
                      reportLiveAudioError(error)
                    })
                    .finally(() => {
                      if (markSilenceInFlightRef.current === liveAudioSessionId) {
                        markSilenceInFlightRef.current = null
                      }
                    })
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
      if (liveAudioStartAbortRef.current === startAbort) {
        liveAudioStartAbortRef.current = null
      }
      micStartInFlightRef.current = false
    }
  }

  const toggleMic = (): void => {
    if (micActive) {
      stopLiveAudio()
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
    const hydrationRevision = ++snapshotControlRequestRevisionRef.current
    const snapshotPreferenceRevision = getAndroidPreferencePublicationRevision()
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
        hydrationRevision !== snapshotControlRequestRevisionRef.current ||
        snapshot.preferences.responseProfile !== profile
      ) {
        return false
      }
      return adoptSnapshotControls(snapshot, hydrationRevision, snapshotPreferenceRevision)
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

    const hadLiveAudio =
      micActive || micStarting || liveAudioSourceRef.current !== null
    if (hadLiveAudio) micAutoStartedRef.current = false
    stopLiveAudio()

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
      initialSilenceTimeoutMs: currentState.initialSilenceTimeoutMs,
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
        await refreshSnapshotControlsAfterProfileTransition(settled.selectedResponseProfile)
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
        const restoredStartup = session.fieldRef('startupState').current
        if (restoredStartup.kind === 'ready') await refreshSnapshotControlsAfterProfileTransition(restoredStartup.selectedResponseProfile)
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
      const write = session.preferences.tts.enqueue(() =>
        invokeTauriCommand('set_tts_enabled', { enabled }),
      )
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
      if (import.meta.env.VITE_VOXGOLEM_BENCHMARK === '1') reportAppBenchmarkStage('benchmark_controls_retire')
      micAutoStartedRef.current = false
      if (micActive || micStarting || micStartInFlightRef.current) stopLiveAudio()
      return
    }
    if (startupState.kind === 'ready' && !voiceInputReady(startupState)) {
      if (import.meta.env.VITE_VOXGOLEM_BENCHMARK === '1') reportAppBenchmarkStage('benchmark_capability_retire')
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
  }, [micActive, micStarting, resetPending, startupState, voiceSnapshotControlsUsable, updateRuntimeBlocked])

  const sendPrompt = async (): Promise<void> => {
    if (runtimeStatusRef.current !== 'sleeping' || androidVoiceActiveRef.current || isSwitchingResponseProfileRef.current || resetPendingRef.current) {
      return
    }

    if (composerValueRef.current.trim().length === 0) {
      return
    }

    if (isAndroidRuntime && snapshotSessionIdRef.current === '0' && shortcutDraftRef.current === null) shortcutDraftRef.current = { oldDraft: '', oldSession: '0', revision: composerEditRevisionRef.current, needsNew: true, prepare: null }
    const preparingShortcut = isAndroidRuntime && shortcutDraftRef.current?.needsNew === true
    if (preparingShortcut && shortcutDraftRef.current !== null) {
      try { await prepareShortcutConversation(shortcutDraftRef.current.oldDraft, () => appActiveRef.current) }
      catch (error) { addNotice({ tone: 'error', title: 'New voice draft was not sent', message: toDisplayErrorMessage(error) }); return }
    }
    await runPrompt(composerValueRef.current, 'typed')
  }

  const onSubmit = (event: FormEvent<HTMLFormElement>): void => {
    event.preventDefault()
    session.transaction(() => { void sendPrompt() })
  }

  const onComposerKeyDown = (
    event: KeyboardEvent<HTMLTextAreaElement>,
  ): void => {
    if (event.nativeEvent.isComposing) {
      return
    }
    if (!shouldSubmitComposer(event.key, event.shiftKey)) {
      return
    }

    event.preventDefault()
    void sendPrompt()
  }

  const acceptTypedCompletion = (suffix: string): void => {
    if (suffix.length === 0) return
    const nextValue = `${composerValueRef.current}${suffix}`
    composerEditRevisionRef.current += 1
    setComposerValue(nextValue)
    if (isAndroidRuntime && androidVoiceActiveRef.current) androidVoiceStopRef.current?.()
    persistAndroidDraft(nextValue)
    clearCompletionDisplay()
  }


  const resetSession = async (): Promise<void> => {
    const lifetime = session.captureLifetime()
    const appActiveRef = { get current() { return lifetime() } }
    if (resetPendingRef.current) return
    if (isAndroidRuntime) androidVoiceStopRef.current?.()
    if (shortcutDraftRef.current?.needsNew === true && composerValueRef.current.trim() !== '' && androidSelectionRecoveryRef.current === null) {
      try { await prepareShortcutConversation(shortcutDraftRef.current.oldDraft, () => appActiveRef.current) }
      catch (error) { addNotice({ tone: 'error', title: 'Voice draft preservation failed', message: toDisplayErrorMessage(error) }); return }
    }
    resetPendingRef.current = true
    const transcriptRevision = ++transcriptRevisionRef.current
    if (androidSelectionRecoveryRef.current !== null) androidSelectionRecoveryRef.current = { transcriptRevision, composerRevision: composerEditRevisionRef.current }
    setResetPending(true)
    if (liveAudioSourceRef.current !== null || micStarting || micStartInFlightRef.current) {
      micAutoStartedRef.current = false
    }
    stopLiveAudio()
    const resetRuntimeActivityRevision = runtimeActivityRevisionRef.current
    const activePrompt = promptRef.current
    let resetDraftRevision = composerEditRevisionRef.current
    cancelTts()
    clearPartialTranscript()
    clearCompletion()
    setResponseAnnouncement('')

    if (getTauriInternals() === null) {
      timingRef.current.retireRequest()
      if (activePrompt !== null) {
        cancelPromptProjectionFrame(activePrompt)
        activePrompt.terminal = true
      }
      promptRef.current = null
      session.retirePromptAdmission()
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
      cancelPromptProjectionFrame(activePrompt)
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
      if (isAndroidRuntime) await invokeTauriCommand('stop_assistant_voice')
      if (!appActiveRef.current) return
      await androidDraftWriteChainRef.current
      resetDraftRevision = composerEditRevisionRef.current
      if (androidSelectionRecoveryRef.current !== null) androidSelectionRecoveryRef.current = { transcriptRevision, composerRevision: resetDraftRevision }
      await invokeTauriCommand('reset_session', isAndroidRuntime ? { draft: composerValueRef.current } : undefined)
      if (appActiveRef.current) timingRef.current.retireRequest()
      if (!appActiveRef.current) return
      if (isAndroidRuntime && composerEditRevisionRef.current === resetDraftRevision) {
        androidDraftPendingRef.current = null
        androidDraftWriteFailureRef.current = null
      }
      shortcutDraftRef.current = null; setShortcutStaging(false)
      promptRef.current = null
      session.retirePromptAdmission()
      canonicalTranscriptRef.current = []
      setMessages((current) => replaceCommittedTranscript(current, []))
      setPromptActivity({ text: 'Recovering session state…', compact: true })
      setPromptState('reconciling')
      let resetSnapshot: AppSnapshot | null = null
      const snapshotControlRevision = ++snapshotControlRequestRevisionRef.current
      const snapshotPreferenceRevision = getAndroidPreferencePublicationRevision()
      for (let attempt = 0; attempt < 3 && resetSnapshot === null; attempt += 1) {
        if (!appActiveRef.current || transcriptRevision !== transcriptRevisionRef.current) return
        try {
          resetSnapshot = await loadAppSnapshot()
        } catch {
          if (attempt < 2) await new Promise((resolve) => window.setTimeout(resolve, 250 * (attempt + 1)))
        }
      }
      if (resetSnapshot === null) {
        setPromptActivity({ text: 'Session recovery required. Use New Chat to retry.', compact: false })
        setPromptState('reconciling')
        return
      }
      if (appActiveRef.current && transcriptRevision === transcriptRevisionRef.current) {
        snapshotHydrationStateRef.current = 'succeeded'
        setSnapshotHydrationState('succeeded')
        canonicalTranscriptRef.current = resetSnapshot.transcript
        snapshotSessionIdRef.current = resetSnapshot.sessionId
        adoptSnapshotControls(resetSnapshot, snapshotControlRevision, snapshotPreferenceRevision)
        adoptAndroidConversation(resetSnapshot, resetDraftRevision, false)
        if (resetSnapshot.activeRequest === null && resetSnapshot.assistant !== undefined) androidSelectionRecoveryRef.current = null
        setMessages((current) => replaceCommittedTranscript(current, resetSnapshot.transcript))
      }
      if (!appActiveRef.current) return
      setPromptActivity(null)
      setPromptState('idle')
      if (resetRuntimeActivityRevision === runtimeActivityRevisionRef.current) {
        applyRuntimeStatus(toRuntimeStatus(resetSnapshot?.runtimePhase ?? 'sleeping'))
      }
    } catch (error) {
      addNotice({ tone: 'error', title: 'New chat failed', message: toDisplayErrorMessage(error) })
      setPromptActivity({ text: 'Recovering session state…', compact: true })
      let snapshot: AppSnapshot | null = null
      let recoveryRuntimeActivityRevision: number | null = null
      const snapshotControlRevision = ++snapshotControlRequestRevisionRef.current
      const snapshotPreferenceRevision = getAndroidPreferencePublicationRevision()
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
        if (snapshot.sessionId !== snapshotSessionIdRef.current ||
          (isAndroidRuntime && snapshot.assistant !== undefined && snapshot.assistant.selectedHead !== androidAssistantRef.current?.selectedHead)) {
          timingRef.current.retireRequest()
        }
        promptRef.current = null
        session.retirePromptAdmission()
        snapshotHydrationStateRef.current = 'succeeded'
        setSnapshotHydrationState('succeeded')
        canonicalTranscriptRef.current = snapshot.transcript
        snapshotSessionIdRef.current = snapshot.sessionId
        adoptSnapshotControls(snapshot, snapshotControlRevision, snapshotPreferenceRevision)
        adoptAndroidConversation(snapshot, -1, false)
        if (snapshot.activeRequest === null && snapshot.assistant !== undefined) androidSelectionRecoveryRef.current = null
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
          setPromptActivity({ text: 'Recovering interrupted response…', compact: true })
          await recoverOrphanedRequest(snapshot, transcriptRevision, () => resetPendingRef.current)
        }
      } else if (appActiveRef.current && transcriptRevision === transcriptRevisionRef.current) {
        setPromptActivity({ text: 'Session recovery required. Use New Chat to retry.', compact: false })
        setPromptState('reconciling')
      }
    } finally {
      resetPendingRef.current = false
      setResetPending(false)
      if (androidDraftPendingRef.current !== null) persistAndroidDraft(composerValueRef.current)
    }
  }

  const selectAndroidConversation = async (command: 'select_conversation' | 'select_conversation_head', id: string): Promise<void> => {
    const lifetime = session.captureLifetime()
    const appActiveRef = { get current() { return lifetime() } }
    if (!isAndroidRuntime || shortcutDraftRef.current?.needsNew === true || resetPendingRef.current || androidSelectionRecoveryRef.current !== null || promptRef.current !== null || androidVoiceActive || headPagePending || !snapshotControlsFreshRef.current) return
    const composerRevision = composerEditRevisionRef.current
    resetPendingRef.current = true
    setResetPending(true)
    const transcriptRevision = ++transcriptRevisionRef.current
    const preferenceRevision = getAndroidPreferencePublicationRevision()
    const controlRevision = ++snapshotControlRequestRevisionRef.current
    try {
      await androidDraftWriteChainRef.current
      if (androidDraftWriteFailureRef.current !== null) throw androidDraftWriteFailureRef.current
      const result = await invokeTauriCommand(command, command === 'select_conversation' ? { conversationId: id } : { headId: id })
      const snapshot = parseAppSnapshot(result)
      if (!appActiveRef.current || transcriptRevision !== transcriptRevisionRef.current) return
      adoptSnapshotControls(snapshot, controlRevision, preferenceRevision)
      adoptAndroidConversation(snapshot, composerRevision, true)
    } catch (error) {
      addNotice({ tone: 'error', title: 'Conversation selection failed', message: toDisplayErrorMessage(error) })
      if (!appActiveRef.current || transcriptRevision !== transcriptRevisionRef.current) return
      const recovery = { transcriptRevision, composerRevision }
      androidSelectionRecoveryRef.current = recovery
      snapshotControlsFreshRef.current = false
      setSnapshotControlsFresh(false)
      snapshotControlsInvalidatedRef.current = true
      setSnapshotControlsInvalidated(true)
      setPromptState('reconciling')
      setPromptActivity({ text: 'Recovering selected conversation…', compact: true })
      let recovered = false
      for (let attempt = 0; attempt < 3 && !recovered; attempt += 1) {
        if (!appActiveRef.current || transcriptRevision !== transcriptRevisionRef.current || androidSelectionRecoveryRef.current !== recovery) return
        const recoveryControlRevision = ++snapshotControlRequestRevisionRef.current
        const recoveryPreferenceRevision = getAndroidPreferencePublicationRevision()
        try {
          const snapshot = await loadAppSnapshot()
          recovered = adoptRecoveredAndroidSelection(snapshot, recovery, recoveryControlRevision, recoveryPreferenceRevision)
        } catch {
          // The rejected command may already have committed selection; never infer its identity.
        }
        if (!recovered && attempt < 2) await new Promise((resolve) => window.setTimeout(resolve, 250 * (attempt + 1)))
      }
      if (!recovered && androidSelectionRecoveryRef.current === recovery) setPromptActivity({ text: 'Conversation selection recovery required. Resume the app or use New Chat to retry.', compact: false })
    } finally {
      resetPendingRef.current = false
      setResetPending(false)
      if (appActiveRef.current && transcriptRevision === transcriptRevisionRef.current && androidDraftPendingRef.current !== null) persistAndroidDraft(composerValueRef.current)
    }
  }

  function persistAndroidDraft(text: string): void {
    if (!isAndroidRuntime) return
    session.selection.persistDraft(text, {
      blocked: () => shortcutDraftRef.current?.needsNew === true || androidAssistantRef.current === null || promptRef.current !== null || resetPendingRef.current || androidSelectionRecoveryRef.current !== null,
      queuedBlocked: () => shortcutDraftRef.current?.needsNew === true || androidSelectionRecoveryRef.current !== null,
      save: async (value) => { await invokeTauriCommand('save_assistant_draft', { text: value }) },
      saved: () => { if (androidAssistantRef.current?.error !== null) refreshAndroidSnapshotControlsRef.current() },
      fail: (error) => { if (appActiveRef.current) addNotice({ tone: 'error', title: 'Draft was not saved', message: toDisplayErrorMessage(error) }) },
    })
  }

  useEffect(() => { persistAndroidDraftRef.current = persistAndroidDraft })

  async function beginCurrentVoiceDraft(completionPolicy: 'manual' | 'single_turn' | 'repeat_after_reply'): Promise<string> {
    const generationAvailable = (): boolean => {
      const startup = startupStateRef.current
      const selected = startup.kind === 'ready' ? applySnapshotModelOptions(
        instantOptions(capabilityMap(startup)), snapshotModelOptionsRef.current,
      ).find((option) => option.value === assistantSettingsRef.current.instant) : undefined
      return startup.kind === 'ready' && selected?.available === true &&
        instantMatchesResponseProfile(startup, assistantSettingsRef.current.instant) &&
        (androidAssistantRef.current?.branchChoices.length ?? 0) === 0 &&
        !['saved_local_state_failed', 'draft_not_saved', 'storage_error'].includes(androidAssistantRef.current?.error ?? '')
    }
    const captureAvailable = (): boolean => !['saved_local_state_failed', 'draft_not_saved', 'storage_error', 'corrupt_store'].includes(androidAssistantRef.current?.error ?? '') &&
      (androidAssistantRef.current?.branchChoices.length ?? 0) === 0 &&
      (completionPolicy === 'manual' || generationAvailable())
    if (!isAndroidRuntime || !captureAvailable() || updateRuntimeBlocked || resetPendingRef.current || promptRef.current !== null || androidVoiceActiveRef.current || androidSelectionRecoveryRef.current !== null || !snapshotControlsFreshRef.current || assistantSettingsPendingRef.current || shortcutDraftRef.current?.needsNew === true) throw new Error('Voice chat is busy; existing work was preserved')
    const conversation = snapshotSessionIdRef.current
    const selectedHead = androidAssistantRef.current?.selectedHead ?? null
    const revision = composerEditRevisionRef.current
    await androidDraftWriteChainRef.current
    if (androidDraftWriteFailureRef.current !== null) throw androidDraftWriteFailureRef.current
    if (!appActiveRef.current || !captureAvailable() || updateRuntimeBlocked || document.visibilityState !== 'visible' || resetPendingRef.current || promptRef.current !== null || androidSelectionRecoveryRef.current !== null || !snapshotControlsFreshRef.current || snapshotSessionIdRef.current !== conversation || (androidAssistantRef.current?.selectedHead ?? null) !== selectedHead || composerEditRevisionRef.current !== revision || assistantSettingsPendingRef.current) throw new Error('Voice chat was interrupted; existing work was preserved')
    if (settingsOpen) closeSettings()
    return composerValueRef.current
  }
  async function prepareShortcutConversation(oldDraft: string, isCurrent: () => boolean): Promise<void> {
    const shortcut = shortcutDraftRef.current
    if (shortcut === null || !shortcut.needsNew) return
    if (shortcut.prepare !== null) return shortcut.prepare
    shortcut.prepare = (async () => {
      if (!isCurrent() || resetPendingRef.current || promptRef.current !== null || androidSelectionRecoveryRef.current !== null) throw new Error('Voice draft interrupted; review before sending')
      resetPendingRef.current = true; setResetPending(true)
      const transcriptRevision = ++transcriptRevisionRef.current
      const controlRevision = ++snapshotControlRequestRevisionRef.current
      const preferenceRevision = getAndroidPreferencePublicationRevision()
      try {
        const snapshot = parseAppSnapshot(await invokeTauriCommand('reset_session', { draft: oldDraft }))
        if (!appActiveRef.current || !isCurrent() || shortcutDraftRef.current !== shortcut || transcriptRevision !== transcriptRevisionRef.current || controlRevision !== snapshotControlRequestRevisionRef.current) throw new Error('Voice draft interrupted; canonical recovery is required before sending')
        if (!isConfirmedNewVoiceContext(snapshot, shortcut.oldSession)) throw new Error('New voice chat context was not confirmed empty')
        if (!adoptSnapshotControls(snapshot, controlRevision, preferenceRevision)) throw new Error('New voice chat context became stale')
        shortcut.needsNew = false
        setShortcutStaging(false)
        adoptAndroidConversation(snapshot, -1, true)
      } catch (error) {
        if (appActiveRef.current && shortcutDraftRef.current === shortcut && transcriptRevision === transcriptRevisionRef.current) {
          androidSelectionRecoveryRef.current = { transcriptRevision, composerRevision: shortcut.revision }
          snapshotControlsFreshRef.current = false; setSnapshotControlsFresh(false)
          setPromptState('reconciling'); setPromptActivity({ text: 'New voice draft requires canonical recovery; resume the app or use New Chat.', compact: false })
        }
        throw error
      } finally {
        resetPendingRef.current = false; setResetPending(false)
        if (!shortcut.needsNew && androidDraftPendingRef.current !== null) persistAndroidDraft(composerValueRef.current)
      }
    })()
    return shortcut.prepare
  }

  const loadMoreAndroidConversations = async (): Promise<void> => {
    const expected = androidAssistantRef.current
    if (expected === null || conversationPagePending || resetPendingRef.current || promptRef.current !== null) return
    setConversationPagePending(true)
    try {
      const page = parseAndroidConversationPage(await invokeTauriCommand('list_assistant_conversations', { offset: expected.conversationOffset, expectedRevision: expected.revision }))
      const current = androidAssistantRef.current
      if (current === null || current.revision !== page.revision || page.nextOffset < current.conversationOffset) return
      const next = { ...current, conversations: Array.from(new Map([...current.conversations, ...page.conversations].map((item) => [item.id, item])).values()), conversationOffset: page.nextOffset }
      androidAssistantRef.current = next
      setAndroidAssistant(next)
    } catch (error) {
      addNotice({ tone: 'error', title: 'Conversation inventory unavailable', message: toDisplayErrorMessage(error) })
      refreshAndroidSnapshotControlsRef.current()
    } finally { setConversationPagePending(false) }
  }

  const loadMoreAndroidHeads = async (): Promise<void> => {
    const expected = androidAssistantRef.current
    const conversationId = snapshotSessionIdRef.current
    if (expected === null || conversationId === null || headPagePending || resetPendingRef.current || promptRef.current !== null || androidVoiceActive) return
    setHeadPagePending(true)
    try {
      const page = parseAndroidHeadPage(await invokeTauriCommand('list_assistant_heads', { offset: expected.branchOffset, expectedRevision: expected.revision, conversationId }))
      const current = androidAssistantRef.current
      if (current === null || current.revision !== page.revision || snapshotSessionIdRef.current !== conversationId || current.selectedHead !== expected.selectedHead || current.branchCount !== expected.branchCount || page.nextOffset < current.branchOffset) return
      const next = { ...current, branchChoices: Array.from(new Set([...current.branchChoices, ...page.heads])), branchOffset: page.nextOffset }
      androidAssistantRef.current = next
      setAndroidAssistant(next)
    } catch (error) {
      addNotice({ tone: 'error', title: 'Continuation inventory unavailable', message: toDisplayErrorMessage(error) })
      refreshAndroidSnapshotControlsRef.current()
    } finally { setHeadPagePending(false) }
  }

  const retryCustom = async (): Promise<void> => {
    if (customRetryPending) return
    setCustomRetryPending(true)
    try {
      await invokeTauriCommand('retry_custom_provider')
      applyStartupState(await loadStartupState(), false)
    } catch (error) {
      addNotice({ tone: 'error', title: 'Custom retry failed', message: toDisplayErrorMessage(error) })
      try {
        const latest = await loadStartupState()
        if (latest.kind === 'ready') applyStartupState(latest, false)
      } catch { /* Retain the persistent capability reason until the next startup poll. */ }
    } finally {
      setCustomRetryPending(false)
    }
  }

  const changeComposer = (event: ChangeEvent<HTMLTextAreaElement>): void => {
    const nextValue = event.target.value
    composerEditRevisionRef.current += 1
    setComposerValue(nextValue)
    if (isAndroidRuntime && androidVoiceActiveRef.current) androidVoiceStopRef.current?.()
    persistAndroidDraft(nextValue)
    setTypedCompletionSuffix('')
    setVoiceCompletionSuffix('')
    const revision = ++completionRevisionRef.current
    if (nextValue.length === 0) {
      latestTypedCompletionRef.current = null
      void invokeTauriCommand('clear_completion').catch(() => undefined)
    } else if (assistantSettingsRef.current.completion && !assistantSettingsPendingRef.current &&
      startupStateRef.current.kind === 'ready' && capabilityIsAvailable(startupStateRef.current, 'qwen_prediction')) {
      const pending = { revision, lifecycle: completionLifecycleRef.current }
      latestTypedCompletionRef.current = pending
      void invokeTauriCommand('request_completion', { revision, prompt: nextValue }).catch(() => {
        if (latestTypedCompletionRef.current === pending) latestTypedCompletionRef.current = null
      })
    } else latestTypedCompletionRef.current = null
  }
  const patchAssistantSettings = (patch: Partial<AssistantSettings>): void => {
    if (instantControlsDisabled || assistantSettingsPendingRef.current || resetPendingRef.current || promptRef.current !== null) return
    if (patch.prefetch === true && !supportsDesktopRuntime) return
    void persistAssistantSettings({ ...assistantSettingsRef.current, ...patch })
  }
  const voiceDraftChanged = (text: string): void => {
    composerEditRevisionRef.current += 1
    setComposerValue(text)
    if (shortcutDraftRef.current !== null) persistAndroidDraft(text)
  }
  const voiceSubmitted = (requestId: string, text: string, context?: AndroidVoiceContext): void => {
    pendingAndroidVoiceRequestRef.current = {
      requestId, text, sessionId: context?.sessionId ?? snapshotSessionIdRef.current,
      selectedHead: context === undefined ? androidAssistantRef.current?.selectedHead ?? null : context.selectedHead,
      composerRevision: composerEditRevisionRef.current, inputEpoch: androidVoiceInputEpochRef.current,
    }
    drainAndroidVoiceRequestRef.current()
  }
  const actions = useSessionIntents(session, {
    onAnswerDomCommitted: (id: string) => timingRef.current.domCommitted(id),
    onSubmit, onComposerKeyDown, acceptTypedCompletion, clearCompletion, changeComposer,
    patchAssistantSettings, voiceDraftChanged, voiceSubmitted,
    voiceSettled: () => refreshAndroidSnapshotControlsRef.current(),
    refreshChatGptAuth: async () => {
      if (isAndroidRuntime) refreshAndroidSnapshotControlsRef.current()
      else applyStartupState(await loadStartupState(), false)
    },
    beginCurrentVoiceDraft, onAndroidVoiceActivity, onAndroidSpeechStatus, onAndroidVoiceInputCancelled,
    changeInstant, resetSession, retryCustom, cancelPrompt,
    changeUiTheme, adjustUiTextSize, toggleTts, toggleMic, changeAudioInputDevice,
    setAutoStopOnSilence, setVoiceNotesOpen, setNoticeInteraction,
    loadMoreAndroidConversations, loadMoreAndroidHeads, selectAndroidConversation,
    canRetry: (value: AssistantSettings['instant']) => localInstantCanRetry(startupStateRef.current, value),
  })
  const voice = useAndroidVoiceAdapter({
    enabled: isAndroidRuntime && !voiceNotesOpen,
    statusEnabled: isAndroidRuntime && (!voiceNotesOpen || settingsOpen),
    draft: composerValue,
    disabled: resetPending || promptState !== 'idle' || !snapshotControlsFresh || assistantSettingsPending,
    generationAvailable: !nativeConversationNeedsRecovery && selectedInstantOption?.available === true && (androidAssistant?.branchChoices.length ?? 0) === 0,
    shortcutReady: snapshotHydrationState === 'succeeded' && androidAssistant !== null && snapshotControlsFresh && !snapshotControlsInvalidated,
    launchContext: { sessionId: snapshotSessionIdRef.current ?? '0', selectedHead: androidAssistant?.selectedHead ?? null },
    stopRef: androidVoiceStopRef,
    onBeforeStart: actions.beginCurrentVoiceDraft,
    onActivity: actions.onAndroidVoiceActivity,
    onStatus: actions.onAndroidSpeechStatus,
    onInputCancelled: actions.onAndroidVoiceInputCancelled,
    onDraft: actions.voiceDraftChanged,
    onSubmit: actions.voiceSubmitted,
    onSettled: actions.voiceSettled,
  })
  const voiceIntents = useSessionIntents(session, {
    start: voice.start, stop: voice.stop, finishDictation: voice.finishDictation,
    finishShortcut: voice.finishShortcut, toggleSpeech: voice.toggleSpeech,
  }, 'voiceIntents')

  const startupTelemetrySentRef = session.operationRef('startupTelemetrySent', () => false)
  useEffect(() => {
    if (startupTelemetrySentRef.current) return
    const readyToSend =
      startupState.kind === 'ready' &&
      snapshotControlsFresh &&
      !assistantSettingsPending &&
      !resetPending &&
      !isSwitchingResponseProfile &&
      !updateRuntimeBlocked &&
      !androidVoiceActive &&
      !nativeConversationNeedsRecovery &&
      (androidAssistant?.branchChoices.length ?? 0) === 0 &&
      shortcutDraftRef.current?.needsNew !== true &&
      androidSelectionRecoveryRef.current === null &&
      promptState === 'idle' &&
      runtimeStatus === 'sleeping' &&
      selectedInstantOption?.available === true &&
      instantMatchesResponseProfile(startupState, assistantSettings.instant)
    if (readyToSend) {
      startupTelemetrySentRef.current = true
      const elapsed = Math.round(performance.now())
      timingRef.current.startup('STATE_READY')
      document.documentElement.dataset['appReadyToSendMs'] = String(elapsed)
      ;(window as unknown as { __appReadyToSendMs?: number }).__appReadyToSendMs = elapsed
    }
  }, [androidAssistant?.branchChoices.length, androidSelectionRecoveryRef, androidVoiceActive, assistantSettings.instant, assistantSettingsPending, isSwitchingResponseProfile, nativeConversationNeedsRecovery, promptState, resetPending, runtimeStatus, selectedInstantOption?.available, shortcutDraftRef, snapshotControlsFresh, startupState, startupTelemetrySentRef, timingRef, updateRuntimeBlocked])

  return {
    actions,
    voice: { ...voice, ...voiceIntents },
    appUpdates,
    navigation: { settingsOpen, settingsButtonRef, settingsPanelRef, settingsCloseButtonRef, openSettings, closeSettings, onSettingsKeyDown },
  }
}

export function FrontendSessionRuntime({ session }: { readonly session: FrontendSession }) {
  const view = useFrontendSessionRuntime(sessionAuthority(session))
  return <SessionViews session={session} view={view} />
}

export type FrontendSessionIntents = ReturnType<typeof useFrontendSessionRuntime>['actions']

type SessionViewProps = { readonly session: FrontendSession; readonly view: ReturnType<typeof useFrontendSessionRuntime> }

// Native/update/navigation adapters may change independently of session state.
// A state publication is consumed by the named boundary, not by all siblings.
function sameExternalView(previous: SessionViewProps, next: SessionViewProps): boolean {
  const sameValues = (left: object, right: object) => Object.keys(left).every((key) =>
    Object.is(Reflect.get(left, key), Reflect.get(right, key)))
  return previous.session === next.session && previous.view.actions === next.view.actions &&
    sameValues(previous.view.navigation, next.view.navigation) &&
    sameValues(previous.view.appUpdates, next.view.appUpdates) &&
    Object.entries(previous.view.voice).every(([key, value]) =>
      typeof value === 'function' || Object.is(value, Reflect.get(next.view.voice, key)))
}

const SessionViews = memo(function SessionViews({ session, view }: SessionViewProps) {
  const { uiTextSize, uiTheme, runtimeStatus } = useSessionView(session, 'shell')
  return <div className="shell" data-platform={isAndroidRuntime ? 'android' : 'desktop'} data-ui-text-size={uiTextSize} data-ui-theme={uiTheme} data-runtime-status={runtimeStatus}>
    <SessionVoiceControls session={session} view={view}>
    <SessionToolbar session={session} view={view} />
    <SessionConversation session={session} view={view} />
    <SessionSettings session={session} view={view} />
    <SessionComposer session={session} view={view} />
    </SessionVoiceControls>
  </div>
}, sameExternalView)

function SessionVoiceControls({ session, view, children }: SessionViewProps & { readonly children: ReactNode }) {
  const { resetPending, promptState, snapshotControlsFresh, assistantSettingsPending,
    nativeConversationNeedsRecovery, selectedInstantOption, androidAssistant, updateRuntimeBlocked } = usePresentation(session, view, 'toolbar')
  if (!isAndroidRuntime) return children
  return <AndroidVoiceControlsProvider voice={view.voice}
    disabled={updateRuntimeBlocked || nativeConversationNeedsRecovery || resetPending || promptState !== 'idle' || !snapshotControlsFresh || assistantSettingsPending || (androidAssistant?.branchChoices.length ?? 0) > 0}
    generationAvailable={!nativeConversationNeedsRecovery && selectedInstantOption?.available === true && (androidAssistant?.branchChoices.length ?? 0) === 0}
    interactionBlocked={view.navigation.settingsOpen}>{children}</AndroidVoiceControlsProvider>
}

function usePresentation(session: FrontendSession, view: SessionViewProps['view'], name: Parameters<typeof session.getViewSnapshot>[0]) {
  useSessionView(session, name)
  const state = session.getControlsSnapshot()
  const { startupState, assistantSettings, runtimeStatus, promptState, resetPending,
    assistantSettingsPending, androidVoiceActive, isSwitchingResponseProfile, snapshotControlsFresh,
    micActive, micStarting } = state
  const options = session.getModelsSnapshot()
  const selectedInstantOption = options.instantAssistantOptions.find((option) => option.value === assistantSettings.instant)
  const selectedInstantProfileReady = instantMatchesResponseProfile(startupState, assistantSettings.instant)
  const nativeConversationNeedsRecovery = ['saved_local_state_failed', 'draft_not_saved', 'storage_error'].includes(state.androidAssistant?.error ?? '')
  const updateRuntimeBlocked = appUpdateBlocksRuntime(view.appUpdates.state)
  const assistantControlsDisabled = startupState.kind !== 'ready' || assistantSettingsPending || isSwitchingResponseProfile || !snapshotControlsFresh || updateRuntimeBlocked || androidVoiceActive
  const instantControlsDisabled = assistantControlsDisabled || resetPending || promptState !== 'idle' || runtimeStatus === 'processing' || runtimeStatus === 'executing'
  const voiceControlsUsable = snapshotControlsFresh || (!state.snapshotControlsInvalidated && state.snapshotHydrationState === 'pending')
  const sizeIndex = Math.max(0, UI_TEXT_SIZE_STEPS.indexOf(state.uiTextSize))
  return {
    ...state, ...options, appUpdates: view.appUpdates, supportsDesktopRuntime: !isAndroidRuntime && !isBrowserPreviewRuntime(),
    updateRuntimeBlocked, selectedInstantOption, selectedInstantProfileReady, nativeConversationNeedsRecovery,
    selectedInstantRetryable: localInstantCanRetry(startupState, assistantSettings.instant),
    assistantControlsDisabled, instantControlsDisabled,
    canSend: generationIneligibility(state, selectedInstantOption?.available === true, updateRuntimeBlocked) === null &&
      !androidVoiceActive && promptState === 'idle' && runtimeStatus === 'sleeping' && state.composerValue.trim().length > 0,
    canToggleMic: !updateRuntimeBlocked && (micActive || (voiceControlsUsable && voiceInputReady(startupState) && !micStarting && !resetPending)),
    canToggleTts: startupState.kind === 'ready' && capabilityIsAvailable(startupState, 'tts') && !isSwitchingResponseProfile && !updateRuntimeBlocked,
    voiceInputUnavailableReason: startupState.kind === 'ready' ? startupState.voiceInputError ?? (startupState.capabilities.filter((capability) =>
      ['wake_word', 'vad', 'parakeet'].includes(capability.id) && capability.state !== 'available').map((capability) => capability.reason ?? `${capabilityLabel(capability.id)} unavailable`).join('; ') || 'required voice capabilities are unavailable') : 'voice input is still starting',
    updateInstallationDisabled: startupState.kind !== 'ready' || runtimeStatus !== 'sleeping' || promptState !== 'idle' || resetPending ||
      isSwitchingResponseProfile || assistantSettingsPending || micStarting || state.ttsPlaying || state.pendingTtsCommands > 0,
    selectedAudioInputDevice: state.audioInputDevices.find((device) => device.deviceId === state.audioInputDeviceId),
    canDecreaseTextSize: sizeIndex > 0, canIncreaseTextSize: sizeIndex < UI_TEXT_SIZE_STEPS.length - 1,
    customFailure: startupState.kind === 'ready' || startupState.kind === 'warming_model' ? startupState.capabilities.find((capability) =>
      capability.id === 'custom_provider' && (capability.state === 'failed' || capability.state === 'unavailable')) : undefined,
    canEnableDeep: options.deepAssistantOptions.some((option) => option.value === assistantSettings.deep && option.available),
    canEnableReview: options.reviewAssistantOptions.some((option) => option.value === assistantSettings.review && option.available),
    headerBusy: androidVoiceActive || ['listening', 'processing', 'executing'].includes(runtimeStatus) || promptState !== 'idle' || resetPending || isSwitchingResponseProfile || startupState.kind === 'warming_model',
  }
}

const SessionToolbar = memo(function SessionToolbar({ session, view }: SessionViewProps) {
  const {
    updateRuntimeBlocked, shortcutStaging, androidAssistant, androidConversationId,
    headPagePending, androidVoiceActive, voiceNotesOpen,
    snapshotControlsFresh,
    promptState, resetPending,
    headerBusy,
    supportsDesktopRuntime, ttsPlaying, ttsEnabled, canToggleTts,
  } = usePresentation(session, view, 'toolbar')
  const { settingsOpen, settingsButtonRef, openSettings } = view.navigation
  const {
    resetSession,
    setVoiceNotesOpen, selectAndroidConversation, toggleTts,
  } = view.actions
  const notesControl = <button type="button" className="shell__control shell__icon-control" aria-label={voiceNotesOpen ? 'Chat' : 'Voice notes'} title={voiceNotesOpen ? 'Chat' : 'Voice notes'} aria-pressed={voiceNotesOpen} onClick={() => setVoiceNotesOpen((open) => !open)}>
    <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path d="M5 3h10l4 4v14H5z M15 3v5h4 M8 12h8 M8 16h6" /></svg>
    <span className="shell__overflow-label" aria-hidden="true">{voiceNotesOpen ? 'Chat' : 'Voice notes'}</span>
  </button>
  const conversationControl = androidAssistant !== null ? <label className="shell__control shell__icon-control shell__conversation-picker" title={androidAssistant.conversations.find((item) => item.id === androidConversationId)?.title ?? 'Conversation'}>
    <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path d="M4 4h16v12H9l-5 4z M8 8h8 M8 12h6" /></svg>
    <select className="shell__conversation-select" aria-label="Conversation" value={androidConversationId} disabled={shortcutStaging || resetPending || promptState !== 'idle' || androidVoiceActive || headPagePending || !snapshotControlsFresh} onChange={(event) => void selectAndroidConversation('select_conversation', event.target.value)}>
      <option value="0" disabled>New draft</option>
      {androidAssistant.conversations.map((conversation) => <option key={conversation.id} value={conversation.id}>{conversation.title}</option>)}
    </select>
  </label> : null
  const newChatControl = <button type="button" className="shell__control shell__new-chat shell__icon-control" onClick={() => void resetSession()} aria-label="New Chat" aria-busy={resetPending} title={resetPending ? 'Starting new chat…' : 'New Chat'} disabled={resetPending || updateRuntimeBlocked}>
    <ControlIcon kind="plus" />
    <span className="shell__overflow-label" aria-hidden="true">New Chat</span>
  </button>
  const settingsControl = <button ref={settingsButtonRef} type="button" className={`shell__control shell__settings-button${isAndroidRuntime ? ' shell__icon-control' : ''}`} onClick={openSettings} aria-label="Settings" aria-haspopup="dialog" aria-controls={SETTINGS_DIALOG_ID} aria-expanded={settingsOpen} title="Settings">
    {isAndroidRuntime ? <ControlIcon kind="settings" /> : '⚙'}
    <span className="shell__overflow-label" aria-hidden="true">Settings</span>
  </button>
  const desktopSpeakerState: import('./components/ControlIcon').ControlIconState = ttsPlaying ? 'speaking' : ttsEnabled ? 'ready' : canToggleTts ? 'off' : 'unavailable'
  const desktopSpeakerDescription = ttsPlaying ? 'Speaking…' : ttsEnabled ? 'Speak replies enabled' : canToggleTts ? 'Speak replies off' : 'Speech output is unavailable'
  const desktopSpeakerControl = supportsDesktopRuntime ? (
    <button
      type="button"
      className="shell__control shell__icon-control"
      aria-label="Speak replies"
      title={`Speak replies: ${desktopSpeakerDescription}`}
      aria-pressed={ttsEnabled}
      disabled={!canToggleTts && !ttsEnabled}
      onClick={() => void toggleTts(!ttsEnabled)}
    >
      <ControlIcon kind="speaker" state={desktopSpeakerState} />
      <span className="shell__overflow-label" aria-hidden="true">Speak replies</span>
    </button>
  ) : null

  return (
      <header className="shell__header" aria-label="VoxGolem">
        <div className="shell__identity" data-shell-identity>
          <img className="shell__logo" src={headerIcon} alt="VoxGolem" width={32} height={32} data-busy={headerBusy} />
          <div className="shell__runtime">
            {isBrowserPreviewRuntime() ? <span data-browser-preview>Browser preview</span> : null}
          </div>
        </div>
        <div className="shell__header-actions" role={isAndroidRuntime ? 'group' : undefined} aria-label={isAndroidRuntime ? 'Android toolbar' : undefined} aria-describedby={isAndroidRuntime && shortcutStaging ? 'new-voice-draft-status' : undefined}>
          {isAndroidRuntime ? <>{voiceNotesOpen ? notesControl : null}{conversationControl}<PhoneActionOverflow>
            {!voiceNotesOpen ? <AndroidVoiceSecondaryControls /> : null}
            {!voiceNotesOpen ? notesControl : newChatControl}{settingsControl}
          </PhoneActionOverflow></> : null}
          {!isAndroidRuntime ? <>{desktopSpeakerControl}{settingsControl}</> : null}
        </div>
      </header>
  )
}, sameExternalView)

const SessionConversation = memo(function SessionConversation({ session, view }: SessionViewProps) {
  const { voiceNotesOpen,
    shortcutStaging, composerValue, uiTextSize, notices } = usePresentation(session, view, 'conversation')
  const { setVoiceNotesOpen, setNoticeInteraction } = view.actions
  return voiceNotesOpen ? <CaptureHistory onBack={() => setVoiceNotesOpen(false)} /> : <>
      <div className="conversation-region">
        <SessionTranscript session={session} staging={shortcutStaging} composerValue={composerValue} textSize={uiTextSize} noticeId={notices[0]?.id} />

        <div
          className="conversation-notice"
          onFocus={() => setNoticeInteraction((current) => ({ ...current, focused: true }))}
          onBlur={(event) => {
            if (!event.currentTarget.contains(event.relatedTarget)) {
              setNoticeInteraction((current) => ({ ...current, focused: false }))
            }
          }}
          onMouseEnter={() => setNoticeInteraction((current) => ({ ...current, hovered: true }))}
          onMouseLeave={() => setNoticeInteraction((current) => ({ ...current, hovered: false }))}
        >
          <UserNoticeToast notice={notices[0] ?? null} />
        </div>
      </div>

      <ResponseAnnouncements session={session} />
      </>
}, sameExternalView)

const SessionSettings = memo(function SessionSettings({ session, view }: SessionViewProps) {
  const { uiTheme, assistantSettingsWriteError, uiThemeWriteError, uiTextSize, canDecreaseTextSize, canIncreaseTextSize,
    supportsDesktopRuntime, selectedAudioInputDevice, audioInputDeviceId, micStarting, updateRuntimeBlocked, audioInputDevices,
    autoStopOnSilence, startupState, voiceInputUnavailableReason, ttsEnabled, canToggleTts, androidVoiceStatus,
    androidSpeechStatusError, assistantSettings, instantControlsDisabled, instantAssistantOptions, assistantCapabilities,
    selectedInstantRetryable, selectedInstantOption, assistantControlsDisabled, deepAssistantOptions, reviewAssistantOptions,
    canEnableDeep, canEnableReview, appUpdates, updateInstallationDisabled, customFailure, customRetryPending } = usePresentation(session, view, 'settings')
  const { settingsOpen, settingsPanelRef, settingsCloseButtonRef, closeSettings, onSettingsKeyDown } = view.navigation
  const { changeUiTheme, adjustUiTextSize, changeAudioInputDevice, setAutoStopOnSilence, toggleTts, patchAssistantSettings,
    changeInstant, clearCompletion, retryCustom, refreshChatGptAuth } = view.actions
  return settingsOpen ? (
        <div
          className="settings-overlay"
          onMouseDown={() => closeSettings()}
          onKeyDown={onSettingsKeyDown}
        >
          <section
            ref={settingsPanelRef}
            className="settings-panel"
            role="dialog"
            id={SETTINGS_DIALOG_ID}
            aria-modal="true"
            aria-labelledby="settings-title"
            onMouseDown={(event) => event.stopPropagation()}
          >
            <div className="settings-panel__header">
              <div>
                <h2 id="settings-title">Settings</h2>
              </div>
              <button
                ref={settingsCloseButtonRef}
                type="button"
                className="settings-panel__close"
                onClick={() => closeSettings()}
                aria-label="Close settings"
              >
                ×
              </button>
              {assistantSettingsWriteError !== null || uiThemeWriteError !== null ? (
                <div className="settings-panel__write-error" role="alert" tabIndex={0}>
                  {assistantSettingsWriteError !== null ? <p className="shell__error">Assistant settings could not be saved: {assistantSettingsWriteError}</p> : null}
                  {uiThemeWriteError !== null ? <p className="shell__error">Theme could not be saved: {uiThemeWriteError}</p> : null}
                </div>
              ) : null}
            </div>
            <div className="settings-panel__row">
              <label htmlFor="uiTheme"><strong>Theme</strong></label>
              <select id="uiTheme" className="shell__control settings-panel__interactive" aria-label="Theme" value={uiTheme} onChange={(event) => changeUiTheme(parseUiTheme(event.target.value))}>
                <option value="dark">OLED Night</option><option value="slate">Slate</option><option value="light">Light</option>
              </select>
            </div>
            <div className="settings-panel__row">
              <div>
                <strong>Text size</strong>
                <p className="settings-panel__hint">Applies to chat, composer, and controls.</p>
              </div>
              <div className="settings-panel__stepper" aria-label="Text size controls">
                <button
                  type="button"
                  className="settings-panel__step-button settings-panel__interactive"
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
                  className="settings-panel__step-button settings-panel__interactive"
                  onClick={() => adjustUiTextSize(1)}
                  disabled={!canIncreaseTextSize}
                  aria-label="Increase text size"
                >
                  +
                </button>
              </div>
            </div>
            <section className="settings-panel__assistant" aria-labelledby="model-settings-title">
              <h3 id="model-settings-title">Model</h3>
              <ChatGptAuth onChanged={refreshChatGptAuth} />
              <label className="settings-panel__interactive"><strong>Instant</strong><InstantModelSelect id="assistantInstantSelect" label="Instant" value={assistantSettings.instant} options={instantAssistantOptions} disabled={instantControlsDisabled} canRetry={(value) => localInstantCanRetry(startupState, value)} onChange={(value) => void changeInstant(value)} /></label>
              {startupState.kind === 'ready' && !assistantCapabilities.custom ? <div>
                <p className="settings-panel__hint">Custom connection: {customFailure?.reason ?? 'unavailable'}</p>
                {customFailure && supportsDesktopRuntime ? <button type="button" className="shell__control" aria-label="Retry Custom" disabled={customRetryPending || updateRuntimeBlocked} onClick={() => void retryCustom()}>{customRetryPending ? 'Retrying…' : 'Retry Custom'}</button> : null}
              </div> : null}
              {selectedInstantRetryable && selectedInstantOption ? <button className="settings-panel__interactive" type="button" aria-label="Retry local profile" disabled={instantControlsDisabled} onClick={() => void changeInstant(assistantSettings.instant)}>Retry local profile</button> : null}
              {!isAndroidRuntime ? <details className="settings-panel__advanced">
                <summary>Advanced model settings</summary>
                <label className="settings-panel__interactive"><strong>Deep model</strong><select aria-label="Deep model" value={assistantSettings.deep} disabled={assistantControlsDisabled} onChange={(event) => {
                  const value = event.target.value as AssistantSettings['deep']
                  if (deepAssistantOptions.find((option) => option.value === value)?.available) patchAssistantSettings({ deep: value })
                }}>
                  {deepAssistantOptions.map((option) => <option key={option.value} value={option.value} disabled={!option.available} title={option.reason}>{option.label}</option>)}
                </select></label>
                <label className="settings-panel__interactive"><strong>Review model</strong><select aria-label="Review model" value={assistantSettings.review} disabled={assistantControlsDisabled} onChange={(event) => {
                  const value = event.target.value as AssistantSettings['review']
                  if (reviewAssistantOptions.find((option) => option.value === value)?.available) patchAssistantSettings({ review: value })
                }}>
                  {reviewAssistantOptions.map((option) => <option key={option.value} value={option.value} disabled={!option.available} title={option.reason}>{option.label}</option>)}
                </select></label>
                <label className="settings-panel__interactive"><input type="checkbox" checked={assistantSettings.deepEnabled} disabled={assistantControlsDisabled || (!assistantSettings.deepEnabled && !canEnableDeep)} onChange={(event) => patchAssistantSettings({ deepEnabled: event.target.checked })} /> Deep</label>
                <label className="settings-panel__interactive"><input type="checkbox" checked={assistantSettings.reviewEnabled} disabled={assistantControlsDisabled || (!assistantSettings.reviewEnabled && !canEnableReview)} onChange={(event) => patchAssistantSettings({ reviewEnabled: event.target.checked })} /> Review</label>
                <label className="settings-panel__interactive"><input type="checkbox" checked={assistantSettings.completion} disabled={assistantControlsDisabled || (!assistantSettings.completion && !assistantCapabilities.qwenPrediction)} onChange={(event) => {
                  if (!event.target.checked) clearCompletion()
                  patchAssistantSettings({ completion: event.target.checked })
                }} /> Completion</label>
                <label className="settings-panel__interactive"><input type="checkbox" checked={assistantSettings.prefetch} disabled={assistantControlsDisabled || !supportsDesktopRuntime} aria-describedby="prefetch-help" onChange={(event) => patchAssistantSettings({ prefetch: event.target.checked })} /> Prefetch</label>
                <p id="prefetch-help" className="settings-panel__hint">{supportsDesktopRuntime ? 'Prefetch may transmit unaccepted predicted text when enabled.' : 'Prefetch is unavailable on this runtime.'}</p>
              </details> : null}
            </section>
            {supportsDesktopRuntime ? <div className="settings-panel__row settings-panel__mic-row">
              <div>
                <label htmlFor="audioInputDevice"><strong>Microphone</strong></label>
                <p className="settings-panel__hint">Choose the input used for wake-word listening.</p>
              </div>
               <select
                 className="settings-panel__interactive"
                id="audioInputDevice"
                aria-label="Microphone"
                title={selectedAudioInputDevice?.label ?? ''}
                value={audioInputDeviceId ?? ''}
                disabled={micStarting || updateRuntimeBlocked}
                onChange={(event) => changeAudioInputDevice(event.target.value)}
              >
                <option value="">System selected microphone</option>
               {audioInputDevices.map((device) => (
                  <option key={device.deviceId} value={device.deviceId}>{device.label}</option>
                ))}
              </select>
              {selectedAudioInputDevice && (
                <p className="settings-panel__hint settings-panel__mic-name">{selectedAudioInputDevice.label}</p>
              )}
           </div> : null}
            {supportsDesktopRuntime ? <div className="settings-panel__row">
              <div>
                <strong>Voice input</strong>
                <p className="settings-panel__hint">Stop listening automatically after silence.</p>
              </div>
               <label className="shell__toggle settings-panel__interactive">
                <input
                  type="checkbox"
                  checked={autoStopOnSilence}
                  onChange={(event) => setAutoStopOnSilence(event.target.checked)}
                  disabled={!voiceInputReady(startupState) || updateRuntimeBlocked}
                  aria-describedby={!voiceInputReady(startupState) ? 'voice-input-help' : undefined}
                />
                <span>Auto Stop</span>
              </label>
              {!voiceInputReady(startupState) ? <span id="voice-input-help" className="sr-only">{voiceInputUnavailableReason}</span> : null}
            </div> : null}
            {supportsDesktopRuntime || isAndroidRuntime ? <div className="settings-panel__row">
              <div>
                <strong>Speech output</strong>
                <p className="settings-panel__hint">Read assistant responses aloud.</p>
              </div>
               <label className="shell__toggle settings-panel__interactive">
                <input
                  id="tts-toggle"
                  type="checkbox"
                  checked={isAndroidRuntime ? (androidVoiceStatus?.ttsEnabled ?? ttsEnabled) : ttsEnabled}
                  disabled={isAndroidRuntime ? (view.voice.pending || (androidVoiceStatus?.ttsEnabled !== true && (instantControlsDisabled || androidVoiceStatus?.ttsAvailable !== true))) : !canToggleTts}
                  onChange={isAndroidRuntime ? () => view.voice.toggleSpeech() : (event) => { void toggleTts(event.target.checked) }}
                />
                <span>TTS</span>
              </label>
            </div> : null}
            {isAndroidRuntime || supportsDesktopRuntime ? <SpeechSettings android={isAndroidRuntime} androidStatus={androidVoiceStatus} statusError={androidSpeechStatusError} startupState={startupState}
              speechNetworkFallback={assistantSettings.speechNetworkFallback} speechSelection={assistantSettings} disabled={instantControlsDisabled}
              onSpeechSelectionChange={patchAssistantSettings}
              onSpeechNetworkFallbackChange={(enabled) => patchAssistantSettings({ speechNetworkFallback: enabled })} /> : null}
            <section className="settings-panel__web" aria-labelledby="web-settings-title">
              <h3 id="web-settings-title">Web access</h3>
              <label className="shell__toggle settings-panel__interactive"><input type="checkbox" aria-label="Allow web access" checked={assistantSettings.webEnabled} disabled={instantControlsDisabled}
                onChange={(event) => patchAssistantSettings({ webEnabled: event.target.checked })} /> Web research</label>
              <p className="settings-panel__hint">{assistantSettings.webEnabled ? 'Native search may send question-derived queries and public-page requests to external providers: Exa is primary, with Parallel as fallback when needed. Delegated OpenCode routes may use their own search providers.' : 'Web is off: no web lookup or search fallback. Normal chat still uses the selected chat provider; answers are not web-verified.'}</p>
              <p className="settings-panel__hint">The selected model decides whether to use available web tools. Models without tool support can still chat but cannot browse automatically. Search activity appears in the response status when reported.</p>
            </section>
            {isAndroidRuntime ? <NewVoiceShortcutSettings /> : null}
            <UpdateSettings updates={appUpdates} installationDisabled={updateInstallationDisabled} />
            {!isAndroidRuntime && !isBrowserPreviewRuntime() ? <>
              <WakeDiagnostics disabled={!voiceInputReady(startupState) || updateRuntimeBlocked} />
              <WakeClipSettings />
            </> : null}
          </section>
        </div>
      ) : null
}, sameExternalView)

const SessionComposer = memo(function SessionComposer({ session, view }: SessionViewProps) {
  const { voiceNotesOpen, shortcutStaging, androidAssistant, conversationPagePending, resetPending, promptState,
    androidVoiceActive, headPagePending, snapshotControlsFresh, composerValue, uiTextSize, partialTranscript,
    typedCompletionSuffix, voiceCompletionSuffix, canSend, startupState, assistantSettingsLoadError, selectedInstantOption,
    selectedInstantProfileReady, canToggleMic, micStarting, micActive, assistantSettings, instantAssistantOptions,
    instantControlsDisabled, runtimeStatus, updateRuntimeBlocked,
    promptActivity, wakeConfidence, customFailure, supportsDesktopRuntime, customRetryPending } = usePresentation(session, view, 'composer')
  const { onSubmit, loadMoreAndroidConversations, loadMoreAndroidHeads, selectAndroidConversation, changeComposer,
     onComposerKeyDown, acceptTypedCompletion, clearCompletion, toggleMic, changeInstant, cancelPrompt, resetSession, retryCustom } = view.actions
  const canStop = (promptState === 'executing' || promptState === 'stopping') && startupState.kind === 'ready' && startupState.promptCancellationAvailable
  const busy = promptState !== 'idle' || runtimeStatus === 'processing' || runtimeStatus === 'executing'
  const actionLabel = canStop ? promptState === 'stopping' ? 'Stopping…' : 'Stop' : busy ? 'Working…' : 'Send'
  const selectedUnavailable = startupState.kind === 'ready' && (selectedInstantOption?.available !== true || !selectedInstantProfileReady)
  let unavailableReason = selectedInstantOption?.reason ?? 'selected provider is unavailable'
  if (selectedInstantOption?.available === true && !selectedInstantProfileReady) unavailableReason = 'selected local model profile is not loaded'
  if (unavailableReason === 'response_backend is not configured') unavailableReason = 'selected provider is not configured'
  const showAvailability = startupState.kind === 'error' || startupState.kind === 'warming_model' || assistantSettingsLoadError !== null || selectedUnavailable
  return !voiceNotesOpen ? <form className="composer" onSubmit={onSubmit}>
        {shortcutStaging ? <p id="new-voice-draft-status" className={isAndroidRuntime ? 'sr-only' : undefined} role="status">New voice draft — Stop &amp; send starts a new conversation. Previous history and draft are preserved.</p> : null}
        {isAndroidRuntime && androidAssistant !== null ? <div className="composer__actions">
          {androidAssistant.conversationOffset < androidAssistant.conversationCount ? <button type="button" disabled={conversationPagePending || resetPending || promptState !== 'idle' || androidVoiceActive} onClick={() => void loadMoreAndroidConversations()}>{conversationPagePending ? 'Loading conversations…' : 'More conversations'}</button> : null}
          {androidAssistant.branchChoices.length > 0 ? <label>Choose continuation <select aria-label="Choose continuation" value="" disabled={shortcutStaging || resetPending || promptState !== 'idle' || androidVoiceActive || headPagePending || !snapshotControlsFresh} onChange={(event) => void selectAndroidConversation('select_conversation_head', event.target.value)}>
            <option value="" disabled>Choose continuation</option>
            {androidAssistant.branchChoices.map((head, index) => <option key={head} value={head}>Continuation {index + 1}</option>)}
          </select></label> : null}
          {androidAssistant.branchOffset < androidAssistant.branchCount ? <button type="button" disabled={headPagePending || resetPending || promptState !== 'idle' || androidVoiceActive} onClick={() => void loadMoreAndroidHeads()}>{headPagePending ? 'Loading continuations…' : 'More continuations'}</button> : null}
          {androidAssistant.error !== null && androidAssistant.error !== 'interrupted' ? <span role="status">Native conversation status: {androidAssistant.error.replaceAll('_', ' ')}</span> : null}
        </div> : null}
        <div className="composer__surface">
        <div className="composer__prompt-row">
        <PromptComposer
          id="promptComposer"
          className="composer__input"
          aria-label="Prompt"
           value={composerValue}
           typographyRevision={uiTextSize}
          partialTranscript={partialTranscript}
          ghostSuffix={typedCompletionSuffix}
           onChange={changeComposer}
          onKeyDown={onComposerKeyDown}
          onAcceptCompletion={acceptTypedCompletion}
           onDismissCompletion={clearCompletion}
           partialCompletionSuffix={voiceCompletionSuffix}
            placeholder="Ask Assistant…"
            rows={1}
            maxRows={6}
          />
        <button type={busy ? 'button' : 'submit'} className="composer__button shell__icon-control" aria-label={actionLabel} title={actionLabel}
          disabled={canStop ? promptState === 'stopping' || updateRuntimeBlocked : busy || !canSend} onClick={canStop ? cancelPrompt : undefined}>
          <ControlIcon kind={canStop ? 'stop' : 'send'} />
        </button>
        </div>
        <div className="composer__utility-row">
          <InstantModelSelect variant="pill" id="composerInstantSelect" label="Instant model" value={assistantSettings.instant} options={instantAssistantOptions} disabled={instantControlsDisabled} canRetry={(value) => localInstantCanRetry(startupState, value)} onChange={(value) => void changeInstant(value)} />
          <div className="composer__utility-actions">
            {isAndroidRuntime ? <AndroidComposerVoiceControl /> : voiceInputReady(startupState) ? <button type="button" className="shell__control shell__icon-control" onClick={toggleMic} disabled={!canToggleMic} aria-label={micStarting ? 'Starting mic…' : micActive ? 'Stop mic' : 'Start mic'} title={micActive ? 'Stop mic' : 'Start mic'}><ControlIcon kind={micActive ? 'stop' : 'mic'} /></button> : null}
            <button type="button" className="shell__control shell__icon-control shell__new-chat" aria-label="New Chat" title={resetPending ? 'Starting new chat…' : 'New Chat'} aria-busy={resetPending} disabled={resetPending || updateRuntimeBlocked} onClick={() => void resetSession()}><ControlIcon kind="plus" /></button>
          </div>
        </div>
        {showAvailability ? <div className="composer__availability">
        {startupState.kind === 'error' ? (
          <p className="shell__error">Startup error: {startupState.message}</p>
        ) : null}
        {assistantSettingsLoadError !== null ? (
          <p className="shell__error">Assistant settings unavailable: {assistantSettingsLoadError}</p>
        ) : null}
        {selectedUnavailable ? (
          <p className="shell__error" role="status">
            Sending disabled: {unavailableReason}
          </p>
        ) : null}
        {selectedUnavailable && assistantSettings.instant.startsWith('custom-') && customFailure && supportsDesktopRuntime ? <button type="button" className="shell__control" aria-label="Retry Custom" disabled={customRetryPending || updateRuntimeBlocked} onClick={() => void retryCustom()}>{customRetryPending ? 'Retrying…' : 'Retry Custom'}</button> : null}
        {startupState.kind === 'warming_model' ? (
          <p className="shell__loading">Model loading: {startupState.message}</p>
        ) : null}
        </div> : null}
        </div>
          <div className="composer__actions composer__actions--secondary">
            {runtimeStatus === 'sleeping' && wakeConfidence !== null ? (
              <div className={wakeConfidence >= 0.7 ? 'composer__wake composer__wake--high' : wakeConfidence >= 0.4 ? 'composer__wake composer__wake--medium' : 'composer__wake composer__wake--low'}>
                <span className="composer__wake-primary">wake: {(wakeConfidence * 100).toFixed(2)}%</span>
              </div>
            ) : null}
          {promptActivity !== null ? <div className={promptActivity.compact ? 'sr-only' : 'composer__status'} role="status" aria-live="polite">{promptActivity.text}</div> : null}
        </div>
      </form> : null
}, sameExternalView)

function toRuntimeStatus(runtimePhase: BackendRuntimePhase): RuntimeStatus {
  return runtimePhase
}


function formatResponseAnnouncement(kind: 'text' | 'refusal', text: string): string {
  const boundedText = text.trim().slice(0, 240)
  if (boundedText.length === 0) return ''
  return `${kind === 'refusal' ? 'Assistant refusal' : 'Assistant response'}: ${boundedText}`
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
  if (value === 'light' || value === 'dark' || value === 'slate') {
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

function parseNativeAudioRequestId(payload: unknown, label: string): number {
  if (typeof payload !== 'number' || !Number.isSafeInteger(payload) || payload <= 0) {
    throw new Error(`${label} ID is invalid`)
  }
  return payload
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
      turn !== undefined && isExpectedCanonicalTurn(turn, requestId, expectedSessionId, priorExecutions, isAndroidRuntime)
    ) {
      return turn
    }
  }
  return undefined
}

function executionKey(turn: Turn): string {
  return canonicalExecutionKey(turn)
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

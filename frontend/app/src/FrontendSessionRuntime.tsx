import { memo, useCallback, useEffect, useLayoutEffect } from 'react'
import { FrontendTiming } from './lib/frontendTiming'
import { useSessionView } from './FrontendSession'
import type { FrontendSession } from './FrontendSession'
import { SessionAuthority, cancelPromptProjectionFrame, sessionAuthority, useSessionControls, useSessionIntents } from './SessionAuthority'
import type { PromptProjection } from './SessionAuthority'
import { applySnapshotModelOptions, capabilityIsAvailable, capabilityMap, instantMatchesResponseProfile } from './sessionPolicy'
import type { ChangeEvent, FormEvent, KeyboardEvent } from 'react'
import { SETTINGS_DIALOG_ID, useSettingsNavigation } from './lib/useSettingsNavigation'
import { persistAudioInputDeviceId } from './lib/audioInputPreference'
import { useAndroidVoiceAdapter } from './lib/useAndroidVoiceAdapter'
import { ResponseAnnouncements, SessionTranscript } from './SessionTranscript'
import { PromptComposer } from './components/PromptComposer'
import { SearchModeSelect } from './components/SearchModeSelect'
import { searchModeAvailability, selectNativeSearchMode } from './lib/searchMode'
import type { SearchMode } from './lib/searchMode'
import { AndroidVoiceControls } from './components/AndroidVoiceControls'
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
import { createBrowserCuePlayer, playCueWithFallback, prepareVoiceCues } from './lib/audioCues'
import { shouldSubmitComposer } from './lib/composer'
import { listAudioInputDevices, startLiveAudioSource } from './lib/liveAudioSource'
import type { LiveAudioSource } from './lib/liveAudioSource'
import { executePrompt } from './lib/promptExecution'
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
import { DEFAULT_ASSISTANT_SETTINGS, instantOptions, parseAssistantSettings, serializeAssistantSettings } from './lib/assistantSettings'
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
import { firstNonEmptyCompletedLine, firstNonEmptyStreamingLine, isValidTtsFirstLine } from './lib/ttsValidation'
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
  const composerEditRevisionRef = session.operationRef('composerRevision', () => 0)
  const androidDraftWriteChainRef = session.operationRef<Promise<void>>('draftWrites', () => Promise.resolve())
  const androidDraftPendingRef = session.operationRef<string | null>('draftPending', () => null)
  const androidDraftWriteFailureRef = session.operationRef<unknown>('draftFailure', () => null)
  const persistAndroidDraftRef = session.operationRef<(text: string) => void>('persistDraft', () => () => undefined)
  const shortcutDraftRef = session.operationRef<{ oldDraft: string; oldSession: string | null; revision: number; needsNew: boolean; prepare: Promise<void> | null } | null>('shortcutDraft', () => null)
  const [, setShortcutStaging] = session.fieldState('shortcutStaging')
  const androidSelectionRecoveryRef = session.operationRef<{ readonly transcriptRevision: number; readonly composerRevision: number } | null>('selectionRecovery', () => null)
  const [androidAssistant, setAndroidAssistant] = session.fieldState('androidAssistant')
  const [, setAndroidConversationId] = session.fieldState('androidConversationId')
  const [conversationPagePending, setConversationPagePending] = session.fieldState('conversationPagePending')
  const [headPagePending, setHeadPagePending] = session.fieldState('headPagePending')
  const [androidVoiceActive, setAndroidVoiceActive] = session.fieldState('androidVoiceActive')
  const androidVoiceActiveRef = session.fieldRef('androidVoiceActive')
  const [, setAndroidVoiceStatus] = session.fieldState('androidVoiceStatus')
  const [, setAndroidSpeechStatusError] = session.fieldState('androidSpeechStatusError')
  const onAndroidSpeechStatus = useCallback((status: AndroidVoiceStatus | null, statusError: string | null): void => {
    setAndroidVoiceStatus(status)
    setAndroidSpeechStatusError(statusError)
  }, [setAndroidSpeechStatusError, setAndroidVoiceStatus])
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
  const [, setTtsEnabled] = session.fieldState('ttsEnabled')
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
  const [, setAssistantSettingsLoadError] = session.fieldState('assistantSettingsLoadError')
  const [, setAssistantSettingsWriteError] = session.fieldState('assistantSettingsWriteError')
  const [customRetryPending, setCustomRetryPending] = session.fieldState('customRetryPending')
  const [assistantSettings, setAssistantSettings] = session.fieldState('assistantSettings')
  const [, setSearchMode] = session.fieldState('searchMode')
  const searchModeRef = session.fieldRef('searchMode')
  const [searchModePending, setSearchModePending] = session.fieldState('searchModePending')
  const searchModePendingRef = session.fieldRef('searchModePending')
  const searchModeInitializedRef = session.operationRef('searchInitialized', () => !isNativeTauriRuntime() && !isAndroidRuntime)
  const searchModeWriteRef = session.operationRef<Promise<void>>('searchWrites', () => Promise.resolve())
  const [, setSnapshotModelOptions] = session.fieldState('snapshotModelOptions')
  const snapshotModelOptionsRef = session.fieldRef('snapshotModelOptions')
  const assistantSettingsRef = session.fieldRef('assistantSettings')
  const assistantSettingsPendingRef = session.fieldRef('assistantSettingsPending')
  const persistedAssistantSettingsRef = session.operationRef<AssistantSettings>('persistedSettings', () => DEFAULT_ASSISTANT_SETTINGS)
  const assistantSettingsWriteRevisionRef = session.operationRef('settingsRevision', () => 0)
  const assistantSettingsWriteChainRef = session.operationRef<Promise<void>>('settingsWrites', () => Promise.resolve())
  const instantSelectionRevisionRef = session.operationRef('instantRevision', () => 0)
  const [uiTextSize, setUiTextSize] = session.fieldState('uiTextSize')
  const [uiTheme, setUiTheme] = session.fieldState('uiTheme')
  const [notices, setNotices] = session.fieldState('notices')
  const [noticeInteraction, setNoticeInteraction] = session.fieldState('noticeInteraction')
  const [, setMessages] = session.fieldState('messages')
  const [, setResponseAnnouncement] = session.fieldState('responseAnnouncement')
  const [, setPromptActivity] = session.fieldState('promptActivity')
  const [promptState, setPromptState] = session.fieldState('promptState')
  const [resetPending, setResetPending] = session.fieldState('resetPending')
  const promptRef = session.operationRef<PromptProjection | null>('prompt', () => null)
  const promptProjectionRevisionRef = session.operationRef('promptRevision', () => 0)
  const liveAudioSourceRef = session.operationRef<LiveAudioSource | null>('liveAudio', () => null)
  const liveAudioStartAbortRef = session.operationRef<AbortController | null>('liveAudioAbort', () => null)
  const audioInputDeviceIdRef = session.fieldRef('audioInputDeviceId')
  const liveAudioSessionIdRef = session.operationRef('liveAudioRevision', () => 0)
  const liveAudioInFlightFramesRef = session.operationRef('liveAudioInFlight', () => 0)
  const markSilenceInFlightRef = session.operationRef<number | null>('markSilenceInFlight', () => null)
  const initialSnapshotPreferencesRef = session.operationRef<ValidatedPreferences | null>('initialPreferences', () => null)
  const snapshotCapabilitiesRef = session.operationRef<readonly import('./types/chat').StartupCapability[] | null>('acceptedCapabilities', () => null)
  const snapshotResponseProfileRef = session.operationRef<ResponseProfile | null>('acceptedResponseProfile', () => null)
  const snapshotSessionIdRef = session.operationRef<string | null>('canonicalSession', () => null)
  const uiTextSizePersistedRevisionRef = session.operationRef('textSizePersistedRevision', () => 0)
  const uiThemePersistedRevisionRef = session.operationRef('themePersistedRevision', () => 0)
  const uiTextSizePersistedValueRef = session.operationRef<UiTextSize>('textSizePersisted', () => DEFAULT_UI_TEXT_SIZE)
  const uiThemePersistedValueRef = session.operationRef<UiTheme>('themePersisted', () => DEFAULT_UI_THEME)
  const uiTextSizePersistedInitializedRef = session.operationRef('textSizeInitialized', () => false)
  const uiThemePersistedInitializedRef = session.operationRef('themeInitialized', () => false)
  const uiTextSizePendingRevisionRef = session.operationRef<number | null>('textSizePendingRevision', () => null)
  const uiThemePendingRevisionRef = session.operationRef<number | null>('themePendingRevision', () => null)
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
  const transcriptRevisionRef = session.operationRef('contextRevision', () => 0)
  const canonicalTranscriptRef = session.operationRef<readonly Turn[]>('canonicalTranscript', () => [])
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
    }
    if (replaceHistory) {
      if (snapshot.sessionId !== snapshotSessionIdRef.current || prior?.selectedHead !== assistant.selectedHead) {
        timingRef.current.retireRequest()
      }
      canonicalTranscriptRef.current = snapshot.transcript
      snapshotSessionIdRef.current = snapshot.sessionId
      setMessages((current) => replaceCommittedTranscript(current, snapshot.transcript))
    }
  }, [androidAssistantRef, canonicalTranscriptRef, composerEditRevisionRef, setAndroidAssistant, setAndroidConversationId, setComposerValue, setMessages, shortcutDraftRef, snapshotSessionIdRef, timingRef])
  const ttsGenerationRef = session.operationRef('ttsRevision', () => 0)
  const ttsEnabledRef = session.fieldRef('ttsEnabled')
  const ttsPlaybackIdRef = session.operationRef<number | null>('ttsPlayback', () => null)
  const cuePlayerRef = session.operationRef('cuePlayer', createBrowserCuePlayer)
  const cueAssetPathsRef = session.operationRef<CueAssetPaths>('cuePaths', () => DEFAULT_CUE_ASSET_PATHS)
  const fallbackCueAssetPathsRef = session.operationRef<CueAssetPaths>('fallbackCuePaths', () => DEFAULT_CUE_ASSET_PATHS)
  const ttsPersistedRevisionRef = session.operationRef('ttsPersistedRevision', () => 0)
  const ttsPersistedValueRef = session.operationRef('ttsPersisted', () => false)
  const ttsPersistedInitializedRef = session.operationRef('ttsInitialized', () => false)
  const ttsPendingRevisionRef = session.operationRef<number | null>('ttsPendingRevision', () => null)
  const uiTextSizeWriteRevisionRef = session.operationRef('textSizeRevision', () => 0)
  const uiThemeWriteRevisionRef = session.operationRef('themeRevision', () => 0)
  const ttsWriteRevisionRef = session.operationRef('ttsPreferenceRevision', () => 0)
  const uiTextSizeWriteChainRef = session.operationRef<Promise<void>>('textSizeWrites', () => Promise.resolve())
  const uiThemeWriteChainRef = session.operationRef<Promise<void>>('themeWrites', () => Promise.resolve())
  const ttsWriteChainRef = session.operationRef<Promise<void>>('ttsWrites', () => Promise.resolve())
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
  }, [setTtsPlaying, ttsGenerationRef, ttsPlaybackIdRef])

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
  }, [addNotice, applyStartupState, isSwitchingResponseProfileRef, setMessages, snapshotHydrationRevisionRef, snapshotRuntimeHydratedRef, startupLoadAllowed, startupStateRef])

  useEffect(() => {
    const cuePlayer = cuePlayerRef.current
    session.activate(() => {
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
  useEffect(() => {
    if (searchModeInitializedRef.current || startupState.kind !== 'ready' || runtimeStatus !== 'sleeping' ||
      promptState !== 'idle' || !snapshotControlsFresh || assistantSettingsPending || resetPending) return
    searchModeInitializedRef.current = true
    timingRef.current.startup('INITIAL_SEARCH_MODE_REQUESTED')
    const write = selectNativeSearchMode('quick')
    searchModeWriteRef.current = write.catch(() => undefined)
    void write.then(() => {
      if (appActiveRef.current) timingRef.current.startup('INITIAL_SEARCH_MODE_ACK')
    }).catch((error) => {
      if (appActiveRef.current) addNotice({ tone: 'error', title: 'Search mode not initialized', message: toDisplayErrorMessage(error) })
    }).finally(() => {
      searchModePendingRef.current = false
      if (appActiveRef.current) setSearchModePending(false)
    })
  }, [addNotice, appActiveRef, assistantSettingsPending, promptState, resetPending, runtimeStatus, searchModeInitializedRef, searchModePendingRef, searchModeWriteRef, setSearchModePending, snapshotControlsFresh, startupState.kind, timingRef])
  const changeSearchMode = async (mode: SearchMode): Promise<void> => {
    if (instantControlsDisabled || runtimeStatusRef.current !== 'sleeping' || micStarting || micActive ||
      searchModePendingRef.current || resetPendingRef.current || promptRef.current !== null ||
      !searchModeAvailability(assistantSettingsRef.current, selectedInstantOption?.available === true).available) return
    searchModePendingRef.current = true
    setSearchModePending(true)
    const write = selectNativeSearchMode(mode)
    searchModeWriteRef.current = write.catch(() => undefined)
    try {
      await write
      if (!appActiveRef.current || resetPendingRef.current) return
      searchModeRef.current = mode
      setSearchMode(mode)
    } catch (error) {
      if (appActiveRef.current) addNotice({ tone: 'error', title: 'Search mode not changed', message: toDisplayErrorMessage(error) })
    } finally {
      searchModePendingRef.current = false
      if (appActiveRef.current) setSearchModePending(false)
    }
  }
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
    nativeRequestId?: string,
  ): Promise<void> => {
    const admission = session.admitPrompt(prompt, source, nativeRequestId, updateRuntimeBlocked)
    if (admission === null) return
    const admittedAt = performance.now()
    const appActiveRef = { get current() { return admission.isCurrent() } }
    const currentSettings = admission.settings
    const executingStatus = applyTransition(runtimeStatusRef.current, 'submit_prompt')
    const promptRuntimeActivityRevision = runtimeActivityRevisionRef.current
    const admittedMode = admission.mode
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
        const timings = requestTiming.received(event.text)
        if (isAndroidRuntime && timings !== undefined) active.customDirectTimings = timings
        const hadCompleteLine = active.text.includes('\n')
        active.text += event.text
        if (source === 'voice' && !hadCompleteLine && active.text.includes('\n')) {
          voiceTelemetry.record('provider_first_line')
        }
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
        session.schedulePromptProjection(active)
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
        setPromptActivity({ text: `${event.stage[0]?.toUpperCase()}${event.stage.slice(1)} stage ${event.status}`, compact: event.status !== 'failed' })
      }
      if (event.kind === 'sources') {
        active.sources = [...event.sources]
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
    try {
      requestTiming.dispatch()
      result = await executePrompt(requestId, prompt, handleEvent, source, admittedMode)
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
        const snapshotControlRevision = ++snapshotControlRequestRevisionRef.current
        const snapshotPreferenceRevision = getAndroidPreferencePublicationRevision()
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

  useEffect(() => {
    drainAndroidVoiceRequestRef.current = () => {
      const pending = pendingAndroidVoiceRequestRef.current
      if (pending === null) return
      if (pending.inputEpoch !== androidVoiceInputEpochRef.current || pending.composerRevision !== composerEditRevisionRef.current) { pendingAndroidVoiceRequestRef.current = null; return }
      if (!appActiveRef.current || document.visibilityState !== 'visible') return
      if (shortcutDraftRef.current?.needsNew === true || androidSelectionRecoveryRef.current !== null || promptRef.current !== null || resetPendingRef.current || !snapshotControlsFreshRef.current || assistantSettingsPendingRef.current) return
      if (pending.sessionId !== snapshotSessionIdRef.current || pending.selectedHead !== (androidAssistantRef.current?.selectedHead ?? null)) { pendingAndroidVoiceRequestRef.current = null; androidVoiceStopRef.current?.(); return }
      pendingAndroidVoiceRequestRef.current = null
      void runPrompt(pending.text, 'typed', pending.requestId)
    }
  })

  async function synthesizeAndPlayAssistantReply(text: string): Promise<void> {
    let commandPending = false
    let playbackId: number | null = null
    cancelTts()
    const generation = ttsGenerationRef.current

    try {
      const speechText = firstNonEmptyCompletedLine(text) ?? ''
      if (!isValidTtsFirstLine(speechText)) return
      voiceTelemetry.record('tts_synthesis_requested')
      playbackId = parseNativeAudioRequestId(
        await invokeTauriCommand('reserve_local_tts_playback_id'),
        'TTS playback',
      )
      if (generation !== ttsGenerationRef.current || !ttsEnabledRef.current) return
      ttsPlaybackIdRef.current = playbackId
      setTtsPlaying(true)
      commandPending = true
      setPendingTtsCommands((count) => count + 1)
      cuePlayerRef.current.stop()
      const payload = await invokeTauriCommand('speak_local_tts', {
        text: speechText,
        playbackId,
      })
      voiceTelemetry.record('tts_playback_completed')
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

  const stopLiveAudio = (content: string | null = null): void => {
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

              applyRuntimeControlResult(status, { quiet: true })
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
      searchModeRef.current = 'quick'
      setSearchMode('quick')
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
      await searchModeWriteRef.current
      resetDraftRevision = composerEditRevisionRef.current
      if (androidSelectionRecoveryRef.current !== null) androidSelectionRecoveryRef.current = { transcriptRevision, composerRevision: resetDraftRevision }
      await invokeTauriCommand('reset_session', isAndroidRuntime ? { draft: composerValueRef.current } : undefined)
      if (appActiveRef.current) timingRef.current.retireRequest()
      searchModeInitializedRef.current = true
      searchModeRef.current = 'quick'
      setSearchMode('quick')
      try { await selectNativeSearchMode('quick') }
      finally {
        searchModePendingRef.current = false
        if (appActiveRef.current) setSearchModePending(false)
      }
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
    androidDraftPendingRef.current = text
    if (shortcutDraftRef.current?.needsNew === true || androidAssistantRef.current === null || promptRef.current !== null || resetPendingRef.current || androidSelectionRecoveryRef.current !== null) return
    const revision = composerEditRevisionRef.current
    const sessionId = snapshotSessionIdRef.current
    androidDraftWriteChainRef.current = androidDraftWriteChainRef.current.then(async () => {
      if (shortcutDraftRef.current?.needsNew === true || androidSelectionRecoveryRef.current !== null || revision !== composerEditRevisionRef.current || sessionId !== snapshotSessionIdRef.current) return
      await invokeTauriCommand('save_assistant_draft', { text })
      if (revision === composerEditRevisionRef.current && sessionId === snapshotSessionIdRef.current) {
        androidDraftPendingRef.current = null
        androidDraftWriteFailureRef.current = null
      }
      if (androidAssistantRef.current?.error !== null) refreshAndroidSnapshotControlsRef.current()
    }).catch((error: unknown) => {
      androidDraftWriteFailureRef.current = error
      if (appActiveRef.current) addNotice({ tone: 'error', title: 'Draft was not saved', message: toDisplayErrorMessage(error) })
    })
  }

  useEffect(() => { persistAndroidDraftRef.current = persistAndroidDraft })

  async function beginCurrentVoiceDraft(): Promise<string> {
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
    if (!isAndroidRuntime || !generationAvailable() || updateRuntimeBlocked || resetPendingRef.current || promptRef.current !== null || androidVoiceActiveRef.current || androidSelectionRecoveryRef.current !== null || !snapshotControlsFreshRef.current || assistantSettingsPendingRef.current || !searchModeInitializedRef.current || searchModePendingRef.current || shortcutDraftRef.current?.needsNew === true) throw new Error('Voice chat is busy; existing work was preserved')
    const conversation = snapshotSessionIdRef.current
    const selectedHead = androidAssistantRef.current?.selectedHead ?? null
    const revision = composerEditRevisionRef.current
    await androidDraftWriteChainRef.current
    if (androidDraftWriteFailureRef.current !== null) throw androidDraftWriteFailureRef.current
    if (!appActiveRef.current || !generationAvailable() || updateRuntimeBlocked || document.visibilityState !== 'visible' || resetPendingRef.current || promptRef.current !== null || androidSelectionRecoveryRef.current !== null || !snapshotControlsFreshRef.current || snapshotSessionIdRef.current !== conversation || (androidAssistantRef.current?.selectedHead ?? null) !== selectedHead || composerEditRevisionRef.current !== revision || assistantSettingsPendingRef.current || !searchModeInitializedRef.current || searchModePendingRef.current) throw new Error('Voice chat was interrupted; existing work was preserved')
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
    beginCurrentVoiceDraft, onAndroidVoiceActivity, onAndroidSpeechStatus, onAndroidVoiceInputCancelled,
    changeInstant, changeSearchMode, resetSession, retryCustom, cancelPrompt,
    toggleUiTheme, adjustUiTextSize, toggleTts, toggleMic, changeAudioInputDevice,
    setAutoStopOnSilence, setVoiceNotesOpen, setNoticeInteraction,
    loadMoreAndroidConversations, loadMoreAndroidHeads, selectAndroidConversation,
    canRetry: (value: AssistantSettings['instant']) => localInstantCanRetry(startupStateRef.current, value),
  })
  const voice = useAndroidVoiceAdapter({
    enabled: isAndroidRuntime && !voiceNotesOpen,
    statusEnabled: isAndroidRuntime && (!voiceNotesOpen || settingsOpen),
    draft: composerValue,
    disabled: searchModePending || resetPending || promptState !== 'idle' || !snapshotControlsFresh || assistantSettingsPending,
    generationAvailable: !nativeConversationNeedsRecovery && selectedInstantOption?.available === true && (androidAssistant?.branchChoices.length ?? 0) === 0,
    shortcutReady: snapshotHydrationState === 'succeeded' && androidAssistant !== null && snapshotControlsFresh && !snapshotControlsInvalidated,
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
      !searchModePending &&
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
  }, [androidAssistant?.branchChoices.length, androidSelectionRecoveryRef, androidVoiceActive, assistantSettings.instant, assistantSettingsPending, isSwitchingResponseProfile, nativeConversationNeedsRecovery, promptState, resetPending, runtimeStatus, searchModePending, selectedInstantOption?.available, shortcutDraftRef, snapshotControlsFresh, startupState, startupTelemetrySentRef, timingRef, updateRuntimeBlocked])

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
    <SessionToolbar session={session} view={view} />
    <SessionConversation session={session} view={view} />
    <SessionSettings session={session} view={view} />
    <SessionComposer session={session} view={view} />
  </div>
}, sameExternalView)

function usePresentation(session: FrontendSession, view: SessionViewProps['view'], name: Parameters<typeof session.getViewSnapshot>[0]) {
  useSessionView(session, name)
  const state = session.getControlsSnapshot()
  const { startupState, assistantSettings, runtimeStatus, promptState, resetPending,
    assistantSettingsPending, androidVoiceActive, isSwitchingResponseProfile, snapshotControlsFresh,
    searchModePending, micActive, micStarting } = state
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
    searchAvailability: searchModeAvailability(assistantSettings, selectedInstantOption?.available === true),
    assistantControlsDisabled, instantControlsDisabled,
    canSend: startupState.kind === 'ready' && selectedInstantOption?.available === true && selectedInstantProfileReady &&
      !assistantSettingsPending && !searchModePending && snapshotControlsFresh && !resetPending && !androidVoiceActive &&
      !nativeConversationNeedsRecovery && (state.androidAssistant?.branchChoices.length ?? 0) === 0 && promptState === 'idle' &&
      runtimeStatus === 'sleeping' && !updateRuntimeBlocked && state.composerValue.trim().length > 0,
    canToggleMic: !updateRuntimeBlocked && (micActive || (!searchModePending && voiceControlsUsable && voiceInputReady(startupState) && !micStarting && !resetPending)),
    canToggleTts: startupState.kind === 'ready' && capabilityIsAvailable(startupState, 'tts') && !isSwitchingResponseProfile && !updateRuntimeBlocked,
    voiceInputUnavailableReason: startupState.kind === 'ready' ? startupState.voiceInputError ?? (startupState.capabilities.filter((capability) =>
      ['wake_word', 'vad', 'parakeet'].includes(capability.id) && capability.state !== 'available').map((capability) => capability.reason ?? `${capabilityLabel(capability.id)} unavailable`).join('; ') || 'required voice capabilities are unavailable') : 'voice input is still starting',
    updateInstallationDisabled: startupState.kind !== 'ready' || runtimeStatus !== 'sleeping' || promptState !== 'idle' || resetPending ||
      isSwitchingResponseProfile || assistantSettingsPending || micStarting || state.ttsPlaying || state.pendingTtsCommands > 0,
    selectedAudioInputDevice: state.audioInputDevices.find((device) => device.deviceId === state.audioInputDeviceId),
    canDecreaseTextSize: sizeIndex > 0, canIncreaseTextSize: sizeIndex < UI_TEXT_SIZE_STEPS.length - 1,
    themeToggleLabel: `Switch to ${state.uiTheme === 'dark' ? 'light' : 'dark'} mode`,
    customFailure: startupState.kind === 'ready' || startupState.kind === 'warming_model' ? startupState.capabilities.find((capability) =>
      capability.id === 'custom_provider' && (capability.state === 'failed' || capability.state === 'unavailable')) : undefined,
    canEnableDeep: options.deepAssistantOptions.some((option) => option.value === assistantSettings.deep && option.available),
    canEnableReview: options.reviewAssistantOptions.some((option) => option.value === assistantSettings.review && option.available),
    headerBusy: androidVoiceActive || ['listening', 'processing', 'executing'].includes(runtimeStatus) || promptState !== 'idle' || resetPending || isSwitchingResponseProfile || startupState.kind === 'warming_model',
  }
}

const SessionToolbar = memo(function SessionToolbar({ session, view }: SessionViewProps) {
  const {
    updateRuntimeBlocked, startupState, runtimeStatus, shortcutStaging, androidAssistant, androidConversationId,
    headPagePending, androidVoiceActive, micStarting, micActive, voiceNotesOpen, assistantSettingsPending,
    snapshotControlsFresh, assistantSettings, searchMode, searchModePending, uiTheme, promptActivity,
    promptState, resetPending, instantAssistantOptions, selectedInstantOption, nativeConversationNeedsRecovery,
    searchAvailability, instantControlsDisabled, themeToggleLabel, headerBusy,
  } = usePresentation(session, view, 'toolbar')
  const { settingsOpen, settingsButtonRef, openSettings } = view.navigation
  const {
    changeInstant, changeSearchMode, resetSession, cancelPrompt, toggleUiTheme,
    setVoiceNotesOpen, selectAndroidConversation, canRetry,
  } = view.actions
  const searchModeControl = <SearchModeSelect iconOnly={isAndroidRuntime} value={searchAvailability.available ? searchMode : 'quick'}
    disabled={!searchAvailability.available || instantControlsDisabled || searchModePending || runtimeStatus !== 'sleeping' || micStarting || micActive}
    reason={searchAvailability.reason} onChange={(mode) => void changeSearchMode(mode)} />
  const stopPromptControl = (promptState === 'executing' || promptState === 'stopping') &&
    startupState.kind === 'ready' && startupState.promptCancellationAvailable ? (
      <button type="button" className={isAndroidRuntime ? 'shell__control shell__icon-control' : 'shell__control'}
        aria-label={isAndroidRuntime ? promptState === 'stopping' ? 'Stopping…' : 'Stop' : undefined}
        title={isAndroidRuntime ? promptState === 'stopping' ? 'Stopping…' : 'Stop' : undefined}
        onClick={cancelPrompt} disabled={promptState === 'stopping' || updateRuntimeBlocked}>
        {isAndroidRuntime ? <><ControlIcon kind="stop" /><span className="sr-only">{promptState === 'stopping' ? 'Stopping…' : 'Stop'}</span></> : promptState === 'stopping' ? 'Stopping…' : 'Stop'}
      </button>
    ) : null
  const notesControl = <button type="button" className="shell__control shell__icon-control" aria-label={voiceNotesOpen ? 'Chat' : 'Voice notes'} title={voiceNotesOpen ? 'Chat' : 'Voice notes'} aria-pressed={voiceNotesOpen} onClick={() => setVoiceNotesOpen((open) => !open)}>
    <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path d="M5 3h10l4 4v14H5z M15 3v5h4 M8 12h8 M8 16h6" /></svg>
  </button>
  const conversationControl = androidAssistant !== null ? <label className="shell__control shell__icon-control shell__conversation-picker" title={androidAssistant.conversations.find((item) => item.id === androidConversationId)?.title ?? 'Conversation'}>
    <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path d="M4 4h16v12H9l-5 4z M8 8h8 M8 12h6" /></svg>
    <select className="shell__conversation-select" aria-label="Conversation" value={androidConversationId} disabled={shortcutStaging || resetPending || promptState !== 'idle' || androidVoiceActive || headPagePending || !snapshotControlsFresh} onChange={(event) => void selectAndroidConversation('select_conversation', event.target.value)}>
      <option value="0" disabled>New draft</option>
      {androidAssistant.conversations.map((conversation) => <option key={conversation.id} value={conversation.id}>{conversation.title}</option>)}
    </select>
  </label> : null
  const newChatControl = <button type="button" className={`shell__control shell__new-chat${isAndroidRuntime ? ' shell__icon-control' : ''}`} onClick={() => void resetSession()} aria-label="New Chat" aria-busy={resetPending} title={resetPending ? 'Starting new chat…' : 'New Chat'} disabled={resetPending || updateRuntimeBlocked}>
    {isAndroidRuntime ? <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path d="M12 5v14 M5 12h14" /></svg> : resetPending ? 'Starting new chat...' : 'New Chat'}
  </button>
  const settingsControl = <button ref={settingsButtonRef} type="button" className={`shell__control shell__settings-button${isAndroidRuntime ? ' shell__icon-control' : ''}`} onClick={openSettings} aria-label="Settings" aria-haspopup="dialog" aria-controls={SETTINGS_DIALOG_ID} aria-expanded={settingsOpen} title="Settings">
    {isAndroidRuntime ? <ControlIcon kind="settings" /> : '⚙'}
  </button>
  const androidVoiceControls = isAndroidRuntime && !voiceNotesOpen ? <AndroidVoiceControls voice={view.voice} compact secondaryControls={<>{notesControl}{conversationControl}{newChatControl}{searchModeControl}{settingsControl}</>} modelControl={<InstantModelSelect iconOnly id="composerInstantSelect" label="Instant model" value={assistantSettings.instant} options={instantAssistantOptions} disabled={instantControlsDisabled} canRetry={canRetry} onChange={changeInstant} />} disabled={searchModePending || resetPending || promptState !== 'idle' || !snapshotControlsFresh || assistantSettingsPending}
    generationAvailable={!nativeConversationNeedsRecovery && selectedInstantOption?.available === true && (androidAssistant?.branchChoices.length ?? 0) === 0} /> : null

  return (
      <header className="shell__header" aria-label="VoxGolem">
        <div className="shell__identity" data-shell-identity>
          <img className="shell__logo" src={headerIcon} alt="VoxGolem" width={isAndroidRuntime ? 32 : 40} height={isAndroidRuntime ? 32 : 40} data-busy={headerBusy} />
          <div className="shell__runtime">
            {isBrowserPreviewRuntime() ? <span data-browser-preview>Browser preview</span> : null}
            {!isAndroidRuntime ? <span data-runtime-phase>Runtime: {runtimeStatus}</span> : null}
          </div>
        </div>
        <div className="shell__header-actions" role={isAndroidRuntime ? 'group' : undefined} aria-label={isAndroidRuntime ? 'Android toolbar' : undefined} aria-describedby={isAndroidRuntime && shortcutStaging ? 'new-voice-draft-status' : undefined}>
          {isAndroidRuntime && voiceNotesOpen ? <>{notesControl}<PhoneActionOverflow>{conversationControl}{newChatControl}{settingsControl}</PhoneActionOverflow></> : null}
          {!isAndroidRuntime ? newChatControl : null}
          {!isAndroidRuntime ? <button
            type="button"
            className="shell__control shell__theme-button"
            onClick={toggleUiTheme}
            aria-label={themeToggleLabel}
            title={themeToggleLabel}
          >
            {uiTheme === 'dark' ? '☀' : '☾'}
          </button> : null}
          {androidVoiceControls}
          {isAndroidRuntime && !voiceNotesOpen ? <>
            {stopPromptControl}
            {promptActivity?.compact ? <span className="composer__status composer__status--compact" role="status" aria-live="polite" title={promptActivity.text}>
              <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false"><circle cx="12" cy="12" r="8" /><path d="M12 7v5l3 2" /></svg>
              <span className="sr-only">{promptActivity.text}</span>
            </span> : null}
          </> : null}
          {!isAndroidRuntime ? settingsControl : null}
        </div>
      </header>
  )
}, sameExternalView)

const SessionConversation = memo(function SessionConversation({ session, view }: SessionViewProps) {
  const { voiceNotesOpen, customFailure, supportsDesktopRuntime, customRetryPending, updateRuntimeBlocked,
    shortcutStaging, composerValue, uiTextSize, notices } = usePresentation(session, view, 'conversation')
  const { setVoiceNotesOpen, retryCustom, setNoticeInteraction } = view.actions
  return voiceNotesOpen ? <CaptureHistory onBack={() => setVoiceNotesOpen(false)} /> : <>
      <div className="conversation-region">
        {customFailure ? <div role="alert" className="shell__error">
          Custom connection: {customFailure.reason}{' '}
          {supportsDesktopRuntime ? <button type="button" aria-label="Retry Custom" disabled={customRetryPending || updateRuntimeBlocked} onClick={() => void retryCustom()}>Retry Custom</button> : null}
        </div> : null}
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
  const { uiTheme, themeToggleLabel, assistantSettingsWriteError, uiTextSize, canDecreaseTextSize, canIncreaseTextSize,
    supportsDesktopRuntime, selectedAudioInputDevice, audioInputDeviceId, micStarting, updateRuntimeBlocked, audioInputDevices,
    autoStopOnSilence, startupState, voiceInputUnavailableReason, ttsEnabled, canToggleTts, androidVoiceStatus,
    androidSpeechStatusError, assistantSettings, instantControlsDisabled, instantAssistantOptions, assistantCapabilities,
    selectedInstantRetryable, selectedInstantOption, assistantControlsDisabled, deepAssistantOptions, reviewAssistantOptions,
    canEnableDeep, canEnableReview, appUpdates, updateInstallationDisabled } = usePresentation(session, view, 'settings')
  const { settingsOpen, settingsPanelRef, settingsCloseButtonRef, closeSettings, onSettingsKeyDown } = view.navigation
  const { toggleUiTheme, adjustUiTextSize, changeAudioInputDevice, setAutoStopOnSilence, toggleTts, patchAssistantSettings,
    changeInstant, clearCompletion } = view.actions
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
                <p>Adjust the app text size live.</p>
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
            </div>
            {isAndroidRuntime ? <div className="settings-panel__row">
              <strong>Theme</strong>
              <button type="button" className="shell__control settings-panel__interactive" onClick={toggleUiTheme} aria-label={themeToggleLabel} title={themeToggleLabel}>
                {uiTheme === 'dark' ? 'Dark' : 'Light'}
              </button>
            </div> : null}
            {assistantSettingsWriteError !== null ? <p className="shell__error settings-panel__write-error" role="alert">Assistant settings could not be saved: {assistantSettingsWriteError}</p> : null}
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
            {supportsDesktopRuntime ? <><div className="settings-panel__row">
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
            </div>
            <div className="settings-panel__row">
              <div>
                <strong>Speech output</strong>
                <p className="settings-panel__hint">Read assistant responses aloud.</p>
              </div>
               <label className="shell__toggle settings-panel__interactive">
                <input
                  id="tts-toggle"
                  type="checkbox"
                  checked={ttsEnabled}
                  disabled={!canToggleTts}
                  onChange={(event) => { void toggleTts(event.target.checked) }}
                />
                <span>TTS</span>
              </label>
            </div></> : null}
            {isAndroidRuntime || supportsDesktopRuntime ? <SpeechSettings android={isAndroidRuntime} androidStatus={androidVoiceStatus} statusError={androidSpeechStatusError} startupState={startupState}
              speechNetworkFallback={assistantSettings.speechNetworkFallback} speechSelection={assistantSettings} disabled={instantControlsDisabled}
              onSpeechSelectionChange={patchAssistantSettings}
              onSpeechNetworkFallbackChange={(enabled) => patchAssistantSettings({ speechNetworkFallback: enabled })} /> : null}
            <section className="settings-panel__web" aria-labelledby="web-settings-title">
              <h3 id="web-settings-title">Web access</h3>
              <label className="shell__toggle settings-panel__interactive"><input type="checkbox" aria-label="Allow web access" checked={assistantSettings.webEnabled} disabled={instantControlsDisabled}
                onChange={(event) => patchAssistantSettings({ webEnabled: event.target.checked })} /> Allow web access</label>
              <p className="settings-panel__hint">{assistantSettings.webEnabled ? 'Native search may send question-derived queries and public-page requests to external providers: Parallel is primary, with Exa as fallback when needed. Delegated OpenCode routes may use their own search providers.' : 'Web is off: no web lookup or search fallback. Normal chat still uses the selected chat provider; answers are not web-verified.'}</p>
              <p className="settings-panel__hint">Search effort (Quick, Verify, Research) is separate from this permission. Providers used depend on availability; search and fallback activity appear in the response status when reported.</p>
            </section>
            <div className="settings-panel__assistant">
               <label className="settings-panel__interactive"><strong>Instant</strong><InstantModelSelect id="assistantInstantSelect" label="Instant" value={assistantSettings.instant} options={instantAssistantOptions} disabled={instantControlsDisabled} canRetry={(value) => localInstantCanRetry(startupState, value)} onChange={(value) => void changeInstant(value)} /></label>
              {startupState.kind === 'ready' && !assistantCapabilities.custom ? (
                <p className="settings-panel__hint">Custom connection: {startupState.capabilities.find((capability) => capability.id === 'custom_provider')?.reason ?? 'unavailable'}</p>
              ) : null}
               {selectedInstantRetryable && selectedInstantOption ? <button className="settings-panel__interactive" type="button" aria-label="Retry local profile" disabled={instantControlsDisabled} onClick={() => void changeInstant(assistantSettings.instant)}>Retry local profile</button> : null}
               <label className="settings-panel__interactive"><strong>Deep model</strong><select aria-label="Deep model" value={assistantSettings.deep} disabled={assistantControlsDisabled} onChange={(event) => {
                const value = event.target.value as AssistantSettings['deep']
                if (deepAssistantOptions.find((option) => option.value === value)?.available) {
                   patchAssistantSettings({ deep: value })
                }
              }}>
                {deepAssistantOptions.map((option) => <option key={option.value} value={option.value} disabled={!option.available} title={option.reason}>{option.label}</option>)}
               </select></label>
               <label className="settings-panel__interactive"><strong>Review model</strong><select aria-label="Review model" value={assistantSettings.review} disabled={assistantControlsDisabled} onChange={(event) => {
                const value = event.target.value as AssistantSettings['review']
                if (reviewAssistantOptions.find((option) => option.value === value)?.available) {
                   patchAssistantSettings({ review: value })
                }
              }}>
                {reviewAssistantOptions.map((option) => <option key={option.value} value={option.value} disabled={!option.available} title={option.reason}>{option.label}</option>)}
              </select></label>
               <label className="settings-panel__interactive"><input type="checkbox" checked={assistantSettings.deepEnabled} disabled={assistantControlsDisabled || (!assistantSettings.deepEnabled && !canEnableDeep)} onChange={(event) => patchAssistantSettings({ deepEnabled: event.target.checked })} /> Deep</label>
               <label className="settings-panel__interactive"><input type="checkbox" checked={assistantSettings.reviewEnabled} disabled={assistantControlsDisabled || (!assistantSettings.reviewEnabled && !canEnableReview)} onChange={(event) => patchAssistantSettings({ reviewEnabled: event.target.checked })} /> Review</label>
               <label className="settings-panel__interactive"><input type="checkbox" checked={assistantSettings.completion} disabled={assistantControlsDisabled || (!assistantSettings.completion && !assistantCapabilities.qwenPrediction)} onChange={(event) => {
                if (!event.target.checked) clearCompletion()
                 patchAssistantSettings({ completion: event.target.checked })
               }} /> Completion</label>
               <label className="settings-panel__interactive"><input type="checkbox" checked={assistantSettings.prefetch} disabled={assistantControlsDisabled} onChange={(event) => patchAssistantSettings({ prefetch: event.target.checked })} /> Prefetch</label>
              <p className="settings-panel__hint">Prefetch may transmit unaccepted predicted text when enabled.</p>
            </div>
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
    instantControlsDisabled, searchAvailability, searchMode, searchModePending, runtimeStatus, updateRuntimeBlocked,
    promptActivity, wakeConfidence } = usePresentation(session, view, 'composer')
  const { onSubmit, loadMoreAndroidConversations, loadMoreAndroidHeads, selectAndroidConversation, changeComposer,
    onComposerKeyDown, acceptTypedCompletion, clearCompletion, toggleMic, changeInstant, changeSearchMode, cancelPrompt } = view.actions
  const searchModeControl = <SearchModeSelect iconOnly={isAndroidRuntime} value={searchAvailability.available ? searchMode : 'quick'}
    disabled={!searchAvailability.available || instantControlsDisabled || searchModePending || runtimeStatus !== 'sleeping' || micStarting || micActive}
    reason={searchAvailability.reason} onChange={(mode) => void changeSearchMode(mode)} />
  const stopPromptControl = (promptState === 'executing' || promptState === 'stopping') && startupState.kind === 'ready' && startupState.promptCancellationAvailable ?
    <button type="button" className="shell__control" onClick={cancelPrompt} disabled={promptState === 'stopping' || updateRuntimeBlocked}>{promptState === 'stopping' ? 'Stopping…' : 'Stop'}</button> : null
  return !voiceNotesOpen ? <form className="composer" onSubmit={onSubmit}>
        {shortcutStaging ? <p id="new-voice-draft-status" className={isAndroidRuntime ? 'sr-only' : undefined} role="status">New voice draft — Stop &amp; send starts a new conversation. Previous history and draft are preserved.</p> : null}
        {isAndroidRuntime && androidAssistant !== null ? <div className="composer__actions">
          {androidAssistant.conversationOffset < androidAssistant.conversationCount ? <button type="button" disabled={conversationPagePending || resetPending || promptState !== 'idle' || androidVoiceActive} onClick={() => void loadMoreAndroidConversations()}>{conversationPagePending ? 'Loading conversations…' : 'More conversations'}</button> : null}
          {androidAssistant.branchChoices.length > 0 ? <label>Choose continuation <select aria-label="Choose continuation" value="" disabled={shortcutStaging || resetPending || promptState !== 'idle' || androidVoiceActive || headPagePending || !snapshotControlsFresh} onChange={(event) => void selectAndroidConversation('select_conversation_head', event.target.value)}>
            <option value="" disabled>Choose continuation</option>
            {androidAssistant.branchChoices.map((head, index) => <option key={head} value={head}>Continuation {index + 1}</option>)}
          </select></label> : null}
          {androidAssistant.branchOffset < androidAssistant.branchCount ? <button type="button" disabled={headPagePending || resetPending || promptState !== 'idle' || androidVoiceActive} onClick={() => void loadMoreAndroidHeads()}>{headPagePending ? 'Loading continuations…' : 'More continuations'}</button> : null}
          {androidAssistant.interrupted ? <span role="status">Interrupted draft restored. Review before sending.</span> : null}
          {androidAssistant.error !== null ? <span role="status">Native conversation status: {androidAssistant.error.replaceAll('_', ' ')}</span> : null}
        </div> : null}
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
           placeholder="Type a prompt..."
           rows={isAndroidRuntime ? 1 : 2}
           maxRows={isAndroidRuntime ? 1 : 4}
          />
        {isAndroidRuntime ? <button type="submit" className="composer__button shell__icon-control" aria-label="Send" title="Send" disabled={!canSend}><ControlIcon kind="send" /></button> : null}
        </div>
        {startupState.kind === 'error' ? (
          <p className="shell__error">Startup error: {startupState.message}</p>
        ) : null}
        {assistantSettingsLoadError !== null ? (
          <p className="shell__error">Assistant settings unavailable: {assistantSettingsLoadError}</p>
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
          <div className={isAndroidRuntime ? 'composer__actions composer__actions--secondary' : 'composer__actions'}>
           {voiceInputReady(startupState) ? (
             <button type="button" className="shell__control" onClick={toggleMic} disabled={!canToggleMic}>
               {micStarting ? 'Starting mic...' : micActive ? 'Stop mic' : 'Start mic'}
             </button>
           ) : null}
              {!isAndroidRuntime ? <label className="composer__instant-model"><strong>Instant:</strong> <InstantModelSelect id="composerInstantSelect" label="Instant model" value={assistantSettings.instant} options={instantAssistantOptions} disabled={instantControlsDisabled} canRetry={(value) => localInstantCanRetry(startupState, value)} onChange={(value) => void changeInstant(value)} /></label> : null}
             {!isAndroidRuntime ? searchModeControl : null}
          <div className="composer__send-side">
            {!isAndroidRuntime ? stopPromptControl : null}
            {runtimeStatus === 'sleeping' && wakeConfidence !== null ? (
              <div className={wakeConfidence >= 0.7 ? 'composer__wake composer__wake--high' : wakeConfidence >= 0.4 ? 'composer__wake composer__wake--medium' : 'composer__wake composer__wake--low'}>
                <span className="composer__wake-primary">wake: {(wakeConfidence * 100).toFixed(2)}%</span>
              </div>
            ) : null}
            {!isAndroidRuntime ? <button type="submit" className="composer__button" disabled={!canSend}>
              Send
            </button> : null}
          </div>
          {promptActivity !== null && (!isAndroidRuntime || !promptActivity.compact) ? <div className="composer__status" role="status" aria-live="polite">{promptActivity.text}</div> : null}
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

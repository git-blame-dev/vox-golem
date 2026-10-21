import { memo, useCallback, useEffect, useLayoutEffect } from 'react'
import { FrontendTiming, reportAppBenchmarkStage } from './lib/frontendTiming'
import { useSessionView } from './FrontendSession'
import type { FrontendSession } from './FrontendSession'
import { SessionAuthority, cancelPromptProjectionFrame, sessionAuthority, useSessionIntents, useSessionView as useAuthorityView } from './SessionAuthority'
import type { PromptProjection } from './SessionAuthority'
import {
  appUpdateBlocksRuntime,
  applySnapshotModelOptions,
  capabilityIsAvailable,
  capabilityLabel,
  capabilityMap,
  instantMatchesResponseProfile,
  localInstantCanRetry,
  parseUiTextSize,
  parseUiTheme,
  sameExternalView,
  startupWarningRelevant,
  UI_TEXT_SIZE_STEPS,
  usePresentation,
  voiceInputReady,
} from './sessionPolicy'
import type { SessionViewProps } from './sessionPolicy'
import type { ChangeEvent, FormEvent, KeyboardEvent, ReactNode } from 'react'
import { useSettingsNavigation } from './lib/useSettingsNavigation'
import { persistAudioInputDeviceId } from './lib/audioInputPreference'
import { useAndroidVoiceAdapter } from './lib/useAndroidVoiceAdapter'
import { AndroidVoiceControlsProvider } from './components/AndroidVoiceControls'
import { SessionComposer } from './components/SessionComposer'
import { SessionConversation } from './components/SessionConversation'
import { SessionSettings } from './components/SessionSettings'
import { SessionToolbar } from './components/SessionToolbar'
import type { AndroidVoiceContext, AndroidVoiceStatus } from './lib/androidVoice'
import { isConfirmedNewVoiceContext, NEW_VOICE_SHORTCUT_EVENT } from './lib/newVoiceShortcut'
import { canonicalMessageId, replaceCommittedTranscript, settleCanonicalAnswerStages } from './committedTranscript'
import { shouldSubmitComposer } from './lib/composer'
import { listAudioInputDevices, startLiveAudioSource } from './lib/liveAudioSource'
import { executePrompt } from './lib/promptExecution'
import { mergeAnswerSources } from './lib/answerSources'
import { appendCanonicalTurn, canonicalExecutionKey, isExpectedCanonicalTurn, loadAppSnapshot, parseAppSnapshot, parseAndroidConversationPage, parseAndroidHeadPage } from './lib/appSnapshot'
import type { AppSnapshot, Capability, Turn, ValidatedPreferences } from './lib/appSnapshot'
import { DesktopRuntimeObserver } from './lib/desktopRuntime'
import type { DesktopRuntimeEvent, DesktopRuntimeSnapshot, NativePromptProjection, VoiceProjection, SpeechProjection } from './lib/desktopRuntime'
import {
  CAPABILITY_IDS,
  isStartupStateSettled,
  loadStartupState,
  startupStateFromSnapshot,
} from './lib/startupState'
import { cacheAndroidSnapshot, getAndroidPreferencePublicationRevision, getTauriInternals, invokeTauriCommand, isAndroidRuntime, isBrowserPreviewRuntime } from './lib/tauri'
import { teardownBrowserPreviewRuntime } from './lib/browserRuntime'
import { useAppUpdates } from './lib/useAppUpdates'
import { instantOptions, parseAssistantSettings, serializeAssistantSettings } from './lib/assistantSettings'
import type { AssistantSettings } from './lib/assistantSettings'
import { acceptsPartialTranscriptionEvent, parsePartialTranscriptionEvent } from './lib/partialTranscription'
import { parseCompletionEvent } from './lib/completionEvents'
import { createVoiceTelemetryRecorder } from './lib/voiceTelemetry'
import { transitionRuntimeStatus } from './state/runtimeMachine'
import type {
  BackendRuntimePhase,
  ChatMessage,
  PromptExecutionResult,
  ResponseProfile,
  RuntimeStatus,
  StartupState,
  UiTextSize,
  UiTheme,
  UserNotice,
} from './types/chat'
import './App.css'

const NOTICE_AUTO_DISMISS_MS = 4_000
const MAX_NOTICE_QUEUE_LENGTH = 3
const ASSISTANT_SETTINGS_WRITE_FAILURE_TITLE = 'Assistant settings failed'

type RuntimeDiagnosticKind =
  | 'frontend_notice'
  | 'runtime_control'
  | 'execution'
  | 'tts'
  | 'audio'
  | 'profile'

type CaptureContinuation = {
  readonly lifetime: () => boolean
  readonly inputRevision: number
  readonly contextRevision: number
  readonly retirement: Promise<void>
}

function useFrontendSessionRuntime(session: SessionAuthority) {
  useAuthorityView(session, 'runtime')
  const timingRef = session.operationRef('frontendTiming', () => new FrontendTiming())
  useLayoutEffect(() => { timingRef.current.startup('SHELL_DOM_COMMITTED') }, [timingRef])
  const supportsDesktopRuntime = !isAndroidRuntime && !isBrowserPreviewRuntime()
  const [desktopRuntimeReady, setDesktopRuntimeReady] = session.fieldState('desktopRuntimeReady')
  const desktopCaptureIdRef = session.operationRef<number | null>('desktopCaptureId', () => null)
  const desktopCaptureErrorRef = session.operationRef<string | null>('desktopCaptureError', () => null)
  const desktopAutoStartRef = session.operationRef('desktopAutoStart', () => false)
  const desktopAutoStartConsumedRef = session.operationRef('desktopAutoStartConsumed', () => false)
  const autoStopWriteRef = session.operationRef('autoStopWrite', () => 0)
  const autoStopPendingRef = session.operationRef<number | null>('autoStopPending', () => null)
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
  const changeAutoStopOnSilence = (enabled: boolean): void => {
    if (!supportsDesktopRuntime) { setAutoStopOnSilence(enabled); return }
    const revision = ++autoStopWriteRef.current
    autoStopPendingRef.current = revision
    const lifetime = session.captureLifetime()
    // Keep the last native-effective value until publication succeeds. Failed
    // or superseded commands cannot manufacture a preference or roll it back.
    void session.preferences.autoStop.enqueue(() => invokeTauriCommand('set_voice_auto_stop', { enabled })).then((value) => {
      if (!lifetime() || autoStopPendingRef.current !== revision) return
      if (typeof value !== 'boolean') throw new Error('Invalid native auto-stop response')
      setAutoStopOnSilence(value)
    }).catch(() => undefined).finally(() => {
      if (!lifetime() || autoStopPendingRef.current !== revision) return
      desktopObserverRef.current?.observer.invalidate()
      autoStopPendingRef.current = null
      void desktopObserverRef.current?.observer.refresh().catch(() => undefined)
    })
  }
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
  const desktopObserverRef = session.operationRef<{ observer: DesktopRuntimeObserver; ready: Promise<DesktopRuntimeSnapshot | null> } | null>('desktopObserver', () => null)
  const desktopSnapshotRef = session.operationRef<(snapshot: DesktopRuntimeSnapshot) => void>('desktopSnapshot', () => () => undefined)
  const desktopEventRef = session.operationRef<(event: DesktopRuntimeEvent) => void>('desktopEvent', () => () => undefined)
  const desktopCanonicalReadRef = session.operationRef('desktopCanonicalRead', () => 0)
  const promptProjectionRevisionRef = session.prompt.revision
  const liveAudioSourceRef = session.audio.liveSource
  const liveAudioStartAbortRef = session.audio.startAbort
  const audioInputDeviceIdRef = session.fieldRef('audioInputDeviceId')
  const liveAudioSessionIdRef = session.audio.inputRevision
  const captureRetirementRef = session.operationRef<{ captureId: number | null; promise: Promise<void> } | null>('captureRetirement', () => null)
  const profileCaptureContinuationRef = session.operationRef<CaptureContinuation | null>('profileCaptureContinuation', () => null)
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
  const micAutoStartedRef = session.operationRef('micAutoStarted', () => false)
  const micStartInFlightRef = session.operationRef('micStartInFlight', () => false)
  const runtimeStatusRef = session.fieldRef('runtimeStatus')
  const runtimeActivityRevisionRef = session.operationRef('runtimeRevision', () => 0)
  const snapshotRuntimeHydratedRef = session.operationRef('runtimeHydrated', () => false)
  const startupStateRef = session.fieldRef('startupState')
  const voiceTelemetryRef = session.operationRef<ReturnType<typeof createVoiceTelemetryRecorder> | null>('voiceTelemetry', () => null)
  const partialTranscriptionRef = session.operationRef<{
    sessionId: number
    revision: number
    active: boolean
  } | null>('partialTranscription', () => null)
  const partialTranscriptionAllowedRef = session.operationRef('partialAllowed', () => false)
  const completionRevisionRef = session.operationRef('completionRevision', () => 0)
  const latestTypedCompletionRef = session.operationRef<{ revision: number; lifecycle: number } | null>('typedCompletion', () => null)
  const completionLifecycleRef = session.operationRef('completionLifecycle', () => 0)
  const pendingVoiceCompletionRef = session.operationRef<{
    sessionId: number; revision: number; suffix: string | null; lifecycle: number; context: number; closed: boolean
  } | null>('pendingVoiceCompletion', () => null)
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
  const ttsPersistedRevisionRef = session.preferences.tts.persistedRevision
  const ttsPersistedValueRef = session.preferences.tts.persisted
  const ttsPersistedInitializedRef = session.preferences.tts.initialized
  const ttsWriteRevisionRef = session.preferences.tts.revision

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
      if (supportsDesktopRuntime) {
        // A native receipt is not a crashed renderer's typed command. Establish
        // observation before orphan recovery; unknown ownership fails closed.
        const observation = desktopObserverRef.current
        if (observation === null) return
        try {
          await observation.ready
          const root = await observation.observer.refresh()
          if (!appActiveRef.current || !isCurrent() || transcriptRevision !== transcriptRevisionRef.current) return
          if (root === null) return
          if (root.prompt?.requestId === latest.activeRequest.requestId &&
            root.prompt.executionId.sessionId === latest.activeRequest.executionId.sessionId &&
            root.prompt.executionId.generation === latest.activeRequest.executionId.generation) {
            desktopSnapshotRef.current(root)
            return
          }
          const queriedRequest = latest.activeRequest
          const executionId = queriedRequest.executionId
          const owner = await invokeTauriCommand('get_desktop_request_owner', {
            requestId: queriedRequest.requestId,
            executionId: { session_id: executionId.sessionId, generation: executionId.generation },
          })
          if (!appActiveRef.current || !isCurrent() || transcriptRevision !== transcriptRevisionRef.current) return
          if (owner === null) {
            const history = canonicalTranscriptRef.current
            const selectedSession = snapshotSessionIdRef.current
            const runtimeRevision = runtimeActivityRevisionRef.current
            const canonical = await loadAppSnapshot()
            if (!appActiveRef.current || !isCurrent() || transcriptRevision !== transcriptRevisionRef.current ||
              history !== canonicalTranscriptRef.current || selectedSession !== snapshotSessionIdRef.current) return
            const current = canonical.activeRequest
            if (current?.requestId === queriedRequest.requestId && current.executionId.sessionId === executionId.sessionId &&
              current.executionId.generation === executionId.generation) return

            // Null also means this exact execution retired while hydration was
            // in flight. Reconcile A; a successor is never a new cancel target.
            const turn = canonical.transcript.find((item) => item.requestId === queriedRequest.requestId &&
              item.executionId.sessionId === executionId.sessionId && item.executionId.generation === executionId.generation)
            const projection = promptRef.current
            if (projection?.nativeOwned && projection.id === queriedRequest.requestId &&
              projection.executionId?.sessionId === executionId.sessionId && projection.executionId.generation === executionId.generation) {
              session.prompt.finishNative(projection.id, { requestId: projection.id, runtimePhase: canonical.runtimePhase,
                outcome: turn === undefined ? 'cancelled' : 'completed', errorMessage: null,
                ...(turn === undefined ? {} : { canonicalTurn: turn }) })
              return
            }
            if (projection !== null && canonical.sessionId !== selectedSession) return
            // B already attached through its native receipt: add only A's
            // committed turn and preserve B's existing presentation resources.
            const transcript = projection === null ? canonical.transcript : appendCanonicalTurn(history, turn)
            canonicalTranscriptRef.current = transcript
            if (projection === null) snapshotSessionIdRef.current = canonical.sessionId
            setMessages((messages) => [
              ...replaceCommittedTranscript(messages, transcript),
              ...(projection === null ? [] : messages.filter((message) => message.id === projection.userId || message.id === projection.assistantId)),
            ])
            snapshotRuntimeHydratedRef.current = true
            if (projection === null) {
              if (runtimeRevision === runtimeActivityRevisionRef.current) publishRuntimeStatus(toRuntimeStatus(canonical.runtimePhase), false)
              setPromptActivity(null)
              setPromptState('idle')
              if (current !== null) await observation.observer.refresh().catch(() => undefined)
            }
            return
          }
          if (promptRef.current?.nativeOwned === true) return
          // Canonical admission can precede observation publication. Absence of
          // a projection is not renderer ownership; await its actual receipt.
          if (owner !== 'renderer') return
        } catch {
          if (!appActiveRef.current || !isCurrent() || transcriptRevision !== transcriptRevisionRef.current || promptRef.current?.nativeOwned === true) return
          setPromptState('reconciling')
          setPromptActivity({ text: 'Native session observation unavailable. Use New Chat to recover.', compact: false })
          return
        }
      }
      await invokeTauriCommand('cancel_prompt', {
        requestId: latest.activeRequest.requestId,
        ...(supportsDesktopRuntime ? { expectedExecutionId: {
          session_id: latest.activeRequest.executionId.sessionId,
          generation: latest.activeRequest.executionId.generation,
        } } : {}),
      }).catch(() => undefined)
      if (!appActiveRef.current || !isCurrent() || transcriptRevision !== transcriptRevisionRef.current) return
      try {
        const runtimeActivityRevision = runtimeActivityRevisionRef.current
        latest = await loadAppSnapshot()
        latestRuntimeActivityRevision = runtimeActivityRevision
      } catch {
        if (attempt < 2) await new Promise((resolve) => window.setTimeout(resolve, 250 * (attempt + 1)))
      }
      if (!appActiveRef.current || !isCurrent() || transcriptRevision !== transcriptRevisionRef.current) return
      if (supportsDesktopRuntime && promptRef.current?.nativeOwned === true) return
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
  }, [androidDraftPendingRef, appActiveRef, canonicalTranscriptRef, composerValueRef, desktopObserverRef, desktopSnapshotRef, persistAndroidDraftRef, promptRef, publishRuntimeStatus, runtimeActivityRevisionRef, session, setMessages, setPromptActivity, setPromptState, snapshotRuntimeHydratedRef, snapshotSessionIdRef, supportsDesktopRuntime, transcriptRevisionRef])

  const resetWakeConfidence = (): void => {
    setWakeConfidence(null)
  }

  useEffect(() => {
    if (supportsDesktopRuntime) return undefined
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
  }, [partialTranscriptionAllowedRef, partialTranscriptionRef, setPartialTranscript, setVoiceCompletionSuffix, supportsDesktopRuntime])

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
          const candidate = pendingVoiceCompletionRef.current
          if (candidate?.closed === true && candidate.sessionId === parsed.voiceSessionId) return
          const transcript = partialTranscriptionRef.current
          if (transcript?.sessionId === parsed.voiceSessionId) {
            if (!transcript.active || parsed.revision < transcript.revision) return
            if (parsed.revision === transcript.revision) {
              pendingVoiceCompletionRef.current = null
              setVoiceCompletionSuffix(parsed.suffix ?? '')
              return
            }
          }
          if (!supportsDesktopRuntime || resetPendingRef.current) return
          pendingVoiceCompletionRef.current = {
            sessionId: parsed.voiceSessionId!, revision: parsed.revision, suffix: parsed.suffix,
            lifecycle: completionLifecycleRef.current, context: transcriptRevisionRef.current, closed: false,
          }
        }
      } catch { return }
    }).then((cleanup) => active ? (unlisten = cleanup) : cleanup()).catch(() => undefined)
    return () => { active = false; unlisten?.() }
  }, [completionLifecycleRef, latestTypedCompletionRef, partialTranscriptionRef, pendingVoiceCompletionRef, resetPendingRef, setTypedCompletionSuffix, setVoiceCompletionSuffix, supportsDesktopRuntime, transcriptRevisionRef])

  const clearPartialTranscript = useCallback((): void => {
    partialTranscriptionAllowedRef.current = false
    if (partialTranscriptionRef.current !== null) {
      partialTranscriptionRef.current = { ...partialTranscriptionRef.current, active: false }
    } else if (pendingVoiceCompletionRef.current !== null) {
      const candidate = pendingVoiceCompletionRef.current
      partialTranscriptionRef.current = { sessionId: candidate.sessionId, revision: candidate.revision, active: false }
    }
    if (pendingVoiceCompletionRef.current !== null) {
      // Retain only the retired identity, not its suffix, so a repeated event
      // cannot masquerade as a new post-reset/endpoint candidate.
      pendingVoiceCompletionRef.current = { ...pendingVoiceCompletionRef.current, suffix: null, closed: true }
    }
    setPartialTranscript('')
    setVoiceCompletionSuffix('')
  }, [partialTranscriptionAllowedRef, partialTranscriptionRef, pendingVoiceCompletionRef, setPartialTranscript, setVoiceCompletionSuffix])

  const cancelPrompt = useCallback((): void => {
    if (isAndroidRuntime && androidVoiceActiveRef.current) androidVoiceStopRef.current?.()
    const cancellation = session.stopPromptPresentation()
    if (cancellation === null) return
    androidVoiceInputEpochRef.current += 1
    pendingAndroidVoiceRequestRef.current = null
    clearPartialTranscript()
    void invokeTauriCommand('cancel_prompt', { requestId: cancellation.requestId }).catch(cancellation.recover)
  }, [androidVoiceActiveRef, androidVoiceInputEpochRef, androidVoiceStopRef, clearPartialTranscript, pendingAndroidVoiceRequestRef, session])

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
      ? startupStateFromSnapshot(acceptedSnapshot)
      : applySnapshotControlState(
          startupStateRef.current,
          capabilities,
          acceptedSnapshot.preferences.responseProfile,
        )
    startupStateRef.current = controlledState
    setStartupState(controlledState)
    if (controlledState.kind === 'ready' && !controlledState.capabilities.some((capability) =>
      startupWarningRelevant(capability, isAndroidRuntime ? 'android' : 'desktop'))) {
      setNotices((current) => current.some((notice) => notice.title === 'Startup capabilities unavailable')
        ? current.filter((notice) => notice.title !== 'Startup capabilities unavailable') : current)
    }
    snapshotControlsFreshRef.current = true
    setSnapshotControlsFresh(true)
    snapshotControlsInvalidatedRef.current = false
    setSnapshotControlsInvalidated(false)
    if (isAndroidRuntime) queueMicrotask(() => drainAndroidVoiceRequestRef.current())
    return true
  }, [adoptSnapshotPreferences, drainAndroidVoiceRequestRef, setNotices, setSnapshotControlsFresh, setSnapshotControlsInvalidated, setSnapshotModelOptions, setStartupState, snapshotCapabilitiesRef, snapshotControlRequestRevisionRef, snapshotControlsFreshRef, snapshotControlsInvalidatedRef, snapshotHydrationRevisionRef, snapshotModelOptionsRef, snapshotResponseProfileRef, startupStateRef])

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
        const activeRequestMessage: ChatMessage[] = snapshot.activeRequest === null || supportsDesktopRuntime ? [] : [{
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
        if (supportsDesktopRuntime && snapshot.activeRequest === null) {
          const observation = desktopObserverRef.current
          if (observation !== null) void observation.ready.then(() => observation.observer.refresh()).catch(() => undefined)
        }
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
  }, [adoptAndroidConversation, adoptSnapshotControls, androidAssistantRef, androidDraftPendingRef, canonicalTranscriptRef, composerEditRevisionRef, composerValueRef, desktopObserverRef, persistAndroidDraftRef, promptRef, recoverOrphanedRequest, resetPendingRef, runtimeActivityRevisionRef, setMessages, setPromptActivity, setPromptState, setSnapshotControlsFresh, setSnapshotControlsInvalidated, setSnapshotHydrationState, setTtsEnabled, snapshotControlRequestRevisionRef, snapshotControlsFreshRef, snapshotControlsInvalidatedRef, snapshotHydrationStateRef, snapshotRuntimeHydratedRef, snapshotSessionIdRef, startupStateRef, supportsDesktopRuntime, synchronizeHydratedRuntimeStatus, timingRef, transcriptRevisionRef, ttsEnabledRef, ttsPersistedInitializedRef, ttsPersistedValueRef, ttsWriteRevisionRef])

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
    await session.preferences.textSize.mutate({
      next: nextTextSize,
      fallback: initialSnapshotPreferencesRef.current?.uiTextSize ?? previousTextSize,
      apply: setUiTextSize,
      save: async () => parseUiTextSize(await invokeTauriCommand('set_ui_text_size', { textSize: nextTextSize })),
      onError: (error) => addNotice({
        tone: 'error',
        title: 'Setting not saved',
        message: toDisplayErrorMessage(error),
      }),
    })
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
    setUiThemeWriteError(null)
    await session.preferences.theme.mutate({
      next: nextTheme,
      fallback: initialSnapshotPreferencesRef.current?.uiTheme ?? previousTheme,
      apply: setUiTheme,
      save: async () => parseUiTheme(await invokeTauriCommand('set_ui_theme', { theme: nextTheme })),
      onError: (error) => {
        const message = toDisplayErrorMessage(error)
        setUiThemeWriteError(message)
        addNotice({
          tone: 'error',
          title: 'Theme not saved',
          message,
        })
      },
    })
  }

  const changeUiTheme = (nextTheme: UiTheme): void => {
    if (nextTheme !== uiTheme) void persistUiTheme(nextTheme, uiTheme)
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
    if (publishedState.kind === 'ready' && !publishedState.capabilities.some((capability) =>
      startupWarningRelevant(capability, isAndroidRuntime ? 'android' : 'desktop'))) {
      setNotices((current) => current.some((notice) => notice.title === 'Startup capabilities unavailable')
        ? current.filter((notice) => notice.title !== 'Startup capabilities unavailable') : current)
    }
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
  }, [publishRuntimeStatus, setNotices, setStartupState, setTtsEnabled, snapshotCapabilitiesRef, snapshotHydrationRevisionRef, snapshotHydrationStateRef, snapshotResponseProfileRef, startupStateRef, synchronizeHydratedRuntimeStatus, ttsEnabledRef, ttsPersistedInitializedRef, ttsPersistedValueRef, ttsWriteRevisionRef])

  const startupLoadAllowed = !isAndroidRuntime || snapshotHydrationState !== 'pending'
  useEffect(() => {
    if (!startupLoadAllowed) return undefined
    let active = true
    let initialStartupCheck = true

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
        const isFirstStartupPass = initialStartupCheck
        initialStartupCheck = false
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
          const newlyUnavailableCapabilities = publishedState.capabilities.filter((capability) => {
            const { id, state } = capability
            if (!startupWarningRelevant(capability, isAndroidRuntime ? 'android' : 'desktop')) return false
            if (previousKind !== 'ready' || isFirstStartupPass) return true
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

        if (isAndroidRuntime && isStartupStateSettled(nextState)) return

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
    session.activate(() => {
      if (import.meta.env.VITE_VOXGOLEM_BENCHMARK === '1') reportAppBenchmarkStage('benchmark_lifetime_retire')
      timingRef.current.retireRequest()
      const activePrompt = promptRef.current
      if (activePrompt !== null) {
        if (activePrompt.renderFrameId !== null) window.cancelAnimationFrame(activePrompt.renderFrameId)
        activePrompt.renderFrameId = null
        activePrompt.renderFrameRevision += 1
        activePrompt.terminal = true
      }
      promptRef.current = null
      session.prompt.detachNative()
      partialTranscriptionAllowedRef.current = false
      partialTranscriptionRef.current = null
      pendingVoiceCompletionRef.current = null
      completionLifecycleRef.current += 1
      latestTypedCompletionRef.current = null
      micStartInFlightRef.current = false
      micAutoStartedRef.current = false
      liveAudioSessionIdRef.current += 1
      if (!supportsDesktopRuntime) liveAudioStartAbortRef.current?.abort()
      liveAudioStartAbortRef.current = null
      if (supportsDesktopRuntime) liveAudioSourceRef.current?.detach()
      else liveAudioSourceRef.current?.stop()
      liveAudioSourceRef.current = null
      if (isBrowserPreviewRuntime()) teardownBrowserPreviewRuntime()
    })
    return () => session.dispose()
  }, [completionLifecycleRef, latestTypedCompletionRef, liveAudioSessionIdRef, liveAudioSourceRef, liveAudioStartAbortRef, micAutoStartedRef, micStartInFlightRef, partialTranscriptionAllowedRef, partialTranscriptionRef, pendingVoiceCompletionRef, promptRef, session, supportsDesktopRuntime, timingRef])

  useEffect(() => {
    if (!supportsDesktopRuntime) return
    const tauri = getTauriInternals()
    if (tauri?.listen === undefined) return
    const observer = new DesktopRuntimeObserver({
      listen: (receive) => tauri.listen!('desktop-runtime-changed', (event) => receive(event.payload)),
      read: (sinceRevision) => tauri.invoke('read_desktop_runtime', { sinceRevision }),
      snapshot: (snapshot) => desktopSnapshotRef.current(snapshot),
      event: (event) => desktopEventRef.current(event),
      error: () => {
        setDesktopRuntimeReady(false)
        // Loss of observation neither retires native work nor sends Stop.
        if (appActiveRef.current && promptRef.current?.nativeOwned) {
          setPromptActivity({ text: 'Reconnecting to native response…', compact: true })
        }
      },
    })
    const ready = observer.start()
    desktopObserverRef.current = { observer, ready }
    void ready.catch(() => undefined)
    const reconnect = (): void => { void observer.refresh().catch(() => undefined) }
    window.addEventListener('focus', reconnect)
    window.addEventListener('online', reconnect)
    return () => {
      window.removeEventListener('focus', reconnect)
      window.removeEventListener('online', reconnect)
      observer.stop()
      desktopObserverRef.current = null
    }
  }, [appActiveRef, desktopEventRef, desktopObserverRef, desktopSnapshotRef, promptRef, setDesktopRuntimeReady, setPromptActivity, supportsDesktopRuntime])

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
    try {
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
    } finally {
      const intent = profileCaptureContinuationRef.current
      profileCaptureContinuationRef.current = null
      if (intent !== null && revision === instantSelectionRevisionRef.current) await resumeUserCapture(intent)
    }
  }


  const applyRuntimeStatus = (nextStatus: RuntimeStatus): RuntimeStatus => {
    const previousStatus = publishRuntimeStatus(nextStatus, true)
    if (supportsDesktopRuntime) return nextStatus

    if (nextStatus === previousStatus) {
      return previousStatus
    }

    if (nextStatus === 'listening' && previousStatus !== 'listening') {
      resetWakeConfidence()
      partialTranscriptionRef.current = null
      partialTranscriptionAllowedRef.current = true
    } else if (nextStatus !== 'listening') {
      partialTranscriptionAllowedRef.current = false
      resetWakeConfidence()
      clearPartialTranscript()
    }

    return nextStatus
  }

  const applyTransition = (
    previousStatus: RuntimeStatus,
    event: Parameters<typeof transitionRuntimeStatus>[1],
  ): RuntimeStatus => applyRuntimeStatus(transitionRuntimeStatus(previousStatus, event))

  const runPrompt = async (
    prompt: string,
    source: 'typed' | 'voice',
    nativeRequestId?: string,
    nativeProjection?: NativePromptProjection,
  ): Promise<void> => {
    const admission = nativeProjection === undefined
      ? session.admitPrompt(prompt, source, nativeRequestId, updateRuntimeBlocked)
      : session.attachNativePrompt()
    if (admission === null) return
    const admittedAt = performance.now()
    const appActiveRef = { get current() { return admission.isCurrent() } }
    const currentSettings = admission.settings
    const executingStatus = applyTransition(runtimeStatusRef.current, 'submit_prompt')
    const promptRuntimeActivityRevision = runtimeActivityRevisionRef.current
    const expectedCanonicalSessionId = nativeProjection?.executionId.sessionId ?? snapshotSessionIdRef.current
    const promptComposerRevision = composerEditRevisionRef.current
    const priorCanonicalExecutions = new Set(
      canonicalTranscriptRef.current.map((turn) => executionKey(turn)),
    )
    clearPartialTranscript()
    setResponseAnnouncement('')

    const requestId = nativeRequestId ?? `request-${Date.now()}-${Math.random().toString(36).slice(2)}`
    const projectionRevision = ++promptProjectionRevisionRef.current
    const userId = `user-${projectionRevision}-${requestId}`
    const assistantId = `assistant-${projectionRevision}-${requestId}`
    const requestTiming = timingRef.current.admit(assistantId, admittedAt)
    const promptProjection: PromptProjection = {
      nativeOwned: nativeProjection !== undefined,
      ...(nativeProjection === undefined ? {} : { executionId: nativeProjection.executionId }),
      id: requestId,
      userId,
      assistantId,
      text: '',
      error: null,
      terminal: false,
      presentationCancelled: false,
      cancelPending: false,
      stages: [
        { stage: 'instant', status: 'running' },
        ...(currentSettings.deepEnabled ? [{ stage: 'deep', status: 'queued' } as const] : []),
        ...(currentSettings.reviewEnabled ? [{ stage: 'review', status: 'queued' } as const] : []),
      ],
      priorVersions: [],
      sources: [],
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
        if (firstTextTimings !== undefined) session.flushPromptProjection(active)
        else session.schedulePromptProjection(active)
      }
      if (event.kind === 'correction') {
        cancelPromptProjectionFrame(active)
        if (active.text.trim().length > 0) {
          active.priorVersions = [...active.priorVersions, { id: `instant-${active.priorVersions.length + 1}`, text: active.text, label: 'Before correction' }]
        }
        active.text = event.text
        const timings = requestTiming.received(event.text)
        if (isAndroidRuntime && timings !== undefined) active.customDirectTimings = timings
        active.stages = active.stages.some((stage) => stage.stage === event.stage)
          ? active.stages.map((stage) => stage.stage === event.stage ? { ...stage, status: 'corrected', detail: event.correction } : stage)
          : [...active.stages, { stage: event.stage, status: 'corrected', detail: event.correction }]
        session.renderPromptProjection(active)
        setPromptActivity({ text: event.correction, compact: false })
      }
      if (event.kind === 'stage') {
        const entry = { stage: event.stage, status: event.status, ...(event.detail === undefined ? {} : { detail: event.detail }) }
        active.stages = active.stages.some((stage) => stage.stage === event.stage)
          ? active.stages.map((stage) => stage.stage === event.stage ? entry : stage)
          : [...active.stages, entry]
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
      result = nativeProjection === undefined
        ? await executePrompt(requestId, prompt, handleEvent, source)
        : await session.prompt.observeNative(nativeProjection, handleEvent, () => {
          cancelPromptProjectionFrame(promptProjection)
          promptProjection.text = ''
          promptProjection.error = null
          promptProjection.stages = [{ stage: 'instant', status: 'running' }]
          promptProjection.sources = []
          promptProjection.priorVersions = []
          delete promptProjection.thinkingStatus
          session.renderPromptProjection(promptProjection)
        })
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
      const acceptedCanonicalTurn = result.canonicalTurn !== undefined &&
        (nativeProjection === undefined || result.canonicalTurn.executionId.generation === nativeProjection.executionId.generation) && isExpectedCanonicalTurn(
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
            ...(nativeProjection === undefined || (result.outcome === 'completed' && acceptedCanonicalTurn === undefined) ? {} : {
              // The correlated terminal receipt plus canonical A-not-active
              // retires A even for cancellation/failure without a committed turn.
              // A successor is not a reason to wait for global idleness.
              acceptRetiredRequest: (snapshot: AppSnapshot) => snapshot.sessionId === expectedCanonicalSessionId &&
                !(snapshot.activeRequest?.requestId === requestId &&
                  snapshot.activeRequest.executionId.sessionId === nativeProjection.executionId.sessionId &&
                  snapshot.activeRequest.executionId.generation === nativeProjection.executionId.generation) &&
                (result.outcome !== 'completed' || snapshot.transcript.some((turn) =>
                  turn.requestId === requestId && turn.executionId.sessionId === nativeProjection.executionId.sessionId &&
                  turn.executionId.generation === nativeProjection.executionId.generation)),
            }),
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
            sources: [...reconciledTurn.sources],
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
        if (reconciledTurn === undefined) setMessages((current) => current.filter((message) => message.id !== active.assistantId))
        addNotice({
          tone: 'info',
          title: 'No response',
          message: 'No response was returned. Try again.',
        })
      }
      if (
        commandRejected &&
        snapshotHydrationStateRef.current !== 'succeeded' &&
        promptRuntimeActivityRevision === runtimeActivityRevisionRef.current
      ) {
        applyTransition(executingStatus, 'fail')
      }
    }
    if (supportsDesktopRuntime && appActiveRef.current && !resetPendingRef.current && promptRef.current === null) {
      // Native may have admitted its next request while this renderer was still
      // reconciling the previous terminal result. Reattach, never resubmit it.
      void desktopObserverRef.current?.observer.refresh().catch(() => undefined)
    }
  }

  const projectDesktopVoice = (voice: VoiceProjection, revision: number): void => {
    if (!appActiveRef.current || resetPendingRef.current) return
    desktopCaptureIdRef.current = voice.captureId
    if (!voice.autoStartAvailable) desktopAutoStartConsumedRef.current = true
    desktopAutoStartRef.current = voice.autoStartAvailable && !desktopAutoStartConsumedRef.current
    if (autoStopPendingRef.current === null) setAutoStopOnSilence(voice.autoStopOnSilence)
    if ((voice.state === 'stopped' || voice.state === 'error') && liveAudioSourceRef.current !== null) {
      liveAudioSourceRef.current.detach()
      liveAudioSourceRef.current = null
    }
    publishRuntimeStatus(toRuntimeStatus(voice.runtimePhase), true)
    setMicStarting(voice.state === 'starting')
    setMicActive(voice.state === 'active')
    setWakeConfidence(voice.wakeConfidence)
    const partial = voice.partialTranscript
    const previousPartial = partialTranscriptionRef.current
    partialTranscriptionAllowedRef.current = partial !== null
    partialTranscriptionRef.current = partial === null
      ? previousPartial === null ? null : { ...previousPartial, active: false }
      : { sessionId: partial.sessionId, revision: partial.revision, active: true }
    setPartialTranscript(partial?.text ?? '')
    if (partial === null || previousPartial === null || previousPartial.sessionId !== partial.sessionId || previousPartial.revision !== partial.revision) setVoiceCompletionSuffix('')
    const candidate = pendingVoiceCompletionRef.current
    if (candidate !== null && partial !== null) {
      const matchingSession = candidate.sessionId === partial.sessionId
      if (!candidate.closed && candidate.lifecycle === completionLifecycleRef.current && candidate.context === transcriptRevisionRef.current &&
        matchingSession && candidate.revision === partial.revision) {
        setVoiceCompletionSuffix(candidate.suffix ?? '')
      }
      // Do not consume a hinted successor while a newer native invalidation is
      // still waiting to be read. Revisions compare only within one session.
      if ((!matchingSession && desktopObserverRef.current?.observer.hasNewerInvalidation(revision) !== true) || (!candidate.closed && ((matchingSession && candidate.revision <= partial.revision) ||
        candidate.lifecycle !== completionLifecycleRef.current || candidate.context !== transcriptRevisionRef.current))) pendingVoiceCompletionRef.current = null
    } else if (candidate !== null && (voice.runtimePhase !== 'listening' || voice.state === 'error' || voice.state === 'stopped') &&
      desktopObserverRef.current?.observer.hasNewerInvalidation(revision) !== true) {
      if (partialTranscriptionRef.current === null) partialTranscriptionRef.current = { sessionId: candidate.sessionId, revision: candidate.revision, active: false }
      if (!candidate.closed) pendingVoiceCompletionRef.current = { ...candidate, suffix: null, closed: true }
    }
    const error = voice.error ?? (voice.state === 'error' ? 'Native microphone capture failed' : null)
    if (error !== null && error !== desktopCaptureErrorRef.current) {
      if (voice.state === 'error') {
        liveAudioSessionIdRef.current += 1
        profileCaptureContinuationRef.current = null
      }
      addNotice({ tone: 'error', title: 'Microphone unavailable', message: error })
    }
    desktopCaptureErrorRef.current = error
  }
  const projectDesktopSpeech = (speech: SpeechProjection): void => {
    if (!appActiveRef.current) return
    setTtsPlaying(speech.playing)
    setPendingTtsCommands(speech.pending ? 1 : 0)
  }
  const projectNativePrompt = (prompt: NativePromptProjection): void => {
    if (!appActiveRef.current || resetPendingRef.current || snapshotHydrationStateRef.current !== 'succeeded' || prompt.executionId.sessionId !== snapshotSessionIdRef.current) return
    if (canonicalTranscriptRef.current.some((turn) => turn.executionId.sessionId === prompt.executionId.sessionId && turn.executionId.generation === prompt.executionId.generation)) return
    const active = promptRef.current
    if (active !== null) {
      if (!active.nativeOwned || active.id !== prompt.requestId || active.terminal) return
      session.prompt.replaceNative(prompt)
      session.flushPromptProjection(active)
    } else {
      void runPrompt(prompt.prompt, 'voice', prompt.requestId, prompt)
    }
  }
  desktopSnapshotRef.current = (snapshot): void => {
    setDesktopRuntimeReady(true)
    projectDesktopVoice(snapshot.voice, snapshot.revision)
    projectDesktopSpeech(snapshot.speech)
    if (resetPendingRef.current || snapshotHydrationStateRef.current !== 'succeeded') return
    if (snapshot.prompt !== null) {
      projectNativePrompt(snapshot.prompt)
      if (promptRef.current === null || promptRef.current.id === snapshot.prompt.requestId) return
    }
    // The bounded native projection may already have retired while detached.
    // Recover committed history through its existing canonical parser/owner.
    const active = promptRef.current
    if (active !== null && !active.nativeOwned) return
    if (active === null && session.fieldRef('promptState').current !== 'idle') return
    const context = transcriptRevisionRef.current
    const lifetime = session.captureLifetime()
    const history = canonicalTranscriptRef.current
    const readRevision = ++desktopCanonicalReadRef.current
    void loadAppSnapshot().then((canonical) => {
      if (!lifetime() || readRevision !== desktopCanonicalReadRef.current || history !== canonicalTranscriptRef.current || resetPendingRef.current || context !== transcriptRevisionRef.current || promptRef.current !== active || canonical.sessionId !== snapshotSessionIdRef.current) return
      if (active !== null) {
        const turn = canonical.transcript.find((turn) => turn.requestId === active.id && turn.executionId.sessionId === active.executionId?.sessionId && turn.executionId.generation === active.executionId.generation)
        if (canonical.activeRequest?.requestId === active.id) return
        session.prompt.finishNative(active.id, { requestId: active.id, outcome: turn === undefined ? 'cancelled' : 'completed', errorMessage: null, runtimePhase: canonical.runtimePhase, ...(turn === undefined ? {} : { canonicalTurn: turn }) })
      } else if (canonical.activeRequest === null) {
        canonicalTranscriptRef.current = canonical.transcript
        setMessages((current) => replaceCommittedTranscript(current, canonical.transcript))
      }
    }).catch(() => undefined)
  }
  desktopEventRef.current = (event): void => {
    if (!appActiveRef.current || resetPendingRef.current) return
    switch (event.kind) {
      case 'voice': projectDesktopVoice(event.voice, event.revision); break
      case 'speech': projectDesktopSpeech(event.speech); break
      case 'prompt_started': projectNativePrompt(event.prompt); break
      case 'prompt_update': session.prompt.updateNative(event.requestId, event.update); break
      case 'prompt_finished': session.prompt.finishNative(event.requestId, event.result); break
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

  const stopLiveAudio = (): Promise<void> => {
    desktopAutoStartConsumedRef.current = true
    desktopAutoStartRef.current = false
    liveAudioSessionIdRef.current += 1
    liveAudioStartAbortRef.current?.abort()
    liveAudioStartAbortRef.current = null
    const source = liveAudioSourceRef.current
    const captureId = source?.captureId ?? desktopCaptureIdRef.current
    const pending = captureRetirementRef.current
    const retirement = pending !== null && (captureId === null || pending.captureId === captureId)
      ? pending.promise
      : source !== null ? source.stop()
        : supportsDesktopRuntime && captureId !== null
          ? invokeTauriCommand('stop_native_microphone', { captureId }).then(() => undefined)
          : Promise.resolve()
    if ((source !== null || captureId !== null || pending !== null) && pending?.promise !== retirement) {
      const operation = { captureId, promise: retirement }
      captureRetirementRef.current = operation
      void retirement.catch(() => undefined).finally(() => {
        if (captureRetirementRef.current === operation) captureRetirementRef.current = null
        if (appActiveRef.current) void desktopObserverRef.current?.observer.refresh().catch(() => undefined)
      })
    }
    desktopCaptureIdRef.current = null
    liveAudioSourceRef.current = null
    resetWakeConfidence()
    if (!supportsDesktopRuntime) {
      setMicStarting(false)
      setMicActive(false)
    }
    clearPartialTranscript()
    return retirement
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
    if (!supportsDesktopRuntime || !session.fieldRef('desktopRuntimeReady').current || session.fieldRef('micActive').current) return
    if (updateRuntimeBlocked || resetPendingRef.current || !voiceSnapshotControlsUsableNow() || !voiceInputReady(startupStateRef.current) || liveAudioSourceRef.current !== null || session.fieldRef('micStarting').current || micStartInFlightRef.current) {
      return
    }
    micStartInFlightRef.current = true
    desktopAutoStartConsumedRef.current = true
    desktopAutoStartRef.current = false
    const startLifetime = session.captureLifetime()
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
        onError: (error) => {
          if (!startLifetime()) return
          if (liveAudioSessionId === liveAudioSessionIdRef.current) reportLiveAudioError(error)
          else void desktopObserverRef.current?.observer.refresh().catch(() => undefined)
        },
        onStatus: (status) => {
          if (!startLifetime() || liveAudioSessionId !== liveAudioSessionIdRef.current) return
          recordRuntimeDiagnostic('audio', `Microphone startup: ${status}`)
        },
        onSelectedDeviceFallback: (attemptedDeviceId) => {
          if (
            !startLifetime() || liveAudioSessionId !== liveAudioSessionIdRef.current ||
            audioInputDeviceIdRef.current !== attemptedDeviceId
          ) {
            return
          }
          audioInputDeviceIdRef.current = null
          setAudioInputDeviceId(null)
          persistAudioInputDeviceId(null)
        },
      })

      if (!startLifetime()) {
        liveAudioSource.detach()
        return
      }

      if (
        liveAudioSessionId !== liveAudioSessionIdRef.current ||
        !voiceSnapshotControlsUsableNow() ||
        (startupStateRef.current.kind === 'ready' && !voiceInputReady(startupStateRef.current))
      ) {
        liveAudioSource.stop()
        return
      }

      liveAudioSourceRef.current = liveAudioSource
      if (!session.fieldRef('micStarting').current && !session.fieldRef('micActive').current) {
        // A terminal projection can precede the start command response.
        liveAudioSource.detach()
        liveAudioSourceRef.current = null
      }
      persistAudioInputDeviceId(audioInputDeviceIdRef.current)
    } catch (error) {
      if (!appActiveRef.current || liveAudioSessionId !== liveAudioSessionIdRef.current) {
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

  const resumeUserCapture = async (intent: CaptureContinuation): Promise<void> => {
    const current = (): boolean => intent.lifetime() && intent.contextRevision === transcriptRevisionRef.current &&
      intent.inputRevision === liveAudioSessionIdRef.current && !resetPendingRef.current && !isSwitchingResponseProfileRef.current
    try {
      await intent.retirement
      if (!current()) return
      const root = await desktopObserverRef.current?.observer.refresh()
      if (!current() || root?.voice.state !== 'stopped') return
      await startMic()
    } catch {
      // Retirement failure is not permission to start a second capture. Native
      // projection/error reporting remains authoritative for the existing one.
    }
  }

  const changeAudioInputDevice = (deviceId: string): void => {
    const selectedDeviceId = deviceId.length === 0 ? null : deviceId
    audioInputDeviceIdRef.current = selectedDeviceId
    setAudioInputDeviceId(selectedDeviceId)
    persistAudioInputDeviceId(selectedDeviceId)

    const shouldRestart = session.fieldRef('micActive').current || liveAudioSourceRef.current !== null
    const retirement = stopLiveAudio()
    if (shouldRestart) {
      void resumeUserCapture({ lifetime: session.captureLifetime(), inputRevision: liveAudioSessionIdRef.current,
        contextRevision: transcriptRevisionRef.current, retirement })
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
    clearPartialTranscript()
    setIsSwitchingResponseProfile(true)
    const requestedCapabilityId = profile === 'quality' ? 'local_quality' : 'local_fast'

    const priorIntent = profileCaptureContinuationRef.current
    const preservedIntent = priorIntent !== null && priorIntent.lifetime() &&
      priorIntent.inputRevision === liveAudioSessionIdRef.current && priorIntent.contextRevision === transcriptRevisionRef.current
    const hadLiveAudio = session.fieldRef('micActive').current || liveAudioSourceRef.current !== null
    if (hadLiveAudio) micAutoStartedRef.current = false
    const retirement = stopLiveAudio()
    profileCaptureContinuationRef.current = hadLiveAudio || preservedIntent ? {
      lifetime: preservedIntent ? priorIntent.lifetime : session.captureLifetime(),
      inputRevision: liveAudioSessionIdRef.current, contextRevision: transcriptRevisionRef.current, retirement,
    } : null


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
       await retirement
       if (!appActiveRef.current || resetPendingRef.current) return false
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
    const previousEnabled = ttsEnabledRef.current
    await session.preferences.tts.mutate({
      next: enabled,
      fallback: initialSnapshotPreferencesRef.current?.ttsEnabled ?? previousEnabled,
      apply: (value) => {
        ttsEnabledRef.current = value
        setTtsEnabled(value)
      },
      save: async () => {
        const payload = await invokeTauriCommand('set_tts_enabled', { enabled })
        return typeof payload === 'object' && payload !== null && 'enabled' in payload && typeof payload.enabled === 'boolean'
          ? payload.enabled
          : enabled
      },
      onError: (error) => {
        const message = toDisplayErrorMessage(error)
        addNotice({
          tone: 'error',
          title: 'TTS setting failed',
          message,
        })
        recordRuntimeDiagnostic('tts', `TTS toggle error: ${message}`)
      },
    })
  }

  useEffect(() => {
    if (supportsDesktopRuntime && (!desktopRuntimeReady || snapshotHydrationStateRef.current !== 'succeeded')) return
    if (!voiceSnapshotControlsUsable) {
      if (import.meta.env.VITE_VOXGOLEM_BENCHMARK === '1') reportAppBenchmarkStage('benchmark_controls_retire')
      micAutoStartedRef.current = false
      if (!supportsDesktopRuntime && (micActive || micStarting || micStartInFlightRef.current)) stopLiveAudio()
      return
    }
    if (startupState.kind === 'ready' && !voiceInputReady(startupState)) {
      if (supportsDesktopRuntime) return
      if (import.meta.env.VITE_VOXGOLEM_BENCHMARK === '1') reportAppBenchmarkStage('benchmark_capability_retire')
      micAutoStartedRef.current = false
      if (micActive || micStarting || micStartInFlightRef.current) stopLiveAudio()
      return
    }
    if (!voiceInputReady(startupState)) return
    if (supportsDesktopRuntime && !desktopAutoStartRef.current) return
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
  }, [desktopRuntimeReady, micActive, micStarting, resetPending, startupState, supportsDesktopRuntime, voiceSnapshotControlsUsable, updateRuntimeBlocked])

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
    desktopObserverRef.current?.observer.invalidate()
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
      if (supportsDesktopRuntime) void desktopObserverRef.current?.observer.refresh().catch(() => undefined)
      return
    }

    snapshotControlsFreshRef.current = false
    setSnapshotControlsFresh(false)
    snapshotControlsInvalidatedRef.current = true
    setSnapshotControlsInvalidated(true)

    if (activePrompt !== null && !activePrompt.terminal) {
      cancelPromptProjectionFrame(activePrompt)
      activePrompt.terminal = true
      if (activePrompt.nativeOwned) session.prompt.detachNative()
      setPromptState('stopping')
      try {
        await invokeTauriCommand('cancel_prompt', { requestId: activePrompt.id })
      } catch {
        // reset_session remains authoritative for backend prompt settlement.
      }
      if (!appActiveRef.current) return
    }
    try {
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
        const activeRequestMessage: ChatMessage[] = snapshot.activeRequest === null || supportsDesktopRuntime ? [] : [{
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
      if (supportsDesktopRuntime) void desktopObserverRef.current?.observer.refresh().catch(() => undefined)
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
    setAutoStopOnSilence: changeAutoStopOnSilence, setVoiceNotesOpen, setNoticeInteraction,
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

export type FrontendSessionRuntimeView = ReturnType<typeof useFrontendSessionRuntime>
export type FrontendSessionIntents = FrontendSessionRuntimeView['actions']

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

function toDisplayErrorMessage(error: unknown): string {
  if (error instanceof Error) {
    return error.message
  }

  if (typeof error === 'string') {
    return error
  }

  return String(error)
}

function toStartupCapabilities(capabilities: readonly Capability[]): readonly import('./types/chat').StartupCapability[] {
  return capabilities
    .filter((capability): capability is Capability & { readonly id: import('./types/chat').CapabilityId } =>
      CAPABILITY_IDS.includes(capability.id as import('./types/chat').CapabilityId))
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

function startupStateToRuntimeStatus(startupState: StartupState): RuntimeStatus {
  if (startupState.kind === 'error') {
    return 'error'
  }

  if (startupState.kind === 'loading') {
    return 'initializing'
  }

  return toRuntimeStatus(startupState.runtimePhase)
}

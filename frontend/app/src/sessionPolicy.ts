import { useSessionView } from './FrontendSession'
import { readUiTheme } from './ui/theme'
import type { FrontendSession } from './FrontendSession'
import type { FrontendSessionRuntimeView } from './FrontendSessionRuntime'
import type { AssistantCapabilities, AssistantOption, AssistantSettings } from './lib/assistantSettings'
import type { AndroidAssistantProjection, ModelOption } from './lib/appSnapshot'
import { isAndroidRuntime, isBrowserPreviewRuntime } from './lib/tauri'
import type { UpdateState } from './lib/useAppUpdates'
import type { CapabilityId, StartupCapability, StartupState, UiTextSize, UiTheme } from './types/chat'

/** Startup notices concern supported generation capabilities; speech has its own UI. */
export function startupWarningRelevant(capability: StartupCapability, platform: 'android' | 'desktop'): boolean {
  if (['wake_word', 'vad', 'parakeet', 'tts'].includes(capability.id)) return false
  if (platform === 'android' && capability.id !== 'custom_provider') return false
  return ['not_configured', 'unavailable', 'failed'].includes(capability.state)
}

export const UI_TEXT_SIZE_STEPS: readonly UiTextSize[] = ['small', 'medium', 'large', 'extra_large']
export const DEFAULT_UI_TEXT_SIZE: UiTextSize = 'medium'
export const DEFAULT_UI_THEME: UiTheme = 'dark'

export const UI_TEXT_SIZE_LABELS: Record<UiTextSize, string> = {
  small: 'Small',
  medium: 'Medium',
  large: 'Large',
  extra_large: 'Extra Large',
}

export const UI_TEXT_SIZE_PERCENTAGES: Record<UiTextSize, string> = {
  small: '90%',
  medium: '100%',
  large: '112.5%',
  extra_large: '125%',
}

export const instantMatchesResponseProfile = (state: StartupState, instant: AssistantSettings['instant']): boolean =>
  state.kind === 'ready' && (instant === 'local-fast' ? state.selectedResponseProfile === 'fast'
    : instant === 'local-quality' ? state.selectedResponseProfile === 'quality' : true)

export function generationIneligibility(state: {
  readonly startupState: StartupState
  readonly assistantSettings: Readonly<AssistantSettings>
  readonly assistantSettingsPending: boolean
  readonly snapshotControlsFresh: boolean
  readonly resetPending: boolean
  readonly isSwitchingResponseProfile: boolean
  readonly androidAssistant: AndroidAssistantProjection | null
  readonly unsavedResult?: import('./lib/appSnapshot').UnsavedResult | null
}, selectedAvailable: boolean, updateBlocked: boolean): string | null {
  if (state.unsavedResult != null) return 'answer-not-saved'
  if (state.startupState.kind !== 'ready') return 'startup-not-ready'
  if (!selectedAvailable) return 'provider-unavailable'
  if (!instantMatchesResponseProfile(state.startupState, state.assistantSettings.instant)) return 'profile-mismatch'
  if (updateBlocked) return 'update-blocked'
  if (state.assistantSettingsPending) return 'settings-pending'
  if (!state.snapshotControlsFresh) return 'controls-stale'
  if (state.resetPending) return 'reset-pending'
  if (state.isSwitchingResponseProfile) return 'profile-changing'
  if ((state.androidAssistant?.branchChoices.length ?? 0) > 0) return 'branch-choice-required'
  if (['saved_local_state_failed', 'draft_not_saved', 'storage_error'].includes(state.androidAssistant?.error ?? '')) return 'storage-recovery-required'
  return null
}

export function capabilityIsAvailable(state: StartupState, id: CapabilityId): boolean {
  return (state.kind === 'ready' || state.kind === 'warming_model') &&
    state.capabilities.some((capability) => capability.id === id && capability.state === 'available')
}

export function hasAvailableCapabilities(
  state: StartupState,
  ids: readonly CapabilityId[],
): boolean {
  return ids.every((id) => capabilityIsAvailable(state, id))
}

export const voiceInputReady = (state: StartupState): boolean =>
  state.kind === 'ready' &&
  state.voiceInputAvailable &&
  hasAvailableCapabilities(state, ['wake_word', 'vad', 'parakeet'])

export function capabilityLabel(id: CapabilityId): string {
  return id.split('_').map((part) => part[0]?.toUpperCase() + part.slice(1)).join(' ')
}

export function localInstantCanRetry(
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

export function parseUiTextSize(value: unknown): UiTextSize {
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

export function parseUiTheme(value: unknown): UiTheme {
  return readUiTheme(value) ?? DEFAULT_UI_THEME
}

export function appUpdateBlocksRuntime(state: UpdateState): boolean {
  return state.kind === 'snapshot' && (
    state.snapshot.phase === 'installing' ||
    state.snapshot.phase === 'restart_required'
  )
}

export function capabilityMap(state: StartupState): AssistantCapabilities {
  const available = (id: CapabilityId) => capabilityIsAvailable(state, id)
  return { localFast: available('local_fast'), localQuality: available('local_quality'),
    custom: available('custom_provider'), openCode: available('opencode'), qwenPrediction: available('qwen_prediction'),
    deep: available('deep'), review: available('review') }
}

export function applySnapshotModelOptions<T extends string>(fallback: readonly AssistantOption<T>[], snapshotOptions: readonly ModelOption[] | null): AssistantOption<T>[] {
  if (snapshotOptions === null) return [...fallback]
  return fallback.map((option) => {
    const native = snapshotOptions.find((candidate) => candidate.id === option.value)
    if (native === undefined) return { ...option, available: false, reason: 'Unavailable in the current app snapshot' }
    if (native.available) {
      const availableOption = { ...option }
      delete availableOption.reason
      return { ...availableOption, label: native.label, available: true }
    }
    return { ...option, label: native.label, available: false, reason: native.unavailableReason ?? option.reason ?? 'Unavailable' }
  })
}

export type SessionViewProps = {
  readonly session: FrontendSession
  readonly view: FrontendSessionRuntimeView
}

// Native/update/navigation adapters may change independently of session state.
// A state publication is consumed by the named boundary, not by all siblings.
export function sameExternalView(previous: SessionViewProps, next: SessionViewProps): boolean {
  const sameValues = (left: object, right: object) => Object.keys(left).every((key) =>
    Object.is(Reflect.get(left, key), Reflect.get(right, key)))
  return previous.session === next.session && previous.view.actions === next.view.actions &&
    sameValues(previous.view.navigation, next.view.navigation) &&
    sameValues(previous.view.appUpdates, next.view.appUpdates) &&
    Object.entries(previous.view.voice).every(([key, value]) =>
      typeof value === 'function' || Object.is(value, Reflect.get(next.view.voice, key)))
}

export function usePresentation(
  session: FrontendSession,
  view: SessionViewProps['view'],
  name: Parameters<FrontendSession['getViewSnapshot']>[0],
) {
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
    canToggleMic: !updateRuntimeBlocked && !resetPending && (micActive || (voiceControlsUsable && voiceInputReady(startupState) && !micStarting)),
    canToggleTts: startupState.kind === 'ready' && capabilityIsAvailable(startupState, 'tts') && !isSwitchingResponseProfile && !updateRuntimeBlocked,
    voiceInputUnavailableReason: startupState.kind === 'ready' ? startupState.voiceInputError ?? (startupState.capabilities.filter((capability) =>
      ['wake_word', 'vad', 'parakeet'].includes(capability.id) && capability.state !== 'available').map((capability) => capability.reason ?? `${capabilityLabel(capability.id)} unavailable`).join('; ') || 'required voice capabilities are unavailable') : 'voice input is still starting',
    updateInstallationDisabled: startupState.kind !== 'ready' || runtimeStatus !== 'sleeping' || promptState !== 'idle' || resetPending ||
      isSwitchingResponseProfile || assistantSettingsPending || micStarting || state.ttsPlaying || state.pendingTtsCommands > 0,
    selectedAudioInputDevice: state.audioInputDevices.find((device) => device.deviceId === state.audioInputDeviceId),
    canDecreaseTextSize: sizeIndex > 0, canIncreaseTextSize: sizeIndex < UI_TEXT_SIZE_STEPS.length - 1,
    customFailure: startupState.kind === 'ready' || startupState.kind === 'warming_model' ? startupState.capabilities.find((capability) =>
      capability.id === 'custom_provider' && (capability.state === 'failed' || capability.state === 'unavailable')) : undefined,
    headerBusy: androidVoiceActive || ['listening', 'processing', 'executing'].includes(runtimeStatus) || promptState !== 'idle' || resetPending || isSwitchingResponseProfile || startupState.kind === 'warming_model',
  }
}

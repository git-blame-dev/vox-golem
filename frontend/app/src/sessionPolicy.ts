import type { AssistantCapabilities, AssistantOption, AssistantSettings } from './lib/assistantSettings'
import type { AndroidAssistantProjection, ModelOption } from './lib/appSnapshot'
import type { CapabilityId, StartupState } from './types/chat'

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
}, selectedAvailable: boolean, updateBlocked: boolean): string | null {
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

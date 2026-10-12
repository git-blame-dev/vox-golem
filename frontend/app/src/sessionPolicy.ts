import type { AssistantCapabilities, AssistantOption, AssistantSettings } from './lib/assistantSettings'
import type { ModelOption } from './lib/appSnapshot'
import type { CapabilityId, StartupState } from './types/chat'

export const instantMatchesResponseProfile = (state: StartupState, instant: AssistantSettings['instant']): boolean =>
  state.kind === 'ready' && (instant === 'local-fast' ? state.selectedResponseProfile === 'fast'
    : instant === 'local-quality' ? state.selectedResponseProfile === 'quality' : true)

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
    if (option.available && native.available) {
      const availableOption = { ...option }
      delete availableOption.reason
      return { ...availableOption, label: native.label, available: true }
    }
    return { ...option, label: native.label, available: false, reason: native.unavailableReason ?? option.reason ?? 'Unavailable' }
  })
}

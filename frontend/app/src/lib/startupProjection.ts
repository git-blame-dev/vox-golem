import type { AppSnapshot } from './appSnapshot'
import type { CapabilityId, CueAssetPaths, StartupState } from '../types/chat'

// Pure shared projection: neither the transport nor the startup loader owns defaults.
export const DEFAULT_INITIAL_SILENCE_TIMEOUT_MS = 2_500
export const DEFAULT_SILENCE_TIMEOUT_MS = 750
export const DEFAULT_TTS_OUTPUT_GAIN_DB = 3
export const DEFAULT_CUE_ASSET_PATHS: CueAssetPaths = {
  startListening: 'resources/start-listening.wav',
  stopListening: 'resources/stop-listening.wav',
}
export const CAPABILITY_IDS: readonly CapabilityId[] = [
  'custom_provider', 'opencode', 'local_fast', 'local_quality', 'qwen_prediction',
  'wake_word', 'vad', 'parakeet', 'tts', 'deep', 'review',
]

export function startupStateFromSnapshot(snapshot: AppSnapshot): Extract<StartupState, { kind: 'ready' }> {
  const byId = new Map(snapshot.capabilities.map((item) => [item.id, item]))
  return {
    kind: 'ready',
    cueAssetPaths: DEFAULT_CUE_ASSET_PATHS,
    runtimePhase: snapshot.runtimePhase,
    voiceInputAvailable: false,
    voiceInputError: 'Desktop audio input is unavailable on Android; native dictation has separate availability',
    initialSilenceTimeoutMs: DEFAULT_INITIAL_SILENCE_TIMEOUT_MS,
    silenceTimeoutMs: 1_500,
    selectedResponseProfile: snapshot.preferences.responseProfile,
    supportedResponseProfiles: [],
    promptCancellationAvailable: true,
    ttsEnabled: snapshot.preferences.ttsEnabled,
    ttsOutputGainDb: DEFAULT_TTS_OUTPUT_GAIN_DB,
    capabilities: CAPABILITY_IDS.map((id) => {
      const existing = byId.get(id)
      return existing !== undefined
        ? { id, state: existing.state, reason: existing.reason, actualProvider: existing.actualProvider }
        : { id, state: 'not_configured', reason: 'Unavailable on Android zero-asset runtime', actualProvider: null }
    }),
  }
}

export function androidStartupWireState(snapshot: AppSnapshot): Record<string, unknown> {
  const state = startupStateFromSnapshot(snapshot)
  return {
    kind: state.kind,
    cue_asset_paths: { start_listening: state.cueAssetPaths.startListening, stop_listening: state.cueAssetPaths.stopListening },
    runtime_phase: state.runtimePhase,
    voice_input_available: state.voiceInputAvailable,
    voice_input_error: state.voiceInputError,
    initial_silence_timeout_ms: state.initialSilenceTimeoutMs,
    silence_timeout_ms: state.silenceTimeoutMs,
    selected_response_profile: state.selectedResponseProfile,
    supported_response_profiles: state.supportedResponseProfiles,
    prompt_cancellation_available: state.promptCancellationAvailable,
    tts_enabled: state.ttsEnabled,
    tts_output_gain_db: state.ttsOutputGainDb,
    capabilities: state.capabilities.map(({ actualProvider, ...capability }) => ({ ...capability, actual_provider: actualProvider })),
  }
}

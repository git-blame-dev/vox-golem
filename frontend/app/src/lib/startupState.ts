import { hasInjectedTauriInternals, invokeTauriCommand } from './tauri'
import type {
  BackendRuntimePhase,
  CueAssetPaths,
  ResponseProfile,
  ResponseProfileState,
  StartupState,
} from '../types/chat'

export const DEFAULT_SILENCE_TIMEOUT_MS = 1_500

export const DEFAULT_CUE_ASSET_PATHS: CueAssetPaths = {
  startListening: 'resources/start-listening.wav',
  stopListening: 'resources/stop-listening.wav',
}

export const DEFAULT_SUPPORTED_RESPONSE_PROFILES: readonly ResponseProfile[] = ['fast']
export const DEFAULT_SELECTED_RESPONSE_PROFILE: ResponseProfile = 'fast'

export function parseStartupState(payload: unknown): StartupState {
  if (!isRecord(payload)) {
    throw new Error('Startup payload must be an object')
  }

  if (payload['kind'] === 'warming_model') {
    const voiceInputAvailable = payload['voice_input_available']
    const voiceInputError = payload['voice_input_error']
    const silenceTimeoutMs = parseSilenceTimeoutMs(payload['silence_timeout_ms'])
    const message = payload['message']

    if (typeof voiceInputAvailable !== 'boolean') {
      throw new Error('Startup warming payload must include voice_input_available')
    }

    if (typeof voiceInputError !== 'string' && voiceInputError !== null) {
      throw new Error('Startup warming payload must include a string or null voice_input_error')
    }

    if (typeof message !== 'string' || message.length === 0) {
      throw new Error('Startup warming payload must include a message')
    }

    const responseProfileState = parseResponseProfileState(payload)

    return {
      kind: 'warming_model',
      cueAssetPaths: parseCueAssetPaths(payload['cue_asset_paths']),
      runtimePhase: parseRuntimePhase(payload['runtime_phase']),
      voiceInputAvailable,
      voiceInputError,
      silenceTimeoutMs,
      message,
      selectedResponseProfile: responseProfileState.selectedResponseProfile,
      supportedResponseProfiles: responseProfileState.supportedResponseProfiles,
    }
  }

  if (payload['kind'] === 'ready') {
    const voiceInputAvailable = payload['voice_input_available']
    const voiceInputError = payload['voice_input_error']
    const silenceTimeoutMs = parseSilenceTimeoutMs(payload['silence_timeout_ms'])

    if (typeof voiceInputAvailable !== 'boolean') {
      throw new Error('Startup ready payload must include voice_input_available')
    }

    if (typeof voiceInputError !== 'string' && voiceInputError !== null) {
      throw new Error('Startup ready payload must include a string or null voice_input_error')
    }

    const responseProfileState = parseResponseProfileState(payload)

    return {
      kind: 'ready',
      cueAssetPaths: parseCueAssetPaths(payload['cue_asset_paths']),
      runtimePhase: parseRuntimePhase(payload['runtime_phase']),
      voiceInputAvailable,
      voiceInputError,
      silenceTimeoutMs,
      selectedResponseProfile: responseProfileState.selectedResponseProfile,
      supportedResponseProfiles: responseProfileState.supportedResponseProfiles,
    }
  }

  if (payload['kind'] === 'error') {
    const message = payload['message']

    if (typeof message === 'string' && message.length > 0) {
      return {
        kind: 'error',
        message,
      }
    }

    throw new Error('Startup error payload must include a message')
  }

  throw new Error('Startup payload contains an unsupported kind')
}

export async function loadStartupState(): Promise<StartupState> {
  if (typeof window === 'undefined') {
    return buildDefaultStartupState()
  }

  try {
    const payload = await invokeTauriCommand('get_startup_state')
    return parseStartupState(payload)
  } catch (error) {
    console.error('[startup] failed to load startup state', {
      error,
      hasInjectedTauriInternals: hasInjectedTauriInternals(),
    })

    if (!hasInjectedTauriInternals()) {
      return buildDefaultStartupState()
    }

    const message = error instanceof Error ? error.message : 'Failed to load startup state'
    return {
      kind: 'error',
      message,
    }
  }
}

function buildDefaultStartupState(): StartupState {
  return {
    kind: 'ready',
    cueAssetPaths: DEFAULT_CUE_ASSET_PATHS,
    runtimePhase: 'sleeping',
    voiceInputAvailable: true,
    voiceInputError: null,
    silenceTimeoutMs: DEFAULT_SILENCE_TIMEOUT_MS,
    selectedResponseProfile: DEFAULT_SELECTED_RESPONSE_PROFILE,
    supportedResponseProfiles: DEFAULT_SUPPORTED_RESPONSE_PROFILES,
  }
}

export function parseResponseProfileState(payload: unknown): ResponseProfileState {
  if (!isRecord(payload)) {
    throw new Error('Response profile payload must be an object')
  }

  if (!Object.prototype.hasOwnProperty.call(payload, 'selected_response_profile')) {
    throw new Error('Startup payload must include selected_response_profile')
  }

  if (!Object.prototype.hasOwnProperty.call(payload, 'supported_response_profiles')) {
    throw new Error('Startup payload must include supported_response_profiles')
  }

  const selectedResponseProfile = parseResponseProfile(payload['selected_response_profile'])
  const supportedResponseProfiles = parseSupportedResponseProfiles(payload['supported_response_profiles'])

  if (!supportedResponseProfiles.includes(selectedResponseProfile)) {
    throw new Error('Selected response profile must be present in supported_response_profiles')
  }

  return {
    selectedResponseProfile,
    supportedResponseProfiles,
  }
}

export function isStartupStateSettled(state: StartupState): boolean {
  return state.kind === 'ready' || state.kind === 'error'
}

function parseRuntimePhase(payload: unknown): BackendRuntimePhase {
  if (
    payload === 'initializing' ||
    payload === 'sleeping' ||
    payload === 'listening' ||
    payload === 'processing' ||
    payload === 'executing' ||
    payload === 'error'
  ) {
    return payload
  }

  throw new Error('Startup ready payload must include a supported runtime phase')
}

function parseCueAssetPaths(payload: unknown): CueAssetPaths {
  if (!isRecord(payload)) {
    throw new Error('Startup ready payload must include cue asset paths')
  }

  const startListening = payload['start_listening']
  const stopListening = payload['stop_listening']

  if (typeof startListening !== 'string' || startListening.length === 0) {
    throw new Error('Startup ready payload must include `start_listening` cue path')
  }

  if (typeof stopListening !== 'string' || stopListening.length === 0) {
    throw new Error('Startup ready payload must include `stop_listening` cue path')
  }

  return {
    startListening,
    stopListening,
  }
}

function parseSilenceTimeoutMs(payload: unknown): number {
  if (typeof payload !== 'number' || !Number.isSafeInteger(payload) || payload <= 0) {
    throw new Error('Startup payload must include a positive integer `silence_timeout_ms`')
  }

  return payload
}

function parseResponseProfile(payload: unknown): ResponseProfile {
  if (payload === 'fast' || payload === 'quality') {
    return payload
  }

  throw new Error('Startup payload must include a supported selected_response_profile')
}

function parseSupportedResponseProfiles(payload: unknown): readonly ResponseProfile[] {
  if (!Array.isArray(payload)) {
    throw new Error('Startup payload must include supported_response_profiles')
  }

  const profiles = payload.map(parseResponseProfile)

  if (profiles.length === 0) {
    throw new Error('Startup payload must include at least one supported_response_profile')
  }

  return Array.from(new Set(profiles))
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null
}

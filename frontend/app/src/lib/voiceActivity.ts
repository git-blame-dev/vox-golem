import type { RuntimeStatus } from '../types/chat'

const SILENCE_TIMEOUT_MS = 2_500

export interface VoiceActivityState {
  readonly lastActivityMs: number | null
  readonly silenceMarked: boolean
}

export interface VoiceActivityUpdate {
  readonly state: VoiceActivityState
  readonly shouldMarkSilence: boolean
}

export function createVoiceActivityState(): VoiceActivityState {
  return {
    lastActivityMs: null,
    silenceMarked: false,
  }
}

export function syncVoiceActivityState(
  state: VoiceActivityState,
  runtimeStatus: RuntimeStatus,
  lastActivityMs: number | null,
): VoiceActivityState {
  if (runtimeStatus !== 'listening') {
    return createVoiceActivityState()
  }

  if (lastActivityMs === null) {
    return state
  }

  if (state.lastActivityMs === lastActivityMs) {
    return state
  }

  return {
    lastActivityMs,
    silenceMarked: false,
  }
}

export function updateVoiceActivityState(
  state: VoiceActivityState,
  nowMs: number,
): VoiceActivityUpdate {
  if (state.lastActivityMs === null || state.silenceMarked) {
    return {
      state,
      shouldMarkSilence: false,
    }
  }

  if (nowMs - state.lastActivityMs < SILENCE_TIMEOUT_MS) {
    return {
      state,
      shouldMarkSilence: false,
    }
  }

  return {
    state: {
      lastActivityMs: state.lastActivityMs,
      silenceMarked: true,
    },
    shouldMarkSilence: true,
  }
}

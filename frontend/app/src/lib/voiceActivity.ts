import type { RuntimeStatus } from '../types/chat'

export interface VoiceActivityState {
  readonly lastActivityMs: number | null
  readonly heardSpeech: boolean
  readonly silenceMarked: boolean
}

export interface VoiceActivityUpdate {
  readonly state: VoiceActivityState
  readonly shouldMarkSilence: boolean
}

export function createVoiceActivityState(): VoiceActivityState {
  return {
    lastActivityMs: null,
    heardSpeech: false,
    silenceMarked: false,
  }
}

export function syncVoiceActivityState(
  state: VoiceActivityState,
  runtimeStatus: RuntimeStatus,
  lastActivityMs: number | null,
  heardSpeech: boolean,
): VoiceActivityState {
  if (runtimeStatus !== 'listening') {
    return createVoiceActivityState()
  }

  if (lastActivityMs === null) {
    return state
  }

  if (state.lastActivityMs === lastActivityMs && state.heardSpeech === heardSpeech) {
    return state
  }

  return {
    lastActivityMs,
    heardSpeech,
    silenceMarked: false,
  }
}

export function updateVoiceActivityState(
  state: VoiceActivityState,
  nowMs: number,
  initialSilenceTimeoutMs: number,
  silenceTimeoutMs: number,
): VoiceActivityUpdate {
  if (state.lastActivityMs === null || state.silenceMarked) {
    return {
      state,
      shouldMarkSilence: false,
    }
  }

  if (nowMs - state.lastActivityMs < (state.heardSpeech ? silenceTimeoutMs : initialSilenceTimeoutMs)) {
    return {
      state,
      shouldMarkSilence: false,
    }
  }

  return {
    state: {
      lastActivityMs: state.lastActivityMs,
      heardSpeech: state.heardSpeech,
      silenceMarked: true,
    },
    shouldMarkSilence: true,
  }
}

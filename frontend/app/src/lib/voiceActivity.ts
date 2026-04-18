import type { RuntimeStatus } from '../types/chat'

const SPEECH_RMS_THRESHOLD = 0.015
const SPEECH_ACTIVITY_REFRESH_MS = 200
const SILENCE_TIMEOUT_MS = 1_200

export interface VoiceActivityState {
  readonly lastActivityMs: number | null
  readonly silenceMarked: boolean
  readonly speechActivityReported: boolean
}

export interface VoiceActivityUpdate {
  readonly state: VoiceActivityState
  readonly shouldRecordSpeechActivity: boolean
  readonly shouldMarkSilence: boolean
}

export function createVoiceActivityState(): VoiceActivityState {
  return {
    lastActivityMs: null,
    silenceMarked: false,
    speechActivityReported: false,
  }
}

export function syncVoiceActivityState(
  state: VoiceActivityState,
  runtimeStatus: RuntimeStatus,
  nowMs: number,
): VoiceActivityState {
  if (runtimeStatus !== 'listening') {
    return createVoiceActivityState()
  }

  if (state.lastActivityMs !== null) {
    return state
  }

  return {
    lastActivityMs: nowMs,
    silenceMarked: false,
    speechActivityReported: false,
  }
}

export function updateVoiceActivityState(
  state: VoiceActivityState,
  frame: readonly number[],
  nowMs: number,
): VoiceActivityUpdate {
  if (frameHasSpeech(frame)) {
    if (
      state.speechActivityReported &&
      state.lastActivityMs !== null &&
      !state.silenceMarked &&
      nowMs - state.lastActivityMs < SPEECH_ACTIVITY_REFRESH_MS
    ) {
      return {
        state,
        shouldRecordSpeechActivity: false,
        shouldMarkSilence: false,
      }
    }

    return {
      state: {
        lastActivityMs: nowMs,
        silenceMarked: false,
        speechActivityReported: true,
      },
      shouldRecordSpeechActivity: true,
      shouldMarkSilence: false,
    }
  }

  if (state.lastActivityMs === null || state.silenceMarked) {
    return {
      state,
      shouldRecordSpeechActivity: false,
      shouldMarkSilence: false,
    }
  }

  if (nowMs - state.lastActivityMs < SILENCE_TIMEOUT_MS) {
    return {
      state,
      shouldRecordSpeechActivity: false,
      shouldMarkSilence: false,
    }
  }

  return {
    state: {
      lastActivityMs: state.lastActivityMs,
      silenceMarked: true,
      speechActivityReported: state.speechActivityReported,
    },
    shouldRecordSpeechActivity: false,
    shouldMarkSilence: true,
  }
}

function frameHasSpeech(frame: readonly number[]): boolean {
  if (frame.length === 0) {
    return false
  }

  let sumSquares = 0

  for (const sample of frame) {
    sumSquares += sample * sample
  }

  const rms = Math.sqrt(sumSquares / frame.length)
  return rms >= SPEECH_RMS_THRESHOLD
}

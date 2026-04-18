import { getTauriInternals } from './tauri'
import type { BackendRuntimePhase } from '../types/chat'

export type RuntimeControlCommand =
  | 'begin_listening'
  | 'mark_silence'
  | 'mark_result_ready'
  | 'reset_session'

export interface RuntimeControlResult {
  readonly runtimePhase: BackendRuntimePhase
  readonly transcriptionReadySamples: number | null
  readonly capturingUtterance: boolean
  readonly prerollSamples: number
  readonly utteranceSamples: number
}

export interface AudioFrameStatus {
  readonly runtimePhase: BackendRuntimePhase
  readonly capturingUtterance: boolean
  readonly prerollSamples: number
  readonly utteranceSamples: number
}

export async function invokeRuntimeControl(
  command: RuntimeControlCommand,
): Promise<RuntimeControlResult | null> {
  if (typeof window === 'undefined') {
    return null
  }

  const tauriInternals = getTauriInternals()

  if (tauriInternals === null) {
    return null
  }

  const payload = await tauriInternals.invoke(command)
  return parseRuntimePhaseResponse(payload)
}

export async function ingestAudioFrame(
  frame: readonly number[],
): Promise<AudioFrameStatus | null> {
  if (typeof window === 'undefined') {
    return null
  }

  const tauriInternals = getTauriInternals()

  if (tauriInternals === null) {
    return null
  }

  const payload = await tauriInternals.invoke('ingest_audio_frame', { frame })
  return parseAudioFrameStatus(payload)
}

function parseRuntimePhaseResponse(payload: unknown): RuntimeControlResult {
  if (typeof payload !== 'object' || payload === null) {
    throw new Error('Runtime control payload must be an object')
  }

  const record = payload as Record<string, unknown>
  const runtimePhase = record['runtime_phase']
  const transcriptionReadySamples = record['transcription_ready_samples']

  if (
    runtimePhase === 'initializing' ||
    runtimePhase === 'sleeping' ||
    runtimePhase === 'listening' ||
    runtimePhase === 'processing' ||
    runtimePhase === 'executing' ||
    runtimePhase === 'result_ready' ||
    runtimePhase === 'error'
  ) {
    if (
      typeof transcriptionReadySamples !== 'number' &&
      transcriptionReadySamples !== null &&
      transcriptionReadySamples !== undefined
    ) {
      throw new Error('Runtime control payload must include a numeric or null transcription sample count')
    }

    const capturingUtterance = record['capturing_utterance']
    const prerollSamples = record['preroll_samples']
    const utteranceSamples = record['utterance_samples']

    if (typeof capturingUtterance !== 'boolean') {
      throw new Error('Runtime control payload must include capturing_utterance')
    }

    if (typeof prerollSamples !== 'number' || typeof utteranceSamples !== 'number') {
      throw new Error('Runtime control payload must include numeric sample counts')
    }

    return {
      runtimePhase,
      transcriptionReadySamples:
        typeof transcriptionReadySamples === 'number' ? transcriptionReadySamples : null,
      capturingUtterance,
      prerollSamples,
      utteranceSamples,
    }
  }

  throw new Error('Runtime control payload must include a supported runtime phase')
}

function parseAudioFrameStatus(payload: unknown): AudioFrameStatus {
  if (typeof payload !== 'object' || payload === null) {
    throw new Error('Audio frame status payload must be an object')
  }

  const record = payload as Record<string, unknown>
  const runtimePhase = parseBackendRuntimePhase(record['runtime_phase'])
  const capturingUtterance = record['capturing_utterance']
  const prerollSamples = record['preroll_samples']
  const utteranceSamples = record['utterance_samples']

  if (typeof capturingUtterance !== 'boolean') {
    throw new Error('Audio frame status payload must include capturing_utterance')
  }

  if (typeof prerollSamples !== 'number' || typeof utteranceSamples !== 'number') {
    throw new Error('Audio frame status payload must include numeric sample counts')
  }

  return {
    runtimePhase,
    capturingUtterance,
    prerollSamples,
    utteranceSamples,
  }
}

function parseBackendRuntimePhase(payload: unknown): BackendRuntimePhase {
  if (
    payload === 'initializing' ||
    payload === 'sleeping' ||
    payload === 'listening' ||
    payload === 'processing' ||
    payload === 'executing' ||
    payload === 'result_ready' ||
    payload === 'error'
  ) {
    return payload
  }

  throw new Error('Payload must include a supported runtime phase')
}

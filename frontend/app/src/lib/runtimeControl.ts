import { getTauriInternals } from './tauri'
import type { RuntimeControlResult, RuntimeControlTelemetry } from '../types/chat'

export type RuntimeControlCommand =
  | 'begin_listening'
  | 'record_speech_activity'
  | 'mark_silence'
  | 'reset_session'

export interface RuntimeControlArgs {
  readonly nowMs?: number
  readonly telemetryFrameId?: string
}

export interface IngestAudioFrameOptions {
  readonly telemetryFrameId?: string
}

export async function invokeRuntimeControl(
  command: RuntimeControlCommand,
  args?: RuntimeControlArgs,
): Promise<RuntimeControlResult | null> {
  if (typeof window === 'undefined') {
    return null
  }

  const tauriInternals = getTauriInternals()

  if (tauriInternals === null) {
    return null
  }

  const payload =
    args === undefined
      ? await tauriInternals.invoke(command)
      : await tauriInternals.invoke(command, args)
  return parseRuntimePhaseResponse(payload)
}

export async function ingestAudioFrame(
  frame: readonly number[],
  options: IngestAudioFrameOptions = {},
): Promise<RuntimeControlResult | null> {
  if (typeof window === 'undefined') {
    return null
  }

  const tauriInternals = getTauriInternals()

  if (tauriInternals === null) {
    return null
  }

  const payload =
    options.telemetryFrameId === undefined
      ? await tauriInternals.invoke('ingest_audio_frame', { frame })
      : await tauriInternals.invoke('ingest_audio_frame', {
          frame,
          telemetryFrameId: options.telemetryFrameId,
        })
  return parseRuntimePhaseResponse(payload)
}

function parseRuntimePhaseResponse(payload: unknown): RuntimeControlResult {
  if (typeof payload !== 'object' || payload === null) {
    throw new Error('Runtime control payload must be an object')
  }

  const record = payload as Record<string, unknown>
  const runtimePhase = record['runtime_phase']
  const transcriptionReadySamples = record['transcription_ready_samples']

  if (!('transcript_text' in record)) {
    throw new Error('Runtime control payload must include transcript_text')
  }

  const transcriptText = record['transcript_text']
  if (!('last_activity_ms' in record)) {
    throw new Error('Runtime control payload must include last_activity_ms')
  }

  const lastActivityMs = record['last_activity_ms']

  if (
    runtimePhase === 'initializing' ||
    runtimePhase === 'sleeping' ||
    runtimePhase === 'listening' ||
    runtimePhase === 'processing' ||
    runtimePhase === 'executing' ||
    runtimePhase === 'error'
  ) {
    if (
      typeof transcriptionReadySamples !== 'number' &&
      transcriptionReadySamples !== null &&
      transcriptionReadySamples !== undefined
    ) {
      throw new Error('Runtime control payload must include a numeric or null transcription sample count')
    }

    if (
      typeof transcriptText !== 'string' &&
      transcriptText !== null &&
      transcriptText !== undefined
    ) {
      throw new Error('Runtime control payload must include a string or null transcript_text')
    }

    if (
      typeof lastActivityMs !== 'number' &&
      lastActivityMs !== null
    ) {
      throw new Error('Runtime control payload must include a numeric or null last activity timestamp')
    }

    const capturingUtterance = record['capturing_utterance']
    const prerollSamples = record['preroll_samples']
    const utteranceSamples = record['utterance_samples']
    const telemetry = parseRuntimeControlTelemetry(record['telemetry'])

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
      transcriptText: typeof transcriptText === 'string' ? transcriptText : null,
      lastActivityMs: typeof lastActivityMs === 'number' ? lastActivityMs : null,
      capturingUtterance,
      prerollSamples,
      utteranceSamples,
      telemetry,
    }
  }

  throw new Error('Runtime control payload must include a supported runtime phase')
}

function parseRuntimeControlTelemetry(payload: unknown): RuntimeControlTelemetry | null {
  if (payload === undefined || payload === null) {
    return null
  }

  if (typeof payload !== 'object') {
    throw new Error('Runtime control payload telemetry must be an object when present')
  }

  const record = payload as Record<string, unknown>
  const frameId = parseTelemetryOptionalString(record['frame_id'], 'frame_id')
  const backendIngestStartedMs = parseTelemetryOptionalNumber(
    record['backend_ingest_started_ms'],
    'backend_ingest_started_ms',
  )
  const backendIngestCompletedMs = parseTelemetryOptionalNumber(
    record['backend_ingest_completed_ms'],
    'backend_ingest_completed_ms',
  )
  const wakeDetectedMs = parseTelemetryOptionalNumber(record['wake_detected_ms'], 'wake_detected_ms')
  const wakeConfidence = parseTelemetryOptionalNumber(record['wake_confidence'], 'wake_confidence')
  const transcriptionStartedMs = parseTelemetryOptionalNumber(
    record['transcription_started_ms'],
    'transcription_started_ms',
  )
  const transcriptionCompletedMs = parseTelemetryOptionalNumber(
    record['transcription_completed_ms'],
    'transcription_completed_ms',
  )

  return {
    frameId,
    backendIngestStartedMs,
    backendIngestCompletedMs,
    wakeDetectedMs,
    wakeConfidence,
    transcriptionStartedMs,
    transcriptionCompletedMs,
  }
}

function parseTelemetryOptionalNumber(value: unknown, fieldName: string): number | null {
  if (value === undefined || value === null) {
    return null
  }

  if (typeof value !== 'number') {
    throw new Error(`Runtime control telemetry field ${fieldName} must be numeric or null`)
  }

  return value
}

function parseTelemetryOptionalString(value: unknown, fieldName: string): string | null {
  if (value === undefined || value === null) {
    return null
  }

  if (typeof value !== 'string') {
    throw new Error(`Runtime control telemetry field ${fieldName} must be a string or null`)
  }

  return value
}

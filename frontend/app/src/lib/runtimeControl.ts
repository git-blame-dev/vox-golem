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

    return {
      runtimePhase,
      transcriptionReadySamples:
        typeof transcriptionReadySamples === 'number' ? transcriptionReadySamples : null,
    }
  }

  throw new Error('Runtime control payload must include a supported runtime phase')
}

import { getTauriInternals } from './tauri'
import type { BackendRuntimePhase } from '../types/chat'

export type RuntimeControlCommand =
  | 'begin_listening'
  | 'mark_silence'
  | 'mark_result_ready'
  | 'reset_session'

export async function invokeRuntimeControl(
  command: RuntimeControlCommand,
): Promise<BackendRuntimePhase | null> {
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

function parseRuntimePhaseResponse(payload: unknown): BackendRuntimePhase {
  if (typeof payload !== 'object' || payload === null) {
    throw new Error('Runtime control payload must be an object')
  }

  const runtimePhase = (payload as Record<string, unknown>)['runtime_phase']

  if (
    runtimePhase === 'initializing' ||
    runtimePhase === 'sleeping' ||
    runtimePhase === 'listening' ||
    runtimePhase === 'processing' ||
    runtimePhase === 'executing' ||
    runtimePhase === 'result_ready' ||
    runtimePhase === 'error'
  ) {
    return runtimePhase
  }

  throw new Error('Runtime control payload must include a supported runtime phase')
}

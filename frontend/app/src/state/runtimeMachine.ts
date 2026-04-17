import type { CueType } from '../lib/audioCues'
import type { RuntimeStatus } from '../types/chat'

export type RuntimeEvent =
  | 'begin_listening'
  | 'end_listening'
  | 'submit_prompt'
  | 'response_ready'
  | 'reset_to_sleeping'

export function transitionRuntimeStatus(
  current: RuntimeStatus,
  event: RuntimeEvent,
): RuntimeStatus {
  switch (event) {
    case 'begin_listening':
      return current === 'sleeping' || current === 'result_ready'
        ? 'listening'
        : current
    case 'end_listening':
      return current === 'listening' ? 'processing' : current
    case 'submit_prompt':
      return current === 'sleeping' || current === 'result_ready'
        ? 'executing'
        : current
    case 'response_ready':
      return current === 'processing' || current === 'executing'
        ? 'result_ready'
        : current
    case 'reset_to_sleeping':
      return current === 'result_ready' ? 'sleeping' : current
    default:
      return current
  }
}

export function cueForTransition(
  previous: RuntimeStatus,
  next: RuntimeStatus,
): CueType | null {
  if (previous === 'sleeping' && next === 'listening') {
    return 'start_listening'
  }

  if (previous === 'listening' && next === 'processing') {
    return 'stop_listening'
  }

  return null
}

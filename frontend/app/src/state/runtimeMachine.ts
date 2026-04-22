import type { CueType } from '../lib/audioCues'
import type { RuntimeStatus } from '../types/chat'

export type RuntimeEvent =
  | 'begin_listening'
  | 'end_listening'
  | 'submit_prompt'
  | 'response_ready'
  | 'fail'
  | 'recover_from_error'

export function transitionRuntimeStatus(
  current: RuntimeStatus,
  event: RuntimeEvent,
): RuntimeStatus {
  switch (event) {
    case 'begin_listening':
      return current === 'sleeping' ? 'listening' : current
    case 'end_listening':
      return current === 'listening' ? 'processing' : current
    case 'submit_prompt':
      return current === 'sleeping' || current === 'processing' ? 'executing' : current
    case 'response_ready':
      return current === 'processing' || current === 'executing'
        ? 'sleeping'
        : current
    case 'fail':
      return 'error'
    case 'recover_from_error':
      return current === 'error' ? 'sleeping' : current
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

  return null
}

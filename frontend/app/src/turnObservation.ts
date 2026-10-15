import type { TranscriptMessage } from './types/chat'

/** Only authority-supplied metadata is observable; text or stage badges cannot imply settlement. */
export function turnObservationAttributes(message: TranscriptMessage) {
  const observation = message.observation
  if (observation === undefined) return {}
  return {
    'data-turn-role': message.role,
    'data-request-id': observation.requestId,
    'data-turn-outcome': observation.outcome,
    'data-stage-status': observation.stageStatus,
    'data-content-kind': observation.contentKind,
  }
}

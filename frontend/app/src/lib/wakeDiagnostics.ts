export type WakePhase = 'sleeping' | 'listening' | 'processing' | 'executing' | 'initializing' | 'error' | 'stopped'
export type WakeMarker = 'wake_attempt' | 'false_wake'

export interface WakeDiagnosticsSnapshot {
  readonly schema_version: 1
  readonly enabled: boolean
  readonly model: string
  readonly feature_profile: 'normalized_f32'
  readonly threshold: number
  readonly sample_rate_hz: number
  readonly window_samples: number
  readonly hop_samples: number
  readonly warmup_remaining_samples: number
  readonly phase: WakePhase
  readonly dropped_events: number
  readonly events: readonly Record<string, unknown>[]
}

const phases: readonly WakePhase[] = ['sleeping', 'listening', 'processing', 'executing', 'initializing', 'error', 'stopped']

export function parseWakeDiagnosticsSnapshot(value: unknown): WakeDiagnosticsSnapshot {
  if (!isRecord(value)
    || value['schema_version'] !== 1
    || typeof value['enabled'] !== 'boolean'
    || typeof value['model'] !== 'string'
    || value['feature_profile'] !== 'normalized_f32'
    || !isFiniteNumber(value['threshold'])
    || !isFiniteNumber(value['sample_rate_hz'])
    || !isFiniteNumber(value['window_samples'])
    || !isFiniteNumber(value['hop_samples'])
    || !isFiniteNumber(value['warmup_remaining_samples'])
    || !phases.includes(value['phase'] as WakePhase)
    || !isFiniteNumber(value['dropped_events'])
    || !Array.isArray(value['events'])) {
    throw new Error('Invalid wake diagnostics response')
  }

  return {
    schema_version: 1,
    enabled: value['enabled'],
    model: (value['model'] as string).slice(0, 200),
    feature_profile: 'normalized_f32',
    threshold: value['threshold'],
    sample_rate_hz: value['sample_rate_hz'],
    window_samples: value['window_samples'],
    hop_samples: value['hop_samples'],
    warmup_remaining_samples: value['warmup_remaining_samples'],
    phase: value['phase'] as WakePhase,
    dropped_events: value['dropped_events'],
    events: (value['events'] as unknown[]).slice(-512).map(parseEvent).filter((event): event is Record<string, unknown> => event !== null),
  }
}

export function latestWakeScore(snapshot: WakeDiagnosticsSnapshot): Record<string, unknown> | null {
  for (let index = snapshot.events.length - 1; index >= 0; index -= 1) {
    const event = snapshot.events[index]
    if (event?.['kind'] === 'score') return event
  }
  return null
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function isFiniteNumber(value: unknown): value is number {
  return typeof value === 'number' && Number.isFinite(value)
}

function parseEvent(value: unknown): Record<string, unknown> | null {
  if (!isRecord(value) || !isFiniteNumber(value['sequence']) || !isFiniteNumber(value['at_ms'])) return null
  const common = { sequence: value['sequence'], at_ms: value['at_ms'], kind: value['kind'] }
  if (value['kind'] === 'score'
    && isFiniteNumber(value['score'])
    && isFiniteNumber(value['sample_position'])
    && (value['rms_dbfs'] === null || isFiniteNumber(value['rms_dbfs']))
    && isFiniteNumber(value['peak'])
    && isFiniteNumber(value['clipped_fraction'])
    && isFiniteNumber(value['inference_ms'])) {
    return { ...common, score: value['score'], sample_position: value['sample_position'], rms_dbfs: value['rms_dbfs'],
      peak: value['peak'], clipped_fraction: value['clipped_fraction'], inference_ms: value['inference_ms'] }
  }
  if (value['kind'] === 'phase' && phases.includes(value['phase'] as WakePhase)) {
    return { ...common, phase: value['phase'] }
  }
  if (value['kind'] === 'reset' && typeof value['reason'] === 'string') {
    return { ...common, reason: value['reason'].slice(0, 200) }
  }
  if (value['kind'] === 'marker' && (value['marker'] === 'wake_attempt' || value['marker'] === 'false_wake')) {
    return { ...common, marker: value['marker'] }
  }
  return null
}

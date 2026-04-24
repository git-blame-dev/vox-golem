const TELEMETRY_STORAGE_KEY = 'voxgolem.voiceTelemetry'
const MAX_EVENTS = 400

type VoiceTelemetryValue = string | number | boolean | null

export interface VoiceTelemetryEvent {
  readonly event: string
  readonly atMs: number
  readonly frameId: string | null
  readonly details: Readonly<Record<string, VoiceTelemetryValue>>
}

export interface VoiceTelemetrySnapshot {
  readonly enabled: boolean
  readonly droppedCount: number
  readonly events: readonly VoiceTelemetryEvent[]
}

export interface VoiceTelemetryRecorder {
  readonly enabled: boolean
  nextFrameId(nowMs: number): string | null
  record(
    event: string,
    options?: {
      readonly atMs?: number
      readonly frameId?: string | null
      readonly details?: Readonly<Record<string, VoiceTelemetryValue>>
    },
  ): void
  snapshot(): VoiceTelemetrySnapshot
  clear(): void
}

export function createVoiceTelemetryRecorder(): VoiceTelemetryRecorder {
  const enabled = resolveVoiceTelemetryEnabled()

  if (!enabled) {
    return {
      enabled,
      nextFrameId: () => null,
      record: () => undefined,
      snapshot: () => ({ enabled, droppedCount: 0, events: [] }),
      clear: () => undefined,
    }
  }

  const events: VoiceTelemetryEvent[] = []
  let droppedCount = 0
  let frameSequence = 0

  const recorder: VoiceTelemetryRecorder = {
    enabled,
    nextFrameId(nowMs: number): string {
      frameSequence += 1
      return `frame-${nowMs}-${frameSequence}`
    },
    record(event, options = {}): void {
      const nextEvent: VoiceTelemetryEvent = {
        event,
        atMs: options.atMs ?? Date.now(),
        frameId: options.frameId ?? null,
        details: options.details ?? {},
      }

      if (events.length >= MAX_EVENTS) {
        events.shift()
        droppedCount += 1
      }

      events.push(nextEvent)
    },
    snapshot(): VoiceTelemetrySnapshot {
      return {
        enabled,
        droppedCount,
        events: [...events],
      }
    },
    clear(): void {
      events.length = 0
      droppedCount = 0
    },
  }

  registerVoiceTelemetryWindowApi(recorder)
  return recorder
}

function resolveVoiceTelemetryEnabled(): boolean {
  if (typeof window === 'undefined') {
    return false
  }

  const queryEnabled = new URLSearchParams(window.location.search).get('voiceTelemetry') === '1'

  let storageEnabled = false
  try {
    storageEnabled = window.localStorage.getItem(TELEMETRY_STORAGE_KEY) === '1'
  } catch {
    storageEnabled = false
  }

  return queryEnabled || storageEnabled
}

function registerVoiceTelemetryWindowApi(recorder: VoiceTelemetryRecorder): void {
  if (typeof window === 'undefined') {
    return
  }

  window.__VOXGOLEM_VOICE_TELEMETRY__ = {
    snapshot: () => recorder.snapshot(),
    clear: () => recorder.clear(),
  }
}

declare global {
  interface Window {
    __VOXGOLEM_VOICE_TELEMETRY__?: {
      snapshot: () => VoiceTelemetrySnapshot
      clear: () => void
    }
  }
}

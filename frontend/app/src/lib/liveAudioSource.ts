import { getTauriInternals } from './tauri'

const MAX_PENDING_FRAMES = 8

export interface LiveAudioSource {
  stop(): void
}

export interface AudioInputDevice {
  readonly deviceId: string
  readonly label: string
}

export interface StartLiveAudioSourceOptions {
  readonly deviceId?: string
  readonly signal?: AbortSignal
  readonly onFrame: (frame: readonly number[]) => void | Promise<void>
  readonly onError: (error: unknown) => void
  readonly onStatus?: (status: string) => void
  readonly onSelectedDeviceFallback?: (attemptedDeviceId: string) => void
}

export async function listAudioInputDevices(): Promise<readonly AudioInputDevice[]> {
  const tauri = getTauriInternals()
  if (tauri === null) return []
  const payload = await tauri.invoke('list_audio_input_devices')
  if (!Array.isArray(payload)) throw new Error('Audio input device list must be an array')
  return payload.map(parseAudioInputDevice)
}

export async function startLiveAudioSource(
  options: StartLiveAudioSourceOptions,
): Promise<LiveAudioSource> {
  const tauri = getTauriInternals()
  if (tauri?.listen === undefined) {
    throw new Error('Native microphone capture is unavailable in this runtime')
  }

  const captureId = parseNativeCaptureId(
    await tauri.invoke('reserve_native_microphone_capture_id'),
  )
  let stopped = false
  let firstFrameReported = false
  let deliveringFrames = false
  let pendingFrames: number[][] = []
  let abortListener: (() => void) | undefined
  const listeners: { frame?: () => void; terminal?: () => void } = {}

  const stopCapture = (notifyNative: boolean): void => {
    if (stopped) return
    stopped = true
    pendingFrames = []
    if (abortListener !== undefined) {
      options.signal?.removeEventListener('abort', abortListener)
      abortListener = undefined
    }
    listeners.frame?.()
    listeners.terminal?.()
    if (notifyNative) {
      void tauri.invoke('stop_native_microphone', { captureId }).catch(() => undefined)
    }
  }
  const reportError = (error: unknown, notifyNative = true): void => {
    if (stopped) return
    stopCapture(notifyNative)
    options.onError(error)
  }
  const drainFrames = async (): Promise<void> => {
    if (deliveringFrames) return
    deliveringFrames = true
    try {
      while (!stopped) {
        const frame = pendingFrames.shift()
        if (frame === undefined) return
        await options.onFrame(frame)
      }
    } catch (error) {
      reportError(error)
    } finally {
      deliveringFrames = false
    }
  }

  listeners.frame = await tauri.listen('native-microphone-frame', (event) => {
    if (stopped) return
    let parsed: NativeMicrophoneFrame
    try {
      parsed = parseNativeMicrophoneFrame(event.payload)
    } catch (error) {
      reportError(error)
      return
    }
    if (parsed.captureId !== captureId) return
    if (!firstFrameReported) {
      firstFrameReported = true
      options.onStatus?.('first_frame')
    }
    if (pendingFrames.length >= MAX_PENDING_FRAMES) {
      reportError(new Error('Native microphone processing fell behind'))
      return
    }
    pendingFrames.push([...parsed.frame])
    void drainFrames()
  })
  try {
    listeners.terminal = await tauri.listen('native-microphone-terminal', (event) => {
      if (stopped) return
      let parsed: NativeMicrophoneTerminal
      try {
        parsed = parseNativeMicrophoneTerminal(event.payload)
      } catch (error) {
        reportError(error)
        return
      }
      if (parsed.captureId !== captureId) return
      reportError(new Error(parsed.message), false)
    })
  } catch (error) {
    stopCapture(false)
    throw error
  }

  abortListener = () => stopCapture(true)
  options.signal?.addEventListener('abort', abortListener, { once: true })
  if (options.signal?.aborted === true) {
    abortListener()
    throw new DOMException('Native microphone startup was cancelled', 'AbortError')
  }

  options.onStatus?.('starting_native_input')
  try {
    const payload = await tauri.invoke('start_native_microphone', {
      captureId,
      deviceId: options.deviceId ?? null,
    })
    const fellBackToDefault = parseCaptureStart(payload)
    if (fellBackToDefault && options.deviceId !== undefined) {
      options.onSelectedDeviceFallback?.(options.deviceId)
    }
    if (!stopped) options.onStatus?.('native_input_started')
  } catch (error) {
    stopCapture(false)
    throw error
  }

  return {
    stop(): void {
      stopCapture(true)
    },
  }
}

function parseAudioInputDevice(payload: unknown): AudioInputDevice {
  if (typeof payload !== 'object' || payload === null) {
    throw new Error('Audio input device must be an object')
  }
  const record = payload as Record<string, unknown>
  if (typeof record['device_id'] !== 'string' || typeof record['label'] !== 'string') {
    throw new Error('Audio input device fields are invalid')
  }
  return { deviceId: record['device_id'], label: record['label'] }
}

function parseNativeCaptureId(payload: unknown): number {
  if (typeof payload !== 'number' || !Number.isSafeInteger(payload) || payload <= 0) {
    throw new Error('Native microphone capture ID is invalid')
  }
  return payload
}

function parseCaptureStart(payload: unknown): boolean {
  if (typeof payload !== 'object' || payload === null) {
    throw new Error('Native microphone start response must be an object')
  }
  const fallback = (payload as Record<string, unknown>)['fell_back_to_default']
  if (typeof fallback !== 'boolean') {
    throw new Error('Native microphone start response is invalid')
  }
  return fallback
}

interface NativeMicrophoneFrame {
  readonly captureId: number
  readonly frame: readonly number[]
}

interface NativeMicrophoneTerminal {
  readonly captureId: number
  readonly message: string
}

function parseNativeMicrophoneFrame(payload: unknown): NativeMicrophoneFrame {
  if (typeof payload !== 'object' || payload === null) {
    throw new Error('Native microphone frame must be an object')
  }
  const record = payload as Record<string, unknown>
  const captureId = record['capture_id']
  const frame = record['frame']
  if (!Number.isSafeInteger(captureId) || typeof captureId !== 'number' || captureId <= 0) {
    throw new Error('Native microphone frame capture ID is invalid')
  }
  if (!Array.isArray(frame) || frame.length === 0 || frame.some((sample) => typeof sample !== 'number' || !Number.isFinite(sample))) {
    throw new Error('Native microphone frame samples are invalid')
  }
  return { captureId, frame: frame as number[] }
}

function parseNativeMicrophoneTerminal(payload: unknown): NativeMicrophoneTerminal {
  if (typeof payload !== 'object' || payload === null) {
    throw new Error('Native microphone terminal event must be an object')
  }
  const record = payload as Record<string, unknown>
  const captureId = record['capture_id']
  const message = record['message']
  if (!Number.isSafeInteger(captureId) || typeof captureId !== 'number' || captureId <= 0) {
    throw new Error('Native microphone terminal capture ID is invalid')
  }
  if (typeof message !== 'string' || message.trim().length === 0) {
    throw new Error('Native microphone terminal message is invalid')
  }
  return { captureId, message }
}

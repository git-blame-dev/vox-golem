import { getTauriInternals } from './tauri'

export interface LiveAudioSource {
  readonly captureId: number
  /** Resolves only after native physical retirement, not merely Stop admission. */
  stop(): Promise<void>
  /** Renderer observation is not the lifetime of the autonomous native capture. */
  detach(): void
}
export interface AudioInputDevice { readonly deviceId: string; readonly label: string }
export interface StartLiveAudioSourceOptions {
  readonly deviceId?: string
  readonly signal?: AbortSignal
  readonly onError: (error: unknown) => void
  readonly onStatus?: (status: string) => void
  readonly onSelectedDeviceFallback?: (attemptedDeviceId: string) => void
}
export async function listAudioInputDevices(): Promise<readonly AudioInputDevice[]> {
  const tauri = getTauriInternals()
  if (tauri === null) return []
  const payload = await tauri.invoke('list_audio_input_devices')
  if (!Array.isArray(payload)) throw new Error('Audio input device list must be an array')
  return payload.map((device: unknown) => {
    if (typeof device !== 'object' || device === null) throw new Error('Invalid audio input device')
    const record = device as Record<string, unknown>
    if (typeof record['device_id'] !== 'string' || typeof record['label'] !== 'string') throw new Error('Invalid audio input device')
    return { deviceId: record['device_id'], label: record['label'] }
  })
}
/** Coarse controls only: audio frames, terminal errors and progress remain native. */
export async function startLiveAudioSource(options: StartLiveAudioSourceOptions): Promise<LiveAudioSource> {
  const tauri = getTauriInternals()
  if (tauri === null) throw new Error('Native microphone capture is unavailable in this runtime')
  const captureId: unknown = await tauri.invoke('reserve_native_microphone_capture_id')
  if (typeof captureId !== 'number' || !Number.isSafeInteger(captureId) || captureId <= 0) throw new Error('Invalid native capture ID')
  let stopped = false
  let retirement: Promise<void> | null = null
  let detached = false
  const detach = (): void => { detached = true; options.signal?.removeEventListener('abort', stop) }
  const stop = (): Promise<void> => {
    if (retirement !== null) return retirement
    if (detached) return Promise.resolve()
    stopped = true
    options.signal?.removeEventListener('abort', stop)
    retirement = tauri.invoke('stop_native_microphone', { captureId }).then(() => undefined)
    void retirement.catch(options.onError)
    return retirement
  }
  options.signal?.addEventListener('abort', stop, { once: true })
  if (options.signal?.aborted === true) { stop(); throw new DOMException('Native microphone startup was cancelled', 'AbortError') }
  options.onStatus?.('starting_native_input')
  try {
    const payload: unknown = await tauri.invoke('start_native_microphone', { captureId, deviceId: options.deviceId ?? null })
    if (typeof payload !== 'object' || payload === null || typeof (payload as Record<string, unknown>)['fell_back_to_default'] !== 'boolean') throw new Error('Invalid native microphone start response')
    if (!stopped && !detached) {
      if ((payload as Record<string, unknown>)['fell_back_to_default'] === true && options.deviceId !== undefined) options.onSelectedDeviceFallback?.(options.deviceId)
      options.onStatus?.('native_input_started')
    }
  } catch (error) { options.signal?.removeEventListener('abort', stop); throw error }
  return { captureId, stop, detach }
}

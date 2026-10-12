const AUDIO_INPUT_DEVICE_STORAGE_KEY = 'voxgolem.audioInputDeviceId'

export function loadAudioInputDeviceId(): string | null {
  try { return window.localStorage.getItem(AUDIO_INPUT_DEVICE_STORAGE_KEY) } catch { return null }
}

export function persistAudioInputDeviceId(deviceId: string | null): void {
  try {
    if (deviceId === null) window.localStorage.removeItem(AUDIO_INPUT_DEVICE_STORAGE_KEY)
    else window.localStorage.setItem(AUDIO_INPUT_DEVICE_STORAGE_KEY, deviceId)
  } catch {
    // Browser storage is optional; the selected device still applies to this session.
  }
}

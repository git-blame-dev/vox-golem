import { useState } from 'react'
import { FrontendSession } from './FrontendSession'
import { FrontendSessionRuntime } from './FrontendSessionRuntime'
import { loadAudioInputDeviceId } from './lib/audioInputPreference'
import { getTauriInternals, isAndroidRuntime, isNativeTauriRuntime } from './lib/tauri'

export default function App({ session: suppliedSession }: { readonly session?: FrontendSession | undefined } = {}) {
  const [session] = useState(() => suppliedSession ?? new FrontendSession({
    snapshotAvailable: getTauriInternals() !== null,
    nativeSearch: isNativeTauriRuntime() || isAndroidRuntime,
    android: isAndroidRuntime,
    audioInputDeviceId: loadAudioInputDeviceId(),
  }))
  return <FrontendSessionRuntime session={session} />
}

import { useEffect, useState } from 'react'
import { invokeTauriCommand } from '../lib/tauri'
import { parseNewVoicePinStatus } from '../lib/newVoiceShortcut'
import type { NewVoicePinStatus } from '../lib/newVoiceShortcut'

export function NewVoiceShortcutSettings() {
  const [status, setStatus] = useState<NewVoicePinStatus | null>(null)
  const [pending, setPending] = useState(false)
  const [error, setError] = useState<string | null>(null)
  useEffect(() => {
    let active = true
    const refresh = (): void => {
      if (document.visibilityState !== 'visible') return
      void invokeTauriCommand('new_voice_shortcut_status').then((value) => { if (active) setStatus(parseNewVoicePinStatus(value)) }).catch(() => { if (active) setError('Native launcher shortcut status is unavailable') })
    }
    refresh(); const timer = window.setInterval(refresh, 1000)
    return () => { active = false; window.clearInterval(timer) }
  }, [])
  return <section className="settings-panel__assistant" aria-label="Ask shortcut">
    <strong>Ask</strong>
    <p>Ask opens voice input in the current conversation, preserving its context and existing draft. One finalized utterance automatically sends once, optionally speaks the reply, then stops. Stop cancels without sending; unconfirmed partial speech is not sent.</p>
    <button type="button" disabled={pending || status?.supported !== true} onClick={() => {
      setPending(true); setError(null)
      void invokeTauriCommand('pin_new_voice_shortcut').then((value) => setStatus(parseNewVoicePinStatus(value))).catch((failure: unknown) => setError(failure instanceof Error ? failure.message : 'Shortcut pin request failed')).finally(() => setPending(false))
    }}>Add Ask to Home screen</button>
    <p role="status">{error ?? (status?.pinned === true ? 'Ask shortcut is pinned' : status?.requested === true ? 'Pin requested — confirm in your launcher. If dismissed, you can try again.' : status?.supported === false ? 'This launcher does not support pinned shortcuts' : 'The launcher asks for your confirmation before pinning')}</p>
  </section>
}

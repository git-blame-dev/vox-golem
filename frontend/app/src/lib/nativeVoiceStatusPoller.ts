import { parseAndroidVoiceStatus } from './androidVoice'
import type { AndroidVoiceStatus } from './androidVoice'
import { invokeTauriCommand } from './tauri'

interface StatusPublication {
  readonly revision: () => number
  readonly publish: (status: AndroidVoiceStatus) => void
  readonly fail: () => void
  readonly hidden: () => void
}

/** One in-flight native status request, including across a StrictMode lifecycle replay. */
export class NativeVoiceStatusPoller {
  private active = false
  private lifecycle = 0
  private pending = false
  private queued = false
  private timer: number | null = null
  private publication: StatusPublication | null = null

  start(publication: StatusPublication): () => void {
    this.stop()
    this.active = true
    this.publication = publication
    const lifecycle = this.lifecycle
    window.addEventListener('focus', this.refresh)
    document.addEventListener('visibilitychange', this.refresh)
    this.refresh()
    return () => { if (this.lifecycle === lifecycle) this.stop() }
  }

  private stop(): void {
    this.active = false
    this.lifecycle += 1
    this.queued = false
    this.publication = null
    if (this.timer !== null) window.clearTimeout(this.timer)
    this.timer = null
    window.removeEventListener('focus', this.refresh)
    document.removeEventListener('visibilitychange', this.refresh)
  }

  private refresh = (): void => {
    if (!this.active) return
    if (this.timer !== null) window.clearTimeout(this.timer)
    this.timer = null
    if (document.visibilityState !== 'visible') {
      this.publication?.hidden()
      return
    }
    if (this.pending) { this.queued = true; return }
    this.pending = true
    this.queued = false
    const lifecycle = this.lifecycle
    const publication = this.publication!
    const revision = publication.revision()
    const isCurrent = () => this.active && lifecycle === this.lifecycle &&
      document.visibilityState === 'visible' && revision === publication.revision()
    void invokeTauriCommand('assistant_voice_status').then((value) => {
      if (isCurrent()) publication.publish(parseAndroidVoiceStatus(value))
    }).catch(() => {
      if (isCurrent()) publication.fail()
    }).finally(() => {
      this.pending = false
      if (!this.active || document.visibilityState !== 'visible') return
      if (this.queued || lifecycle !== this.lifecycle) this.refresh()
      else this.timer = window.setTimeout(this.refresh, 1000)
    })
  }
}

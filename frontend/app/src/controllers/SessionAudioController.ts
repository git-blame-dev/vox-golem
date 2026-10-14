import { createBrowserCuePlayer } from '../lib/audioCues'
import type { LiveAudioSource } from '../lib/liveAudioSource'

/** Typed resources are per authority, never inferred from global/native activity. */
export class SessionAudioController {
  readonly liveSource = { current: null as LiveAudioSource | null }
  readonly startAbort = { current: null as AbortController | null }
  readonly inputRevision = { current: 0 }
  readonly inFlightFrames = { current: 0 }
  readonly silenceInFlight = { current: null as number | null }
  readonly speechRevision = { current: 0 }
  readonly speechPlayback = { current: null as number | null }
  readonly cues = { current: createBrowserCuePlayer() }

  cancelSpeech(finish: (playbackId: number) => Promise<unknown>, publishIdle: () => void): void {
    this.speechRevision.current += 1
    const playbackId = this.speechPlayback.current
    if (playbackId === null) { publishIdle(); return }
    void finish(playbackId).catch(() => undefined).finally(() => {
      if (this.speechPlayback.current === playbackId) { this.speechPlayback.current = null; publishIdle() }
    })
  }
}

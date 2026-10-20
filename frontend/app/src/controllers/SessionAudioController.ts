import type { LiveAudioSource } from '../lib/liveAudioSource'

/** Renderer-owned capture controls only; native runtimes own speech and cues. */
export class SessionAudioController {
  readonly liveSource = { current: null as LiveAudioSource | null }
  readonly startAbort = { current: null as AbortController | null }
  readonly inputRevision = { current: 0 }
}

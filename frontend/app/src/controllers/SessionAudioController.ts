import { createBrowserCuePlayer } from '../lib/audioCues'
import type { LiveAudioSource } from '../lib/liveAudioSource'
import { firstNonEmptyCompletedLine, isValidTtsFirstLine } from '../lib/ttsValidation'

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

  async speakReply(
    text: string,
    options: {
      ttsGeneration: { current: number }
      ttsEnabledRef: { current: boolean }
      voiceTelemetry: { record: (event: string) => void }
      reservePlaybackId: () => Promise<unknown>
      finishPlayback: (playbackId: number) => Promise<unknown>
      speakTts: (text: string, playbackId: number) => Promise<unknown>
      parseRequestId: (value: unknown, context: string) => number
      toDisplayErrorMessage: (error: unknown) => string
      addNotice: (notice: { tone: 'error'; title: string; message: string }) => void
      recordDiagnostic: (category: string, message: string) => void
      setPlaying: (playing: boolean) => void
      setPendingCount: (updater: (count: number) => number) => void
    }
  ): Promise<void> {
    let commandPending = false
    let playbackId: number | null = null
    this.cancelSpeech(options.finishPlayback, () => {
      options.setPlaying(false)
    })
    const generation = options.ttsGeneration.current

    try {
      const speechText = firstNonEmptyCompletedLine(text) ?? ''
      if (!isValidTtsFirstLine(speechText)) return
      options.voiceTelemetry.record('tts_synthesis_requested')
      playbackId = options.parseRequestId(
        await options.reservePlaybackId(),
        'TTS playback',
      )
      if (generation !== options.ttsGeneration.current || !options.ttsEnabledRef.current) {
        await options.finishPlayback(playbackId)
        return
      }
      this.speechPlayback.current = playbackId
      options.setPlaying(true)
      commandPending = true
      options.setPendingCount((count) => count + 1)
      this.cues.current.stop()
      const payload = await options.speakTts(speechText, playbackId)
      options.voiceTelemetry.record('tts_playback_completed')
      if (generation !== options.ttsGeneration.current || !options.ttsEnabledRef.current) return

      if (
        typeof payload !== 'object' ||
        payload === null ||
        !('duration_ms' in payload) ||
        typeof (payload as { duration_ms?: unknown }).duration_ms !== 'number' ||
        ((payload as { duration_ms: number }).duration_ms) < 0 ||
        !Number.isFinite((payload as { duration_ms: number }).duration_ms)
      ) {
        throw new Error('TTS synthesis payload is invalid')
      }
    } catch (error) {
      if (generation !== options.ttsGeneration.current) return
      const message = options.toDisplayErrorMessage(error)

      options.addNotice({
        tone: 'error',
        title: 'TTS playback failed',
        message,
      })
      options.recordDiagnostic('tts', `TTS error: ${message}`)
    } finally {
      if (commandPending) {
        options.setPendingCount((count) => Math.max(0, count - 1))
      }
      if (playbackId !== null && this.speechPlayback.current === playbackId) {
        this.speechPlayback.current = null
        options.setPlaying(false)
      }
    }
  }
}

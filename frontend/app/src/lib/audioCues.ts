import type { CueAssetPaths } from '../types/chat'
import { convertTauriFileSrc, getTauriInternals } from './tauri'

export type CueType = 'start_listening' | 'stop_listening'

const WAV_DATA_URL_PREFIX = 'data:audio/wav;base64,'
const MAX_VOICE_CUE_URL_LENGTH = 2_100_000

export async function prepareVoiceCues(): Promise<CueAssetPaths> {
  const tauri = getTauriInternals()
  if (tauri === null) throw new Error('Local voice cues require the desktop runtime')
  const payload = await tauri.invoke('prepare_voice_cues')
  if (typeof payload !== 'object' || payload === null) {
    throw new Error('Local voice cue response is invalid')
  }
  const record = payload as Record<string, unknown>
  if (record['native_ready'] === true) {
    // Readiness markers only: native PCM and fallback assets never enter the WebView.
    return { startListening: 'native:start', stopListening: 'native:stop', stopListeningShort: 'native:stop-short' }
  }
  const startListening = parseVoiceCueUrl(record['start_listening'])
  const stopListening = parseVoiceCueUrl(record['stop_listening'])
  const stopListeningShort = parseVoiceCueUrl(record['stop_listening_short'])
  return { startListening, stopListening, stopListeningShort }
}

function parseVoiceCueUrl(value: unknown): string {
  if (
    typeof value !== 'string' ||
    !value.startsWith(WAV_DATA_URL_PREFIX) ||
    value.length <= WAV_DATA_URL_PREFIX.length ||
    value.length > MAX_VOICE_CUE_URL_LENGTH ||
    !/^[A-Za-z\d+/]+={0,2}$/.test(value.slice(WAV_DATA_URL_PREFIX.length))
  ) {
    throw new Error('Local voice cue URL is invalid')
  }
  return value
}

export interface CuePlayer {
  play(source: string): Promise<void | boolean>
  playNativeCue?(cueType: CueType | 'stop_listening_short'): Promise<boolean>
}

export interface BrowserCuePlayer extends CuePlayer {
  stop(): void
}

export interface NativeCuePlayer extends BrowserCuePlayer {
  retire(): void
}

export async function playCue(
  cueType: CueType,
  cueAssetPaths: CueAssetPaths,
  cuePlayer: CuePlayer = createBrowserCuePlayer(),
): Promise<boolean> {
  if (cuePlayer.playNativeCue !== undefined) return cuePlayer.playNativeCue(cueType)
  const configuredSource = resolveCueSource(cueType, cueAssetPaths)
  const source = resolveCuePlaybackSource(configuredSource)

  return (await cuePlayer.play(source)) !== false
}

export async function playCueWithFallback(
  cueType: CueType,
  preferred: CueAssetPaths,
  fallback: CueAssetPaths,
  cuePlayer: CuePlayer = createBrowserCuePlayer(),
): Promise<boolean> {
  if (cuePlayer.playNativeCue !== undefined) return cuePlayer.playNativeCue(cueType)
  try {
    return await playCue(cueType, preferred, cuePlayer)
  } catch (error) {
    if (resolveCueSource(cueType, preferred) === resolveCueSource(cueType, fallback)) throw error
    return playCue(cueType, fallback, cuePlayer)
  }
}

/** An activation receipt is distinct from completion (and from acoustic output).
 * Native completion retains its own update/lifetime lease after this resolves. */
export function createNativeCuePlayer(isActive: () => boolean): NativeCuePlayer {
  let generation = 0
  let ticket: number | null = null
  let retired = false
  const admitted = (): boolean => !retired && isActive()
  const invoke = async (command: string, args?: Record<string, unknown>): Promise<unknown> => {
    const tauri = getTauriInternals()
    if (tauri === null) throw new Error('Native cue runtime is unavailable')
    return tauri.invoke(command, args)
  }
  const retire = async (playbackId: number): Promise<void> => {
    await invoke('finish_cue_playback', { playbackId })
  }
  const stop = (): void => {
    generation += 1
    const previous = ticket
    ticket = null
    if (previous !== null) void retire(previous).catch(() => {
      console.error('[cue] native cancellation failed')
    })
  }
  return {
    stop,
    retire(): void {
      retired = true
      stop()
    },
    async play(): Promise<boolean> {
      throw new Error('Native cues require a typed cue request')
    },
    async playNativeCue(cueType): Promise<boolean> {
      if (!admitted()) return false
      const revision = ++generation
      const previous = ticket
      ticket = null
      if (previous !== null) await retire(previous)
      if (revision !== generation || !admitted()) return false
      const reserved = await invoke('reserve_cue_playback_id')
      if (reserved === null) return false // Speech owns output; cues may not interrupt it.
      if (typeof reserved !== 'number' || !Number.isSafeInteger(reserved) || reserved <= 0) {
        throw new Error('Native cue admission response is invalid')
      }
      if (revision !== generation || !admitted()) {
        await retire(reserved)
        return false
      }
      ticket = reserved
      try {
        const result = await invoke('activate_voice_cue', { playbackId: reserved, cueType })
        if (revision !== generation || !admitted()) return false
        if (typeof result === 'object' && result !== null &&
          (result as Record<string, unknown>)['playback_id'] === reserved &&
          (result as Record<string, unknown>)['stage'] === 'cancelled') {
          if (ticket === reserved) ticket = null
          return false
        }
        if (typeof result !== 'object' || result === null ||
          (result as Record<string, unknown>)['playback_id'] !== reserved ||
          (result as Record<string, unknown>)['stage'] !== 'activated') {
          throw new Error('Native cue activation response is invalid')
        }
        return true
      } catch {
        await retire(reserved)
        if (revision !== generation) return false
        if (ticket === reserved) ticket = null
        throw new Error('Native cue activation failed')
      }
    },
  }
}

/** One in-flight lifecycle IPC and one coalesced desired state, including remounts. */
export function createNativeAudioLifecycle(): {
  createCuePlayer(): NativeCuePlayer
  setActive(active: boolean): void
} {
  let desired = false
  let ready = false
  let revision = 0
  let running = false
  let closePending = false
  let closing = false
  let player: NativeCuePlayer | null = null
  return {
    createCuePlayer(): NativeCuePlayer {
      player?.retire()
      player = createNativeCuePlayer(() => desired && ready)
      return player
    },
    setActive(active): void {
      desired = active
      revision += 1
      // Synchronous admission closure and generation retirement precede any IPC.
      // Showing again never replays a hidden request, even if native close lags.
      if (!active) {
        ready = false
        player?.stop()
        if (!closing) closePending = true
      }
      if (running) return
      running = true
      void (async () => {
        let applied = -1
        try {
          while (applied !== revision || closePending) {
            applied = revision
            const requested = closePending ? false : desired
            closePending = false
            closing = !requested
            try {
              const tauri = getTauriInternals()
              if (tauri === null) throw new Error('Native audio runtime is unavailable')
              await tauri.invoke('set_native_audio_active', { active: requested })
              if (applied === revision && requested === desired && !closePending) ready = requested
            } catch {
              ready = false
              console.error('[cue] native audio lifecycle transition failed')
            } finally {
              closing = false
            }
            // A hide is a durable retirement boundary, not a latest-state hint.
            // Resume only after that close was acknowledged, even after rapid show.
            if (requested !== desired) applied = -1
          }
        } finally {
          running = false
        }
      })()
    },
  }
}

export function createBrowserCuePlayer(): BrowserCuePlayer {
  let activeAudio: HTMLAudioElement | null = null
  const stop = (): void => {
    const previous = activeAudio
    activeAudio = null
    if (previous !== null) {
      previous.onerror = null
      previous.onended = null
      if (typeof previous.pause === 'function') previous.pause()
    }
  }
  return {
    stop,
    async play(source: string): Promise<boolean> {
      if (typeof Audio !== 'function') {
        throw new Error('Audio playback is unavailable in this runtime')
      }

      stop()
      const element = new Audio(source)
      activeAudio = element
      element.onended = () => { if (activeAudio === element) activeAudio = null }

      element.onerror = () => {
        console.error('[cue] audio element reported an error', {
          networkState: element.networkState,
          readyState: element.readyState,
          code: element.error?.code ?? null,
        })
      }

      const playback = element.play()

      if (playback !== undefined) {
        try {
          await playback
        } catch {
          if (activeAudio !== element) return false

          console.error('[cue] audio playback failed', {
            networkState: element.networkState,
            readyState: element.readyState,
          })

          throw new Error('Audio playback failed')
        }
      }
      return true
    },
  }
}

function resolveCueSource(cueType: CueType, cueAssetPaths: CueAssetPaths): string {
  const source =
    cueType === 'start_listening'
      ? cueAssetPaths.startListening
      : cueAssetPaths.stopListening

  if (source.trim().length === 0) {
    const fieldName = cueType === 'start_listening' ? 'startListening' : 'stopListening'
    throw new Error(`Missing \`${fieldName}\` cue asset path`)
  }

  return source
}

function resolveCuePlaybackSource(source: string): string {
  if (isWindowsAbsolutePath(source) || source.startsWith('/')) {
    const convertedSource = convertTauriFileSrc(source)

    if (convertedSource !== null) {
      return convertedSource
    }
  }

  if (isWindowsAbsolutePath(source)) {
    return `file:///${encodeURI(source.replace(/\\/g, '/'))}`
  }

  if (source.startsWith('/')) {
    return `file://${encodeURI(source)}`
  }

  if (isUrlLikeSource(source)) {
    return source
  }

  return source
}

function isWindowsAbsolutePath(source: string): boolean {
  return /^[A-Za-z]:[\\/]/.test(source)
}

function isUrlLikeSource(source: string): boolean {
  return /^[A-Za-z][A-Za-z\d+.-]*:/.test(source)
}

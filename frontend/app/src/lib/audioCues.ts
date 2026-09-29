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
}

export interface BrowserCuePlayer extends CuePlayer {
  stop(): void
}

export async function playCue(
  cueType: CueType,
  cueAssetPaths: CueAssetPaths,
  cuePlayer: CuePlayer = createBrowserCuePlayer(),
): Promise<boolean> {
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
  try {
    return await playCue(cueType, preferred, cuePlayer)
  } catch (error) {
    if (resolveCueSource(cueType, preferred) === resolveCueSource(cueType, fallback)) throw error
    return playCue(cueType, fallback, cuePlayer)
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
          source,
          currentSrc: element.currentSrc,
          networkState: element.networkState,
          readyState: element.readyState,
          error: element.error
            ? {
                code: element.error.code,
                message: element.error.message,
              }
            : null,
        })
      }

      const playback = element.play()

      if (playback !== undefined) {
        try {
          await playback
        } catch (error) {
          if (activeAudio !== element) return false
          const message = error instanceof Error ? error.message : String(error)

          console.error('[cue] audio playback failed', {
            source,
            currentSrc: element.currentSrc,
            networkState: element.networkState,
            readyState: element.readyState,
            error,
          })

          throw new Error(`Audio playback failed for source ${source}: ${message}`)
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

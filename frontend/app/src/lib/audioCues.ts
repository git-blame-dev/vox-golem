import type { CueAssetPaths } from '../types/chat'
import { convertTauriFileSrc } from './tauri'

export type CueType = 'start_listening' | 'stop_listening'

export interface CuePlayer {
  play(source: string): Promise<void>
}

export async function playCue(
  cueType: CueType,
  cueAssetPaths: CueAssetPaths,
  cuePlayer: CuePlayer = createBrowserCuePlayer(),
): Promise<void> {
  const configuredSource = resolveCueSource(cueType, cueAssetPaths)
  const source = resolveCuePlaybackSource(configuredSource)

  await cuePlayer.play(source)
}

export function createBrowserCuePlayer(): CuePlayer {
  return {
    async play(source: string): Promise<void> {
      if (typeof Audio !== 'function') {
        throw new Error('Audio playback is unavailable in this runtime')
      }

      const element = new Audio(source)

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

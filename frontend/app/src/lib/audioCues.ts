import type { CueAssetPaths } from '../types/chat'

export type CueType = 'start_listening' | 'stop_listening'

export interface CuePlayer {
  play(source: string): Promise<void>
}

export async function playCue(
  cueType: CueType,
  cueAssetPaths: CueAssetPaths,
  cuePlayer: CuePlayer = createBrowserCuePlayer(),
): Promise<void> {
  const source = resolveCuePlaybackSource(resolveCueSource(cueType, cueAssetPaths))
  await cuePlayer.play(source)
}

export function createBrowserCuePlayer(): CuePlayer {
  return {
    async play(source: string): Promise<void> {
      if (typeof Audio !== 'function') {
        throw new Error('Audio playback is unavailable in this runtime')
      }

      const element = new Audio(source)
      const playback = element.play()

      if (playback !== undefined) {
        await playback
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
  if (isWindowsAbsolutePath(source)) {
    return `file:///${encodeURI(source.replace(/\\/g, '/'))}`
  }

  if (isUrlLikeSource(source)) {
    return source
  }

  if (source.startsWith('/')) {
    return `file://${encodeURI(source)}`
  }

  return source
}

function isWindowsAbsolutePath(source: string): boolean {
  return /^[A-Za-z]:[\\/]/.test(source)
}

function isUrlLikeSource(source: string): boolean {
  return /^[A-Za-z][A-Za-z\d+.-]*:/.test(source)
}

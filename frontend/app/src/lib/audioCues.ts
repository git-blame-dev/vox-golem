export type CueType = 'start_listening' | 'stop_listening'

export interface CueAssetPaths {
  readonly startListening: string
  readonly stopListening: string
}

export interface CuePlayer {
  play(source: string): Promise<void>
}

export async function playCue(
  cueType: CueType,
  cueAssetPaths: CueAssetPaths,
  cuePlayer: CuePlayer = createBrowserCuePlayer(),
): Promise<void> {
  const source = resolveCueSource(cueType, cueAssetPaths)
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

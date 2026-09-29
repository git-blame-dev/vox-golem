import { afterEach, describe, expect, it, vi } from 'vitest'
import { playCue, playCueWithFallback, prepareVoiceCues } from './audioCues'
import { DEFAULT_CUE_ASSET_PATHS } from './startupState'

const WINDOWS_CUE_PATH = 'C:\\bundle\\start-listening.wav'
const WINDOWS_CUE_FILE_URL = 'file:///C:/bundle/start-listening.wav'

afterEach(() => {
  Reflect.deleteProperty(window, '__TAURI_INTERNALS__')
})

describe('prepareVoiceCues', () => {
  it('loads the two prepared local WAV URLs without changing the embedded fallback', async () => {
    const start = 'data:audio/wav;base64,AAEC'
    const stop = 'data:audio/wav;base64,AQID'
    const shortStop = 'data:audio/wav;base64,BAUG'
    window.__TAURI_INTERNALS__ = {
      invoke: vi.fn(async () => ({ start_listening: start, stop_listening: stop, stop_listening_short: shortStop })),
    }

    const prepared = await prepareVoiceCues()

    expect(prepared).toEqual({ startListening: start, stopListening: stop, stopListeningShort: shortStop })
    expect(window.__TAURI_INTERNALS__.invoke).toHaveBeenCalledWith('prepare_voice_cues')
    expect(DEFAULT_CUE_ASSET_PATHS).toEqual({
      startListening: 'resources/start-listening.wav',
      stopListening: 'resources/stop-listening.wav',
    })
  })

  it('rejects incomplete or nonlocal cue responses', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: vi.fn(async () => ({
        start_listening: 'data:audio/wav;base64,AAEC',
        stop_listening: 'https://example.test/audio.wav',
      })),
    }

    await expect(prepareVoiceCues()).rejects.toThrow('Local voice cue URL is invalid')
  })
})

describe('playCue', () => {
  it('falls back to the embedded chime when prepared voice playback fails', async () => {
    const voice = 'data:audio/wav;base64,AAEC'
    const play = vi.fn(async (source: string) => {
      if (source === voice) throw new Error('voice playback failed')
    })

    await playCueWithFallback(
      'stop_listening',
      { ...DEFAULT_CUE_ASSET_PATHS, stopListening: voice },
      DEFAULT_CUE_ASSET_PATHS,
      { play },
    )

    expect(play.mock.calls.map(([source]) => source)).toEqual([
      voice,
      DEFAULT_CUE_ASSET_PATHS.stopListening,
    ])
  })
  it('plays the configured start-listening cue', async () => {
    const play = vi.fn(async () => undefined)

    await playCue('start_listening', DEFAULT_CUE_ASSET_PATHS, { play })

    expect(play).toHaveBeenCalledWith(DEFAULT_CUE_ASSET_PATHS.startListening)
  })

  it('fails clearly when a configured cue asset path is missing', async () => {
    const play = vi.fn(async () => undefined)

    await expect(
      playCue(
        'stop_listening',
        {
          startListening: DEFAULT_CUE_ASSET_PATHS.startListening,
          stopListening: '',
        },
        { play },
      ),
    ).rejects.toThrow('Missing `stopListening` cue asset path')
  })

  it('converts configured windows filesystem paths into file urls', async () => {
    const play = vi.fn(async () => undefined)

    await playCue(
      'start_listening',
      {
        startListening: WINDOWS_CUE_PATH,
        stopListening: DEFAULT_CUE_ASSET_PATHS.stopListening,
      },
      { play },
    )

    expect(play).toHaveBeenCalledWith(WINDOWS_CUE_FILE_URL)
  })

  it('uses tauri convertFileSrc when available for local files', async () => {
    const play = vi.fn(async () => undefined)

    window.__TAURI_INTERNALS__ = {
      invoke: async () => null,
      convertFileSrc: (filePath) => `asset://localhost/${encodeURIComponent(filePath)}`,
    }

    await playCue(
      'start_listening',
      {
        startListening: WINDOWS_CUE_PATH,
        stopListening: DEFAULT_CUE_ASSET_PATHS.stopListening,
      },
      { play },
    )

    expect(play).toHaveBeenCalledWith(
      `asset://localhost/${encodeURIComponent(WINDOWS_CUE_PATH)}`,
    )
  })

  it('preserves already-url cue sources', async () => {
    const play = vi.fn(async () => undefined)

    await playCue(
      'start_listening',
      {
        startListening: WINDOWS_CUE_FILE_URL,
        stopListening: DEFAULT_CUE_ASSET_PATHS.stopListening,
      },
      { play },
    )

    expect(play).toHaveBeenCalledWith(WINDOWS_CUE_FILE_URL)
  })
})

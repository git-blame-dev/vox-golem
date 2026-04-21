import { describe, expect, it, vi } from 'vitest'
import { playCue } from './audioCues'
import { DEFAULT_CUE_ASSET_PATHS } from './startupState'

const WINDOWS_CUE_PATH = 'C:\\bundle\\start-listening.wav'
const WINDOWS_CUE_FILE_URL = 'file:///C:/bundle/start-listening.wav'

describe('playCue', () => {
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

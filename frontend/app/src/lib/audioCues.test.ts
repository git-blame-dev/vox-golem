import { describe, expect, it, vi } from 'vitest'
import { playCue } from './audioCues'

const WINDOWS_CUE_PATH =
  'C:\\Users\\user\\AppData\\Roaming\\VoxGolem\\assets\\start-listening.mp3'
const WINDOWS_CUE_FILE_URL =
  'file:///C:/Users/user/AppData/Roaming/VoxGolem/assets/start-listening.mp3'

describe('playCue', () => {
  it('plays the configured start-listening cue', async () => {
    const play = vi.fn(async () => undefined)

    await playCue(
      'start_listening',
      {
        startListening: 'assets/start-listening.mp3',
        stopListening: 'assets/stop-listening.mp3',
      },
      { play },
    )

    expect(play).toHaveBeenCalledWith('assets/start-listening.mp3')
  })

  it('fails clearly when a configured cue asset path is missing', async () => {
    const play = vi.fn(async () => undefined)

    await expect(
      playCue(
        'stop_listening',
        {
          startListening: 'assets/start-listening.mp3',
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
        stopListening: 'assets/stop-listening.mp3',
      },
      { play },
    )

    expect(play).toHaveBeenCalledWith(WINDOWS_CUE_FILE_URL)
  })

  it('preserves already-url cue sources', async () => {
    const play = vi.fn(async () => undefined)

    await playCue(
      'start_listening',
      {
        startListening: WINDOWS_CUE_FILE_URL,
        stopListening: 'assets/stop-listening.mp3',
      },
      { play },
    )

    expect(play).toHaveBeenCalledWith(WINDOWS_CUE_FILE_URL)
  })
})

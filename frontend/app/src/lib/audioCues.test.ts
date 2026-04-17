import { describe, expect, it, vi } from 'vitest'
import { playCue } from './audioCues'

describe('playCue', () => {
  it('plays the configured start-listening cue', async () => {
    const play = vi.fn(async () => undefined)

    await playCue(
      'start_listening',
      {
        startListening: '/assets/start-listening.mp3',
        stopListening: '/assets/stop-listening.mp3',
      },
      { play },
    )

    expect(play).toHaveBeenCalledWith('/assets/start-listening.mp3')
  })

  it('fails clearly when a configured cue asset path is missing', async () => {
    const play = vi.fn(async () => undefined)

    await expect(
      playCue(
        'stop_listening',
        {
          startListening: '/assets/start-listening.mp3',
          stopListening: '',
        },
        { play },
      ),
    ).rejects.toThrow('Missing `stopListening` cue asset path')
  })
})

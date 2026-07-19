import { describe, expect, it } from 'vitest'
import { getInitialMessages } from './appShell'

describe('getInitialMessages', () => {
  it('returns the initialized shell message', () => {
    expect(getInitialMessages()).toEqual([
      {
        id: 'system-intro',
        role: 'system',
        content:
          'VoxGolem shell initialized. Voice pipeline wiring starts in the next phase.',
      },
    ])
  })
})

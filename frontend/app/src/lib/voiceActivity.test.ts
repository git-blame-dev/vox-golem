import { describe, expect, it } from 'vitest'
import {
  createVoiceActivityState,
  syncVoiceActivityState,
  updateVoiceActivityState,
} from './voiceActivity'

describe('voiceActivity', () => {
  it('resets activity state outside listening', () => {
    expect(
      syncVoiceActivityState(
        { lastActivityMs: 250, silenceMarked: true },
        'processing',
        500,
      ),
    ).toEqual(createVoiceActivityState())
  })

  it('tracks backend last-activity timestamps while listening', () => {
    expect(syncVoiceActivityState(createVoiceActivityState(), 'listening', 100)).toEqual({
      lastActivityMs: 100,
      silenceMarked: false,
    })

    expect(
      syncVoiceActivityState(
        { lastActivityMs: 100, silenceMarked: true },
        'listening',
        250,
      ),
    ).toEqual({
      lastActivityMs: 250,
      silenceMarked: false,
    })
  })

  it('ignores missing backend activity timestamps while listening', () => {
    const state = { lastActivityMs: 100, silenceMarked: false }

    expect(syncVoiceActivityState(state, 'listening', null)).toEqual(state)
  })

  it('requests silence only after sustained inactivity', () => {
    const listeningState = { lastActivityMs: 100, silenceMarked: false }

    expect(updateVoiceActivityState(listeningState, 1_000)).toEqual({
      state: listeningState,
      shouldMarkSilence: false,
    })

    expect(updateVoiceActivityState(listeningState, 1_300)).toEqual({
      state: {
        lastActivityMs: 100,
        silenceMarked: true,
      },
      shouldMarkSilence: true,
    })
  })
})

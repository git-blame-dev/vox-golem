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
        { lastActivityMs: 250, heardSpeech: true, silenceMarked: true },
        'processing',
        500,
        true,
      ),
    ).toEqual(createVoiceActivityState())
  })

  it('tracks backend last-activity timestamps while listening', () => {
    expect(syncVoiceActivityState(createVoiceActivityState(), 'listening', 100, false)).toEqual({
      lastActivityMs: 100,
      heardSpeech: false,
      silenceMarked: false,
    })

    expect(
      syncVoiceActivityState(
        { lastActivityMs: 100, heardSpeech: false, silenceMarked: true },
        'listening',
        250,
        true,
      ),
    ).toEqual({
      lastActivityMs: 250,
      heardSpeech: true,
      silenceMarked: false,
    })
  })

  it('ignores missing backend activity timestamps while listening', () => {
    const state = { lastActivityMs: 100, heardSpeech: false, silenceMarked: false }

    expect(syncVoiceActivityState(state, 'listening', null, false)).toEqual(state)
  })

  it('waits 2.5 seconds for the first VAD speech, then 0.75 seconds after the last speech', () => {
    const waiting = { lastActivityMs: 100, heardSpeech: false, silenceMarked: false }

    expect(updateVoiceActivityState(waiting, 2_599, 2_500, 750)).toEqual({
      state: waiting,
      shouldMarkSilence: false,
    })

    expect(updateVoiceActivityState(waiting, 2_600, 2_500, 750)).toEqual({
      state: {
        lastActivityMs: 100,
        heardSpeech: false,
        silenceMarked: true,
      },
      shouldMarkSilence: true,
    })

    const speaking = syncVoiceActivityState(waiting, 'listening', 400, true)
    expect(updateVoiceActivityState(speaking, 1_149, 2_500, 750).shouldMarkSilence).toBe(false)
    expect(updateVoiceActivityState(speaking, 1_150, 2_500, 750).shouldMarkSilence).toBe(true)
    const continued = syncVoiceActivityState(speaking, 'listening', 900, true)
    expect(updateVoiceActivityState(continued, 1_150, 2_500, 750).shouldMarkSilence).toBe(false)
    expect(updateVoiceActivityState(continued, 1_650, 2_500, 750).shouldMarkSilence).toBe(true)
  })

  it('observes first speech even when its timestamp equals the wake timestamp', () => {
    const waiting = { lastActivityMs: 100, heardSpeech: false, silenceMarked: true }
    expect(syncVoiceActivityState(waiting, 'listening', 100, true)).toEqual({
      lastActivityMs: 100,
      heardSpeech: true,
      silenceMarked: false,
    })
  })
})

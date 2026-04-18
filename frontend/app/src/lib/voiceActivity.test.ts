import { describe, expect, it } from 'vitest'
import {
  createVoiceActivityState,
  syncVoiceActivityState,
  updateVoiceActivityState,
} from './voiceActivity'

describe('voiceActivity', () => {
  it('seeds listening activity when runtime enters listening', () => {
    expect(syncVoiceActivityState(createVoiceActivityState(), 'listening', 100)).toEqual({
      lastActivityMs: 100,
      silenceMarked: false,
      speechActivityReported: false,
    })
  })

  it('resets activity state outside listening', () => {
    expect(
      syncVoiceActivityState(
        { lastActivityMs: 250, silenceMarked: true, speechActivityReported: true },
        'processing',
        500,
      ),
    ).toEqual(createVoiceActivityState())
  })

  it('requests speech activity updates for audible frames', () => {
    expect(
      updateVoiceActivityState(createVoiceActivityState(), [0.03, -0.03, 0.03, -0.03], 250),
    ).toEqual({
      state: {
        lastActivityMs: 250,
        silenceMarked: false,
        speechActivityReported: true,
      },
      shouldRecordSpeechActivity: true,
      shouldMarkSilence: false,
    })
  })

  it('records the first speech frame and throttles consecutive updates', () => {
    expect(
      updateVoiceActivityState(
        { lastActivityMs: 1_000, silenceMarked: false, speechActivityReported: false },
        [0.03, -0.03, 0.03, -0.03],
        1_100,
      ),
    ).toEqual({
      state: {
        lastActivityMs: 1_100,
        silenceMarked: false,
        speechActivityReported: true,
      },
      shouldRecordSpeechActivity: true,
      shouldMarkSilence: false,
    })

    expect(
      updateVoiceActivityState(
        { lastActivityMs: 1_000, silenceMarked: false, speechActivityReported: true },
        [0.03, -0.03, 0.03, -0.03],
        1_100,
      ),
    ).toEqual({
      state: {
        lastActivityMs: 1_000,
        silenceMarked: false,
        speechActivityReported: true,
      },
      shouldRecordSpeechActivity: false,
      shouldMarkSilence: false,
    })
  })

  it('requests silence only after sustained quiet', () => {
    const listeningState = syncVoiceActivityState(createVoiceActivityState(), 'listening', 100)

    expect(updateVoiceActivityState(listeningState, [0.001, -0.001], 1_000)).toEqual({
      state: listeningState,
      shouldRecordSpeechActivity: false,
      shouldMarkSilence: false,
    })

    expect(updateVoiceActivityState(listeningState, [0.001, -0.001], 1_300)).toEqual({
      state: {
        lastActivityMs: 100,
        silenceMarked: true,
        speechActivityReported: false,
      },
      shouldRecordSpeechActivity: false,
      shouldMarkSilence: true,
    })
  })
})

import { afterEach, describe, expect, it } from 'vitest'
import { createVoiceTelemetryRecorder } from './voiceTelemetry'

const TELEMETRY_STORAGE_KEY = 'voxgolem.voiceTelemetry'
const ORIGINAL_URL = window.location.href

afterEach(() => {
  window.localStorage.removeItem(TELEMETRY_STORAGE_KEY)
  Reflect.deleteProperty(window, '__VOXGOLEM_VOICE_TELEMETRY__')
  window.history.replaceState({}, '', ORIGINAL_URL)
})

describe('createVoiceTelemetryRecorder', () => {
  it('is disabled by default', () => {
    const recorder = createVoiceTelemetryRecorder()

    expect(recorder.enabled).toBe(false)
    expect(recorder.nextFrameId(1000)).toBeNull()
    recorder.record('frontend_frame_captured', { frameId: 'frame-1' })
    expect(recorder.snapshot()).toEqual({
      enabled: false,
      droppedCount: 0,
      events: [],
    })
  })

  it('captures bounded events when enabled via localStorage', () => {
    window.localStorage.setItem(TELEMETRY_STORAGE_KEY, '1')
    const recorder = createVoiceTelemetryRecorder()

    expect(recorder.enabled).toBe(true)

    const frameId = recorder.nextFrameId(2000)
    recorder.record('frontend_frame_captured', {
      atMs: 2000,
      frameId,
      details: { sampleCount: 480 },
    })

    const snapshot = recorder.snapshot()
    expect(snapshot.enabled).toBe(true)
    expect(snapshot.droppedCount).toBe(0)
    expect(snapshot.events).toEqual([
      {
        event: 'frontend_frame_captured',
        atMs: 2000,
        frameId,
        details: { sampleCount: 480 },
      },
    ])
    expect(window.__VOXGOLEM_VOICE_TELEMETRY__?.snapshot().events).toHaveLength(1)
  })

  it('enables telemetry when query flag is present', () => {
    window.history.replaceState({}, '', `${window.location.pathname}?voiceTelemetry=1`)
    const recorder = createVoiceTelemetryRecorder()

    expect(recorder.enabled).toBe(true)

    const frameId = recorder.nextFrameId(3000)
    recorder.record('frontend_frame_captured', {
      atMs: 3000,
      frameId,
      details: { sampleCount: 240 },
    })

    expect(recorder.snapshot().events).toEqual([
      {
        event: 'frontend_frame_captured',
        atMs: 3000,
        frameId,
        details: { sampleCount: 240 },
      },
    ])
  })

  it('drops oldest events after max capacity', () => {
    window.localStorage.setItem(TELEMETRY_STORAGE_KEY, '1')
    const recorder = createVoiceTelemetryRecorder()

    for (let index = 0; index < 405; index += 1) {
      recorder.record('frame', {
        atMs: index,
        frameId: `frame-${index}`,
        details: { index },
      })
    }

    const snapshot = recorder.snapshot()
    expect(snapshot.events).toHaveLength(400)
    expect(snapshot.droppedCount).toBe(5)
    expect(snapshot.events[0]?.details['index']).toBe(5)
    expect(snapshot.events[399]?.details['index']).toBe(404)
  })
})

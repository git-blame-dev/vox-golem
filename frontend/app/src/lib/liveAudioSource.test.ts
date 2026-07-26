import { afterEach, describe, expect, it, vi } from 'vitest'
import { listAudioInputDevices, startLiveAudioSource } from './liveAudioSource'

type FakeAudioSetup = {
  readonly processorNode: FakeScriptProcessorNode
  readonly audioContext: FakeAudioContext
  readonly trackStop: ReturnType<typeof vi.fn>
  readonly getUserMedia: ReturnType<typeof vi.fn>
  readonly track: FakeMediaStreamTrack
}

const originalAudioContext = window.AudioContext
const originalMediaDevices = Object.getOwnPropertyDescriptor(navigator, 'mediaDevices')

afterEach(() => {
  if (originalAudioContext === undefined) {
    Reflect.deleteProperty(window, 'AudioContext')
  } else {
    Object.defineProperty(window, 'AudioContext', {
      configurable: true,
      value: originalAudioContext,
    })
  }

  if (originalMediaDevices === undefined) {
    Reflect.deleteProperty(navigator, 'mediaDevices')
  } else {
    Object.defineProperty(navigator, 'mediaDevices', originalMediaDevices)
  }
})

describe('startLiveAudioSource', () => {
  it('lists audio inputs without exposing non-audio devices', async () => {
    const enumerateDevices = vi.fn(async () => [
      { kind: 'audioinput', deviceId: 'studio-device', label: 'Studio Microphone' },
      { kind: 'videoinput', deviceId: 'camera-device', label: 'Camera' },
    ] as MediaDeviceInfo[])
    Object.defineProperty(navigator, 'mediaDevices', {
      configurable: true,
      value: { getUserMedia: vi.fn(), enumerateDevices },
    })

    await expect(listAudioInputDevices()).resolves.toEqual([
      { deviceId: 'studio-device', label: 'Studio Microphone' },
    ])
  })

  it('requests the selected microphone instead of the portal remembered source', async () => {
    const setup = installFakeAudioRuntime()

    const source = await startLiveAudioSource({
      deviceId: 'default',
      onFrame: async () => undefined,
      onError: () => undefined,
    })

    expect(setup.getUserMedia).toHaveBeenCalledWith({
      audio: { deviceId: { exact: 'default' } },
    })
    source.stop()
  })

  it('falls back to the system input when the selected device is stale', async () => {
    const setup = installFakeAudioRuntime()
    const onSelectedDeviceFallback = vi.fn()
    setup.getUserMedia
      .mockRejectedValueOnce(new DOMException('stale device', 'NotFoundError'))
      .mockResolvedValueOnce(createMediaStream(setup.trackStop, setup.track))

    const source = await startLiveAudioSource({ deviceId: 'stale-device', onFrame: async () => undefined, onError: () => undefined, onSelectedDeviceFallback })

    expect(setup.getUserMedia).toHaveBeenNthCalledWith(2, { audio: true })
    expect(onSelectedDeviceFallback).toHaveBeenCalledWith('stale-device')
    source.stop()
  })

  it('does not report fallback when the fallback capture fails', async () => {
    const setup = installFakeAudioRuntime()
    const onSelectedDeviceFallback = vi.fn()
    setup.getUserMedia
      .mockRejectedValueOnce(new DOMException('stale device', 'NotFoundError'))
      .mockRejectedValueOnce(new DOMException('permission denied', 'NotAllowedError'))

    await expect(startLiveAudioSource({ deviceId: 'stale-device', onFrame: async () => undefined, onError: () => undefined, onSelectedDeviceFallback })).rejects.toThrow('permission denied')
    expect(onSelectedDeviceFallback).not.toHaveBeenCalled()
  })

  it('does not report fallback when audio setup fails after fallback capture', async () => {
    const setup = installFakeAudioRuntime()
    const onSelectedDeviceFallback = vi.fn()
    setup.getUserMedia
      .mockRejectedValueOnce(new DOMException('stale device', 'NotFoundError'))
      .mockResolvedValueOnce(createMediaStream(setup.trackStop, setup.track))
    setup.audioContext.resume.mockRejectedValueOnce(new Error('resume failed'))

    await expect(startLiveAudioSource({ deviceId: 'stale-device', onFrame: async () => undefined, onError: () => undefined, onSelectedDeviceFallback })).rejects.toThrow('resume failed')
    expect(onSelectedDeviceFallback).not.toHaveBeenCalled()
    expect(setup.trackStop).toHaveBeenCalledTimes(1)
  })

  it('preserves permission errors for a selected device', async () => {
    const setup = installFakeAudioRuntime()
    const permissionError = new DOMException('permission denied', 'NotAllowedError')
    setup.getUserMedia.mockRejectedValueOnce(permissionError)

    await expect(startLiveAudioSource({ deviceId: 'selected-device', onFrame: async () => undefined, onError: () => undefined })).rejects.toBe(permissionError)
    expect(setup.getUserMedia).toHaveBeenCalledTimes(1)
  })

  it('reports unexpected track termination once but not intentional stop', async () => {
    const setup = installFakeAudioRuntime()
    const onError = vi.fn()
    const source = await startLiveAudioSource({ onFrame: async () => undefined, onError })

    setup.track.onended?.(new Event('ended'))
    setup.track.onended?.(new Event('ended'))
    expect(onError).toHaveBeenCalledTimes(1)
    source.stop()
    expect(onError).toHaveBeenCalledTimes(1)
  })

  it('emits 30ms frames after low-latency processor callbacks', async () => {
    const setup = installFakeAudioRuntime()
    const onFrame = vi.fn<(frame: readonly number[]) => Promise<void>>(async () => undefined)
    const onError = vi.fn()

    const source = await startLiveAudioSource({ onFrame, onError })

    expect(setup.audioContext.createScriptProcessor).toHaveBeenCalledWith(1024, 2, 1)

    setup.processorNode.emit(createStereoRampFrame(0, 1024))
    expect(onFrame).toHaveBeenCalledTimes(0)

    setup.processorNode.emit(createStereoRampFrame(1024, 1024))
    await vi.waitFor(() => {
      expect(onFrame).toHaveBeenCalledTimes(1)
    })

    expect(onError).not.toHaveBeenCalled()
    const firstFrame = onFrame.mock.calls[0]?.[0]
    expect(firstFrame).toBeDefined()
    expect(firstFrame).toHaveLength(480)

    source.stop()
    expect(setup.trackStop).toHaveBeenCalledTimes(1)
  })

  it('clamps captured samples to the backend transcription range', async () => {
    const setup = installFakeAudioRuntime()
    const onFrame = vi.fn<(frame: readonly number[]) => Promise<void>>(async () => undefined)
    const source = await startLiveAudioSource({ onFrame, onError: () => undefined })
    const overdriven = new Float32Array(2048).fill(1.25)

    setup.processorNode.emit([overdriven])
    await vi.waitFor(() => expect(onFrame).toHaveBeenCalled())

    expect(onFrame.mock.calls[0]?.[0]).toSatisfy(
      (frame: readonly number[]) => frame.every((sample) => Number.isFinite(sample) && sample >= -1 && sample <= 1),
    )
    source.stop()
  })

  it('delivers frames sequentially in emitted order', async () => {
    const setup = installFakeAudioRuntime()
    let resolveFirst: () => void = () => {}
    const firstFrameDelivered = new Promise<void>((resolve) => {
      resolveFirst = resolve
    })
    const startedFrames: number[] = []
    const finishedFrames: number[] = []
    const onError = vi.fn()
    const onFrame = vi.fn<(frame: readonly number[]) => Promise<void>>(async (frame) => {
      const firstSample = frame[0] ?? 0
      startedFrames.push(firstSample)

      if (startedFrames.length === 1) {
        await firstFrameDelivered
      }

      finishedFrames.push(firstSample)
    })

    const source = await startLiveAudioSource({ onFrame, onError })

    setup.processorNode.emit(createStereoRampFrame(0, 1024))
    setup.processorNode.emit(createStereoRampFrame(1024, 1024))
    setup.processorNode.emit(createStereoRampFrame(2048, 1024))
    await vi.waitFor(() => {
      expect(startedFrames).toHaveLength(1)
    })
    expect(finishedFrames).toHaveLength(0)

    resolveFirst()
    await vi.waitFor(() => {
      expect(startedFrames).toHaveLength(2)
    })
    await vi.waitFor(() => {
      expect(finishedFrames).toHaveLength(2)
    })

    expect(onError).not.toHaveBeenCalled()
    const firstFinished = finishedFrames[0] ?? 0
    const secondFinished = finishedFrames[1]
    expect(secondFinished).toBeDefined()
    expect(secondFinished).toBeGreaterThan(firstFinished)
    expect(onFrame.mock.calls[0]?.[0]).toHaveLength(480)
    expect(onFrame.mock.calls[1]?.[0]).toHaveLength(480)

    source.stop()
  })
})

function createStereoRampFrame(startValue: number, sampleCount: number): Float32Array[] {
  const left = new Float32Array(sampleCount)
  const right = new Float32Array(sampleCount)

  for (let index = 0; index < sampleCount; index += 1) {
    const value = (startValue + index) / 4096
    left[index] = value
    right[index] = value + 0.0001
  }

  return [left, right]
}

function createInputBuffer(channels: readonly Float32Array[]): AudioBuffer {
  return {
    numberOfChannels: channels.length,
    length: channels[0]?.length ?? 0,
    getChannelData(channelIndex: number): Float32Array {
      return channels[channelIndex] ?? new Float32Array(0)
    },
  } as AudioBuffer
}

function installFakeAudioRuntime(): FakeAudioSetup {
  const trackStop = vi.fn()
  const stream = createMediaStream(trackStop)
  const track = stream.getTracks()[0] as FakeMediaStreamTrack
  const getUserMedia = vi.fn(async () => stream)
  const processorNode = new FakeScriptProcessorNode()
  const audioContext = new FakeAudioContext(48_000, processorNode)

  Object.defineProperty(navigator, 'mediaDevices', {
    configurable: true,
    value: {
      getUserMedia,
    },
  })

  Object.defineProperty(window, 'AudioContext', {
    configurable: true,
    value: class {
      constructor() {
        return audioContext
      }
    },
  })

  return {
    processorNode,
    audioContext,
    trackStop,
    getUserMedia,
    track,
  }
}

type FakeMediaStreamTrack = MediaStreamTrack & { onended: ((event: Event) => void) | null }

function createMediaStream(
  trackStop: ReturnType<typeof vi.fn>,
  track: FakeMediaStreamTrack = { stop: trackStop, onended: null } as unknown as FakeMediaStreamTrack,
): MediaStream {
  return {
    getTracks: () => [track],
  } as MediaStream
}

class FakeScriptProcessorNode {
  onaudioprocess: ((event: AudioProcessingEvent) => void) | null = null
  readonly connect = vi.fn()
  readonly disconnect = vi.fn()

  emit(channels: readonly Float32Array[]): void {
    this.onaudioprocess?.({ inputBuffer: createInputBuffer(channels) } as AudioProcessingEvent)
  }
}

class FakeAudioContext {
  readonly sampleRate: number
  private readonly processorNode: FakeScriptProcessorNode
  readonly destination = {} as AudioNode
  readonly createMediaStreamSource = vi.fn(() => new FakeMediaStreamAudioSourceNode())
  readonly createScriptProcessor = vi.fn(() => this.processorNode)
  readonly resume = vi.fn(async () => undefined)
  readonly close = vi.fn(async () => undefined)

  constructor(sampleRate: number, processorNode: FakeScriptProcessorNode) {
    this.sampleRate = sampleRate
    this.processorNode = processorNode
  }
}

class FakeMediaStreamAudioSourceNode {
  readonly channelCount = 2
  readonly connect = vi.fn()
  readonly disconnect = vi.fn()
}

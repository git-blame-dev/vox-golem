import { afterEach, describe, expect, it, vi } from 'vitest'
import { startLiveAudioSource } from './liveAudioSource'

type FakeAudioSetup = {
  readonly processorNode: FakeScriptProcessorNode
  readonly audioContext: FakeAudioContext
  readonly trackStop: ReturnType<typeof vi.fn>
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
    const value = startValue + index
    left[index] = value
    right[index] = value + 0.5
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
  const stream = {
    getTracks: () => [{ stop: trackStop } as unknown as MediaStreamTrack],
  } as MediaStream
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
  }
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

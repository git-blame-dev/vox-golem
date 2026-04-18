const PROCESSOR_BUFFER_SIZE = 4096
const TARGET_SAMPLE_RATE = 16_000
const OUTPUT_CHUNK_SIZE = 1_600

export interface LiveAudioSource {
  stop(): void
}

export interface StartLiveAudioSourceOptions {
  readonly onFrame: (frame: readonly number[]) => void | Promise<void>
  readonly onError: (error: unknown) => void
}

export async function startLiveAudioSource(
  options: StartLiveAudioSourceOptions,
): Promise<LiveAudioSource> {
  const mediaDevices = navigator.mediaDevices

  if (mediaDevices === undefined || typeof mediaDevices.getUserMedia !== 'function') {
    throw new Error('Microphone capture is unavailable in this runtime')
  }

  const AudioContextConstructor = window.AudioContext

  if (AudioContextConstructor === undefined) {
    throw new Error('Web Audio is unavailable in this runtime')
  }

  const stream = await mediaDevices.getUserMedia({ audio: true })
  let audioContext: AudioContext | null = null

  try {
    audioContext = new AudioContextConstructor()
    const sourceNode = audioContext.createMediaStreamSource(stream)
    const processorNode = audioContext.createScriptProcessor(
      PROCESSOR_BUFFER_SIZE,
      Math.max(sourceNode.channelCount, 1),
      1,
    )
    const resampler = createLinearResampler(audioContext.sampleRate, TARGET_SAMPLE_RATE)
    let pendingSamples: number[] = []
    let pendingFrameDelivery = Promise.resolve()
    let stopped = false

    const reportError = (error: unknown): void => {
      if (stopped) {
        return
      }

      options.onError(error)
    }

    processorNode.onaudioprocess = (event) => {
      if (stopped) {
        return
      }

      try {
        const monoFrame = downmixInputBuffer(event.inputBuffer)
        const resampledFrame = resampler.process(monoFrame)

        if (resampledFrame.length === 0) {
          return
        }

        pendingSamples = pendingSamples.concat(resampledFrame)

        while (pendingSamples.length >= OUTPUT_CHUNK_SIZE) {
          const nextFrame = pendingSamples.slice(0, OUTPUT_CHUNK_SIZE)
          pendingSamples = pendingSamples.slice(OUTPUT_CHUNK_SIZE)
          pendingFrameDelivery = pendingFrameDelivery
            .then(async () => {
              if (stopped) {
                return
              }

              await options.onFrame(nextFrame)
            })
            .catch(reportError)
        }
      } catch (error) {
        reportError(error)
      }
    }

    sourceNode.connect(processorNode)
    processorNode.connect(audioContext.destination)
    await audioContext.resume()
    const liveAudioContext = audioContext

    return {
      stop() {
        if (stopped) {
          return
        }

        stopped = true
        processorNode.onaudioprocess = null
        processorNode.disconnect()
        sourceNode.disconnect()
        stopStream(stream)
        void liveAudioContext.close()
      },
    }
  } catch (error) {
    stopStream(stream)
    if (audioContext !== null) {
      void audioContext.close()
    }
    throw error
  }
}

function stopStream(stream: MediaStream): void {
  stream.getTracks().forEach((track) => {
    track.stop()
  })
}

function downmixInputBuffer(inputBuffer: AudioBuffer): Float32Array {
  if (inputBuffer.numberOfChannels === 1) {
    return new Float32Array(inputBuffer.getChannelData(0))
  }

  const monoFrame = new Float32Array(inputBuffer.length)

  for (let channelIndex = 0; channelIndex < inputBuffer.numberOfChannels; channelIndex += 1) {
    const channelData = inputBuffer.getChannelData(channelIndex)

    for (let sampleIndex = 0; sampleIndex < channelData.length; sampleIndex += 1) {
      const currentSample = monoFrame[sampleIndex] ?? 0

      monoFrame[sampleIndex] =
        currentSample + (channelData[sampleIndex] ?? 0) / inputBuffer.numberOfChannels
    }
  }

  return monoFrame
}

function createLinearResampler(inputSampleRate: number, outputSampleRate: number) {
  if (inputSampleRate <= 0 || outputSampleRate <= 0) {
    throw new Error('Audio sample rates must be positive')
  }

  const sampleStep = inputSampleRate / outputSampleRate
  let pendingInput: number[] = []
  let inputPosition = 0

  return {
    process(frame: Float32Array): number[] {
      pendingInput = pendingInput.concat(Array.from(frame))

      if (pendingInput.length < 2) {
        return []
      }

      const output: number[] = []

      while (inputPosition + 1 < pendingInput.length) {
        const lowerIndex = Math.floor(inputPosition)
        const upperIndex = lowerIndex + 1
        const fraction = inputPosition - lowerIndex
        const lowerSample = pendingInput[lowerIndex] ?? 0
        const upperSample = pendingInput[upperIndex] ?? lowerSample

        output.push(lowerSample + (upperSample - lowerSample) * fraction)
        inputPosition += sampleStep
      }

      const consumedSamples = Math.floor(inputPosition)

      if (consumedSamples > 0) {
        pendingInput = pendingInput.slice(consumedSamples)
        inputPosition -= consumedSamples
      }

      return output
    },
  }
}

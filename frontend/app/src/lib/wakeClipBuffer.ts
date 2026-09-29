const WAKE_CLIP_SAMPLES = 0.8 * 16_000

export interface WakeClipBuffer {
  push(frame: readonly number[]): void
  take(): number[]
  clear(): void
}

export function createWakeClipBuffer(): WakeClipBuffer {
  const audio = new Float32Array(WAKE_CLIP_SAMPLES)
  let next = 0
  let count = 0

  return {
    push(frame): void {
      for (const sample of frame) {
        audio[next] = sample
        next = (next + 1) % audio.length
        count = Math.min(count + 1, audio.length)
      }
    },
    take(): number[] {
      const result = Array.from({ length: count }, (_, index) => audio[(next - count + index + audio.length) % audio.length]!)
      next = 0
      count = 0
      return result
    },
    clear(): void {
      next = 0
      count = 0
    },
  }
}

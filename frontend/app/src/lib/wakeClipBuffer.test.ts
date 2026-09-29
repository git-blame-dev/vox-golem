import { describe, expect, it } from 'vitest'
import { createWakeClipBuffer } from './wakeClipBuffer'

describe('wake clip pre-roll', () => {
  it('retains the latest 1.5 seconds including the beginning of a slower wake phrase', () => {
    const buffer = createWakeClipBuffer()
    for (let index = 0; index < 101; index += 1) {
      buffer.push(Array(480).fill(index))
    }
    const samples = buffer.take()
    expect(samples).toHaveLength(24_000)
    expect(samples[0]).toBe(51)
    expect(samples[479]).toBe(51)
    expect(samples[480]).toBe(52)
    expect(samples.at(-1)).toBe(100)
    expect(buffer.take()).toEqual([])
  })

  it('drops stale session audio when stopped', () => {
    const buffer = createWakeClipBuffer()
    buffer.push([0.2, 0.3])
    buffer.clear()
    buffer.push([0.5])
    expect(buffer.take()).toEqual([0.5])
  })
})

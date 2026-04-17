import { describe, expect, it } from 'vitest'
import { shouldSubmitComposer } from './lib/composer'

describe('shouldSubmitComposer', () => {
  it('returns true for Enter without Shift', () => {
    expect(shouldSubmitComposer('Enter', false)).toBe(true)
  })

  it('returns false for Shift+Enter', () => {
    expect(shouldSubmitComposer('Enter', true)).toBe(false)
  })

  it('returns false for non-Enter keys', () => {
    expect(shouldSubmitComposer('A', false)).toBe(false)
  })
})

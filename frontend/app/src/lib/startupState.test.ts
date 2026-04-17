import { describe, expect, it } from 'vitest'
import { parseStartupState } from './startupState'

describe('parseStartupState', () => {
  it('returns ready state for ready payload', () => {
    expect(parseStartupState({ kind: 'ready' })).toEqual({ kind: 'ready' })
  })

  it('returns error state for valid error payload', () => {
    expect(
      parseStartupState({
        kind: 'error',
        message: 'config file not found',
      }),
    ).toEqual({
      kind: 'error',
      message: 'config file not found',
    })
  })

  it('throws for unsupported payloads', () => {
    expect(() => parseStartupState({ kind: 'loading' })).toThrow()
  })
})

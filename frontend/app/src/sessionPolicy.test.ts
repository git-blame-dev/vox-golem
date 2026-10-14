import { describe, expect, it } from 'vitest'
import { applySnapshotModelOptions } from './sessionPolicy'

describe('canonical native model availability', () => {
  it('uses native availability rather than a stale frontend capability fallback', () => {
    expect(applySnapshotModelOptions([{ value: 'local-fast', label: 'Synthetic fallback', available: false, reason: 'Synthetic stale loading state' }], [
      { id: 'local-fast', label: 'Synthetic native model', available: true, unavailableReason: null },
    ])).toEqual([{ value: 'local-fast', label: 'Synthetic native model', available: true }])
  })
  it('fails closed for native unavailability and absent native membership', () => {
    expect(applySnapshotModelOptions([{ value: 'local-fast', label: 'Synthetic fallback', available: true }], [
      { id: 'local-fast', label: 'Synthetic native model', available: false, unavailableReason: 'Synthetic native unavailable' },
    ])[0]).toMatchObject({ available: false, reason: 'Synthetic native unavailable' })
    expect(applySnapshotModelOptions([{ value: 'local-fast', label: 'Synthetic fallback', available: true }], [])[0]).toMatchObject({ available: false })
  })
})

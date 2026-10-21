import { describe, expect, it } from 'vitest'
import { applySnapshotModelOptions, startupWarningRelevant } from './sessionPolicy'

describe('canonical native model availability', () => {
  it('ignores unsupported Android local-fast startup failures, not supported provider failures', () => {
    expect(startupWarningRelevant({ id: 'local_fast', state: 'failed', reason: 'Synthetic unsupported', actualProvider: null }, 'android')).toBe(false)
    expect(startupWarningRelevant({ id: 'custom_provider', state: 'failed', reason: 'Synthetic supported failure', actualProvider: null }, 'android')).toBe(true)
    expect(startupWarningRelevant({ id: 'local_fast', state: 'failed', reason: 'Synthetic desktop failure', actualProvider: null }, 'desktop')).toBe(true)
    expect(startupWarningRelevant({ id: 'tts', state: 'failed', reason: 'Synthetic speech failure', actualProvider: null }, 'desktop')).toBe(false)
    expect(startupWarningRelevant({ id: 'custom_provider', state: 'available', reason: 'Synthetic available', actualProvider: null }, 'android')).toBe(false)
  })
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

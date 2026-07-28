import { describe, expect, it } from 'vitest'
import { parseUpdateCheckResult } from './appUpdates'

describe('parseUpdateCheckResult', () => {
  it('accepts an available signed update', () => {
    expect(parseUpdateCheckResult({
      status: 'available',
      current_version: '0.1.0',
      version: '2026.7.27-12',
      notes: 'Safer updates',
    })).toEqual({
      status: 'available',
      currentVersion: '0.1.0',
      version: '2026.7.27-12',
      notes: 'Safer updates',
    })
  })

  it('accepts up-to-date, unavailable, and unsupported results', () => {
    expect(parseUpdateCheckResult({
      status: 'up_to_date',
      current_version: '2026.7.27-12',
    })).toEqual({ status: 'up_to_date', currentVersion: '2026.7.27-12' })

    expect(parseUpdateCheckResult({
      status: 'unavailable',
      current_version: '0.1.0',
      reason: 'No updater-enabled release is published yet.',
    })).toEqual({
      status: 'unavailable',
      currentVersion: '0.1.0',
      reason: 'No updater-enabled release is published yet.',
    })

    expect(parseUpdateCheckResult({
      status: 'unsupported',
      current_version: '0.1.0',
      reason: 'Install the AppImage to enable updates.',
    })).toEqual({
      status: 'unsupported',
      currentVersion: '0.1.0',
      reason: 'Install the AppImage to enable updates.',
    })
  })

  it('accepts process-wide installing and installed snapshots', () => {
    expect(parseUpdateCheckResult({
      status: 'installing', current_version: '0.1.0', version: '2026.7.27-12',
    })).toEqual({ status: 'installing', currentVersion: '0.1.0', version: '2026.7.27-12' })
    expect(parseUpdateCheckResult({
      status: 'installed', current_version: '0.1.0', version: '2026.7.27-12',
    })).toEqual({ status: 'installed', currentVersion: '0.1.0', version: '2026.7.27-12' })
  })

  it('rejects malformed or oversized updater payloads', () => {
    expect(() => parseUpdateCheckResult({ status: 'available', version: '2.0.0' })).toThrow(
      'Invalid update check payload',
    )
    expect(() => parseUpdateCheckResult({
      status: 'unsupported',
      current_version: '0.1.0',
      reason: 'x'.repeat(2_049),
    })).toThrow('Invalid update check payload')
  })
})

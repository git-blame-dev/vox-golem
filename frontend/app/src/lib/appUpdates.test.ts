import { describe, expect, it } from 'vitest'
import { parseUpdateCheckResult, parseUpdateProgress } from './appUpdates'

describe('parseUpdateCheckResult', () => {
  it('accepts an available signed update', () => {
    expect(parseUpdateCheckResult({
      status: 'available',
      current_version: '0.1.0',
      version: '2026.7.27-12',
       notes: 'Safer updates',
       install_behavior: 'install_then_restart',
    })).toEqual({
      status: 'available',
      currentVersion: '0.1.0',
      version: '2026.7.27-12',
       notes: 'Safer updates',
       installBehavior: 'install_then_restart',
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
       status: 'installing', current_version: '0.1.0', version: '2026.7.27-12', install_behavior: 'install_then_restart',
    })).toEqual({ status: 'installing', currentVersion: '0.1.0', version: '2026.7.27-12', installBehavior: 'install_then_restart' })
    expect(parseUpdateCheckResult({
       status: 'installed', current_version: '0.1.0', version: '2026.7.27-12', install_behavior: 'install_then_restart',
    })).toEqual({ status: 'installed', currentVersion: '0.1.0', version: '2026.7.27-12', installBehavior: 'install_then_restart' })
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

  it('parses bounded progress and rejects unsafe or inconsistent values', () => {
    expect(parseUpdateProgress({ version: '2.0.0', phase: 'progress', downloaded_bytes: 10, total_bytes: 20 })).toEqual({ version: '2.0.0', phase: 'progress', downloadedBytes: 10, totalBytes: 20 })
    expect(parseUpdateProgress({ version: '2.0.0', phase: 'started', downloaded_bytes: 10 })).toEqual({ version: '2.0.0', phase: 'started', downloadedBytes: 10 })
    expect(parseUpdateProgress({ version: '2.0.0', phase: 'progress', downloaded_bytes: 10, total_bytes: null })).toEqual({ version: '2.0.0', phase: 'progress', downloadedBytes: 10 })
    expect(() => parseUpdateProgress({ version: '2.0.0', phase: 'progress', downloaded_bytes: 21, total_bytes: 20 })).toThrow()
    expect(() => parseUpdateProgress({ version: '2.0.0', phase: 'progress', downloaded_bytes: Number.MAX_SAFE_INTEGER + 1 })).toThrow()
  })
})

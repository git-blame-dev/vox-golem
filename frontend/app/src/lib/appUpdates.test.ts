import { describe, expect, it } from 'vitest'
import { parseUpdateSnapshot, retryOperation, selectFreshSnapshot } from './appUpdates'
import type { UpdateSnapshot } from './appUpdates'

describe('app update snapshots', () => {
  it('parses a complete native snapshot including empty and long plain-text notes', () => {
    const notes = `\n${'x'.repeat(10_000)}<b>literal</b>`
    expect(parseUpdateSnapshot(nativeSnapshot({ notes }))).toMatchObject({
      revision: 4,
      phase: 'available',
      notes,
      autoDownloadEnabled: true,
      autoInstallEnabled: true,
    })
    expect(parseUpdateSnapshot(nativeSnapshot({ notes: '' })).notes).toBe('')
  })

  it('rejects malformed revisions and inconsistent progress', () => {
    expect(() => parseUpdateSnapshot(nativeSnapshot({ revision: -1 }))).toThrow('Invalid update snapshot payload')
    expect(() => parseUpdateSnapshot(nativeSnapshot({ downloaded_bytes: 11, total_bytes: 10 }))).toThrow()
  })

  it('orders event and IPC snapshots by native revision', () => {
    const ready = parsed({ revision: 8, phase: 'ready' })
    const stale = parsed({ revision: 7, phase: 'downloading', operation: 'download' })
    expect(selectFreshSnapshot(ready, stale)).toBe(ready)
    expect(selectFreshSnapshot(stale, ready)).toBe(ready)
    expect(selectFreshSnapshot(ready, parsed({ revision: 8, phase: 'downloading' }))).toBe(ready)
  })

  it('retries according to the retained native phase', () => {
    expect(retryOperation(parsed({ phase: 'available' }))).toBe('download')
    expect(retryOperation(parsed({ phase: 'ready' }))).toBe('install')
    expect(retryOperation(parsed({ phase: 'idle' }))).toBe('check')
    expect(retryOperation(parsed({ phase: 'unavailable', reason: 'No release yet', version: null }))).toBe('check')
  })
})

function parsed(overrides: Record<string, unknown>): UpdateSnapshot {
  return parseUpdateSnapshot(nativeSnapshot(overrides))
}

function nativeSnapshot(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    revision: 4,
    phase: 'available',
    operation: null,
    current_version: '1.0.0',
    version: '2.0.0',
    notes: null,
    progress_phase: null,
    downloaded_bytes: 0,
    total_bytes: null,
    error: null,
    reason: null,
    auto_download_enabled: true,
    auto_install_enabled: true,
    ...overrides,
  }
}

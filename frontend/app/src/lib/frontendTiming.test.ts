import { afterEach, describe, expect, it, vi } from 'vitest'
import { FrontendTiming, frontendTimingEnabled } from './frontendTiming'
import { invokeTauriCommand } from './tauri'

vi.mock('./tauri', () => ({ isAndroidRuntime: true, invokeTauriCommand: vi.fn().mockResolvedValue(null) }))
afterEach(() => { vi.unstubAllEnvs(); vi.restoreAllMocks(); vi.mocked(invokeTauriCommand).mockClear() })

describe('frontend timing boundaries', () => {
  it('requires exact explicit opt-in', () => {
    for (const value of ['', '0', 'true']) {
      vi.stubEnv('VITE_VOXGOLEM_TIMING', value)
      expect(frontendTimingEnabled()).toBe(false)
    }
    vi.stubEnv('VITE_VOXGOLEM_TIMING', '1')
    expect(frontendTimingEnabled()).toBe(true)
  })

  it('keeps response timing and closure when the diagnostic sink throws', () => {
    vi.mocked(invokeTauriCommand).mockResolvedValue(null)
    let now = 100
    vi.spyOn(performance, 'now').mockImplementation(() => now)
    vi.spyOn(console, 'info').mockImplementation(() => { throw new Error('Synthetic sink failure') })
    const timing = new FrontendTiming(true)
    expect(() => timing.startup('SHELL_DOM_COMMITTED')).not.toThrow()
    const request = timing.admit('synthetic-projection', now)
    request.dispatch()
    now = 200
    expect(request.received('Synthetic answer')).toEqual({ firstTextMs: 100, completedMs: 100 })
    now = 300
    expect(request.settle()).toEqual({ firstTextMs: 100, completedMs: 200 })
    expect(() => timing.domCommitted('synthetic-projection')).not.toThrow()
  })

  it('drops pending canonical correlation on retirement without disabling the next fresh request', () => {
    vi.mocked(invokeTauriCommand).mockResolvedValue(null)
    vi.spyOn(performance, 'now').mockReturnValue(100)
    const log = vi.spyOn(console, 'info').mockImplementation(() => undefined)
    const timing = new FrontendTiming(true)
    const retired = timing.admit('synthetic-retired-projection', 100)
    retired.received('Synthetic retired answer')
    retired.canonicalAnswer('synthetic-retired-canonical')
    retired.settle()
    timing.retireRequest()
    timing.domCommitted('synthetic-retired-canonical')
    expect(invokeTauriCommand).not.toHaveBeenCalled()
    const fresh = timing.admit('synthetic-fresh-projection', 100)
    fresh.received('Synthetic fresh answer')
    timing.domCommitted('synthetic-fresh-projection')
    const commits = log.mock.calls.map(([, value]) => JSON.parse(value as string) as Record<string, unknown>).filter((entry) => entry['stage'] === 'FIRST_TEXT_DOM_COMMITTED')
    expect(commits).toEqual([{ scope: 'frontend_request', request: 2, stage: 'FIRST_TEXT_DOM_COMMITTED', atMs: 100, elapsedMs: 0 }])
    expect(invokeTauriCommand).toHaveBeenCalledExactlyOnceWith('report_startup_stage', { stage: 'first_response_dom' })
  })

  it('bounds stage output, ignores restored/unrelated DOM, and retains only the last request after settlement', async () => {
    let now = 100
    vi.spyOn(performance, 'now').mockImplementation(() => now)
    const log = vi.spyOn(console, 'info').mockImplementation(() => undefined)
    vi.mocked(invokeTauriCommand).mockRejectedValue(new Error('Synthetic bridge unavailable'))
    const timing = new FrontendTiming(true)
    timing.startup('STATE_READY'); timing.startup('STATE_READY')
    timing.domCommitted('synthetic-restored-answer')
    const first = timing.admit('synthetic-projection-a', now)
    first.dispatch(); first.dispatch()
    first.reconciliation('inline'); first.reconciliation('inline')
    now = 150
    expect(first.received(' \n\t')).toBeUndefined()
    timing.domCommitted('synthetic-projection-a')
    now = 200
    expect(first.received('Synthetic answer')).toEqual({ firstTextMs: 100, completedMs: 100 })
    now = 250
    first.canonicalAnswer('synthetic-canonical-a')
    expect(first.settle()).toEqual({ firstTextMs: 100, completedMs: 150 })
    timing.domCommitted('synthetic-restored-answer')
    now = 275
    timing.domCommitted('synthetic-canonical-a'); timing.domCommitted('synthetic-canonical-a')
    first.provider({ firstTextMs: 7, completedMs: 9 }); first.provider({ firstTextMs: 8, completedMs: 10 })
    const second = timing.admit('synthetic-projection-b', now)
    second.received('Synthetic second answer')
    timing.domCommitted('synthetic-canonical-a')
    timing.domCommitted('synthetic-projection-b')
    await Promise.resolve()
    const metadata = log.mock.calls.map(([, value]) => JSON.parse(value as string) as Record<string, unknown>)
    expect(metadata.filter((entry) => entry['stage'] === 'FIRST_TEXT_DOM_COMMITTED')).toEqual([
      { scope: 'frontend_request', request: 1, stage: 'FIRST_TEXT_DOM_COMMITTED', atMs: 275, elapsedMs: 175 },
      { scope: 'frontend_request', request: 2, stage: 'FIRST_TEXT_DOM_COMMITTED', atMs: 275, elapsedMs: 0 },
    ])
    expect(metadata.filter((entry) => entry['stage'] === 'IPC_DISPATCH')).toHaveLength(1)
    expect(metadata.filter((entry) => entry['stage'] === 'INLINE_SNAPSHOT_ACCEPTED')).toHaveLength(1)
    expect(metadata.filter((entry) => entry['scope'] === 'provider_request')).toEqual([
      { scope: 'provider_request', request: 1, stage: 'PROVIDER_DURATIONS', firstTextMs: 7, completedMs: 9 },
    ])
    expect(JSON.stringify(metadata)).not.toContain('synthetic')
    expect(JSON.stringify(metadata)).not.toContain('Synthetic')
    expect(invokeTauriCommand).toHaveBeenCalledTimes(2)
    expect(invokeTauriCommand).toHaveBeenNthCalledWith(1, 'report_startup_stage', { stage: 'frontend_ready' })
    expect(invokeTauriCommand).toHaveBeenNthCalledWith(2, 'report_startup_stage', { stage: 'first_response_dom' })
  })
})

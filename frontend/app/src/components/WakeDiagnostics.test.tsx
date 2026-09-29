import { act } from 'react'
import { createRoot } from 'react-dom/client'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { WakeDiagnostics } from './WakeDiagnostics'

const roots: ReturnType<typeof createRoot>[] = []
const containers: HTMLElement[] = []

afterEach(() => {
  for (const root of roots) act(() => root.unmount())
  for (const container of containers) container.remove()
  roots.length = 0
  containers.length = 0
  Reflect.deleteProperty(window, '__TAURI_INTERNALS__')
  vi.restoreAllMocks()
})

describe('WakeDiagnostics', () => {
  it('distinguishes numeric diagnostics from separately saved wake clips', async () => {
    const container = await render()
    expect(container.textContent).toContain('Wake clips may separately save local audio')
    expect(container.textContent).not.toContain('no audio is saved')
  })
  it('does not invoke diagnostics until the panel is opened and collection is started', async () => {
    vi.useFakeTimers()
    const invoke = vi.fn(async (command: string, args?: unknown) => diagnosticSnapshot({
      enabled: command === 'set_wake_diagnostics' && args !== undefined,
    }))
    installTauri(invoke)
    const container = await render()

    expect(invoke).not.toHaveBeenCalled()
    await click(container, 'Wake diagnostics')
    expect(invoke).not.toHaveBeenCalled()
    await click(container, 'Start collection')
    expect(invoke).toHaveBeenCalledWith('set_wake_diagnostics', { enabled: true })
    expect(invoke.mock.calls.some(([command]) => command === 'get_wake_diagnostics')).toBe(false)
    await act(async () => { await vi.advanceTimersByTimeAsync(1_000) })
    expect(invoke).toHaveBeenCalledWith('get_wake_diagnostics', undefined)
    vi.useRealTimers()
  })

  it('polls only while collecting and displays the raw score and signal measurements', async () => {
    vi.useFakeTimers()
    const invoke = vi.fn(async (command: string, args?: unknown) => {
      if (command === 'set_wake_diagnostics') {
        return diagnosticSnapshot({ enabled: (args as { enabled?: boolean }).enabled === true })
      }
      return diagnosticSnapshot({ enabled: true, events: [scoreEvent({ score: 0.73, rms_dbfs: -24, inference_ms: 12 })] })
    })
    installTauri(invoke)
    const container = await render()
    await click(container, 'Wake diagnostics')
    await click(container, 'Start collection')
    await act(async () => { await vi.advanceTimersByTimeAsync(1_000) })
    expect(container.textContent).toContain('0.73')
    expect(container.textContent).toContain('-24 dBFS')
    expect(container.textContent).toContain('12 ms')
    await act(async () => { await vi.advanceTimersByTimeAsync(1_000) })
    expect(invoke.mock.calls.filter(([command]) => command === 'get_wake_diagnostics').length).toBeGreaterThanOrEqual(1)
    await click(container, 'Stop collection')
    const polls = invoke.mock.calls.filter(([command]) => command === 'get_wake_diagnostics').length
    await act(async () => { await vi.advanceTimersByTimeAsync(3_000) })
    expect(invoke.mock.calls.filter(([command]) => command === 'get_wake_diagnostics')).toHaveLength(polls)
    vi.useRealTimers()
  })

  it('exports an immutable marked snapshot rather than later live poll updates', async () => {
    vi.useFakeTimers()
    const created: Blob[] = []
    const createObjectURL = vi.spyOn(URL, 'createObjectURL').mockImplementation((value) => {
      if (value instanceof Blob) created.push(value)
      return 'blob:diagnostics'
    })
    const revokeObjectURL = vi.spyOn(URL, 'revokeObjectURL').mockImplementation(() => undefined)
    const invoke = vi.fn(async (command: string) => {
      if (command === 'set_wake_diagnostics') return diagnosticSnapshot({ enabled: true })
      if (command === 'mark_wake_diagnostics') return diagnosticSnapshot({ enabled: true, events: [scoreEvent({ score: 0.61, audio: 'must-not-export' }), markerEvent()] })
      return diagnosticSnapshot({ enabled: true, events: [scoreEvent({ score: 0.99 })] })
    })
    installTauri(invoke)
    const container = await render()
    await click(container, 'Wake diagnostics')
    await click(container, 'Start collection')
    await click(container, 'Mark wake attempt')
    expect(invoke).toHaveBeenCalledWith('mark_wake_diagnostics', { marker: 'wake_attempt' })
    await act(async () => { await vi.advanceTimersByTimeAsync(1_000) })
    expect(container.textContent).toContain('0.99')
    const exportButton = getButton(container, 'Export marked snapshot')
    await act(async () => { exportButton.click() })
    const exported = await created[0]?.text()
    await act(async () => { await vi.advanceTimersByTimeAsync(0) })
    expect(exported).toContain('"score": 0.61')
    expect(exported).not.toContain('0.99')
    expect(exported).toContain('"marker": "wake_attempt"')
    expect(exported).not.toContain('must-not-export')
    expect(revokeObjectURL).toHaveBeenCalledWith('blob:diagnostics')
    expect(createObjectURL).toHaveBeenCalledOnce()
    vi.useRealTimers()
  })

  it('exports the stopped collection and surfaces backend errors inline', async () => {
    const invoke = vi.fn(async (command: string) => {
      if (command === 'set_wake_diagnostics') {
        if (invoke.mock.calls.filter(([name]) => name === command).length === 1) return diagnosticSnapshot({ enabled: true })
        throw new Error('diagnostics unavailable')
      }
      return diagnosticSnapshot({ enabled: true })
    })
    installTauri(invoke)
    const container = await render()
    await click(container, 'Wake diagnostics')
    await click(container, 'Start collection')
    await click(container, 'Stop collection')
    expect(container.textContent).toContain('diagnostics unavailable')
    expect(getButton(container, 'Export current snapshot').disabled).toBe(false)
  })

  it('ignores late poll responses after unmount', async () => {
    const poll = deferred<unknown>()
    const invoke = vi.fn(async (command: string) => command === 'get_wake_diagnostics' ? poll.promise : diagnosticSnapshot({ enabled: true }))
    installTauri(invoke)
    const container = await render()
    await click(container, 'Wake diagnostics')
    await click(container, 'Start collection')
    const root = roots[0]
    if (!root) throw new Error('missing test root')
    await act(async () => { root.unmount() })
    await act(async () => { poll.resolve(diagnosticSnapshot()); await poll.promise })
    expect(container.textContent).toBe('')
  })

  it('ignores an in-flight poll after collection has stopped', async () => {
    vi.useFakeTimers()
    const poll = deferred<unknown>()
    const invoke = vi.fn(async (command: string, args?: unknown) => {
      if (command === 'get_wake_diagnostics') return poll.promise
      if (command === 'set_wake_diagnostics' && (args as { enabled?: boolean }).enabled === false) {
        return diagnosticSnapshot({ enabled: false, events: [scoreEvent({ score: 0.42 })] })
      }
      return diagnosticSnapshot({ enabled: true })
    })
    installTauri(invoke)
    const container = await render()
    await click(container, 'Wake diagnostics')
    await click(container, 'Start collection')
    await act(async () => { await vi.advanceTimersByTimeAsync(1_000) })
    await click(container, 'Stop collection')
    poll.resolve(diagnosticSnapshot({ enabled: true, events: [scoreEvent({ score: 0.91 })] }))
    await act(async () => { await poll.promise })
    expect(container.textContent).toContain('0.42')
    expect(container.textContent).not.toContain('0.91')
    vi.useRealTimers()
  })

  it('turns backend collection off when the mounted collector closes', async () => {
    const invoke = vi.fn(async (command: string, args?: unknown) => diagnosticSnapshot({
      enabled: command === 'set_wake_diagnostics' && (args as { enabled?: boolean } | undefined)?.enabled === true,
    }))
    installTauri(invoke)
    const container = await render()
    await click(container, 'Wake diagnostics')
    await click(container, 'Start collection')
    const root = roots[0]
    if (!root) throw new Error('missing test root')
    await act(async () => { root.unmount() })
    expect(invoke).toHaveBeenCalledWith('set_wake_diagnostics', { enabled: false })
  })

  it('does not send a stop command when closed before collection starts', async () => {
    const invoke = vi.fn(async () => diagnosticSnapshot())
    installTauri(invoke)
    await render()
    const root = roots[0]
    if (!root) throw new Error('missing test root')
    await act(async () => { root.unmount() })
    expect(invoke).not.toHaveBeenCalled()
  })

  it('turns collection off if Start succeeds after the component has unmounted', async () => {
    const started = deferred<unknown>()
    const invoke = vi.fn(async (command: string, args?: unknown) => {
      if (command === 'set_wake_diagnostics' && (args as { enabled?: boolean } | undefined)?.enabled === true) return started.promise
      return diagnosticSnapshot({ enabled: false })
    })
    installTauri(invoke)
    const container = await render()
    await click(container, 'Wake diagnostics')
    await act(async () => { getButton(container, 'Start collection').click() })
    const root = roots[0]
    if (!root) throw new Error('missing test root')
    await act(async () => { root.unmount() })
    started.resolve(diagnosticSnapshot({ enabled: true }))
    await act(async () => { await started.promise; await Promise.resolve() })
    expect(invoke).toHaveBeenCalledWith('set_wake_diagnostics', { enabled: false })
  })

  it('serializes old Start, unmount cleanup, and reopened Start in FIFO order', async () => {
    const oldStart = deferred<unknown>()
    const operations: string[] = []
    let startCount = 0
    let backendEnabled = false
    const invoke = vi.fn(async (command: string, args?: unknown) => {
      if (command !== 'set_wake_diagnostics') return diagnosticSnapshot({ enabled: backendEnabled })
      if ((args as { enabled?: boolean }).enabled) {
        startCount += 1
        const currentStart = startCount
        operations.push(`start-${currentStart}`)
        if (currentStart === 1) {
          return oldStart.promise.then((response) => {
            backendEnabled = (response as { enabled: boolean }).enabled
            operations.push('old-start-resolved')
            return response
          })
        }
        backendEnabled = true
        operations.push('new-start-resolved')
        return diagnosticSnapshot({ enabled: true })
      }
      backendEnabled = false
      operations.push('cleanup-stop')
      return diagnosticSnapshot({ enabled: false })
    })
    installTauri(invoke)

    const oldContainer = await render()
    await click(oldContainer, 'Wake diagnostics')
    await click(oldContainer, 'Start collection')
    expect(startCount).toBe(1)
    const oldRoot = roots[0]
    if (!oldRoot) throw new Error('missing old root')
    await act(async () => { oldRoot.unmount() })

    const newContainer = await render()
    await click(newContainer, 'Wake diagnostics')
    await click(newContainer, 'Start collection')
    expect(startCount).toBe(1)

    oldStart.resolve(diagnosticSnapshot({ enabled: true }))
    await act(async () => {
      await oldStart.promise
      for (let index = 0; index < 12; index += 1) await Promise.resolve()
    })
    expect(operations).toEqual(['start-1', 'old-start-resolved', 'cleanup-stop', 'start-2', 'new-start-resolved'])
    expect(backendEnabled).toBe(true)
    expect(getButton(newContainer, 'Stop collection')).toBeInstanceOf(HTMLButtonElement)
  })

  it('serializes old Stop, unmount cleanup, and reopened Start in FIFO order', async () => {
    const oldStop = deferred<unknown>()
    const operations: string[] = []
    let backendEnabled = false
    let stopCount = 0
    const invoke = vi.fn(async (command: string, args?: unknown) => {
      if (command !== 'set_wake_diagnostics') return diagnosticSnapshot({ enabled: backendEnabled })
      if ((args as { enabled?: boolean }).enabled) {
        operations.push('new-start')
        backendEnabled = true
        return diagnosticSnapshot({ enabled: true })
      }
      stopCount += 1
      if (stopCount === 1) {
        operations.push('old-stop')
        return oldStop.promise.then((response) => {
          backendEnabled = (response as { enabled: boolean }).enabled
          operations.push('old-stop-resolved')
          return response
        })
      }
      operations.push('unmount-cleanup')
      backendEnabled = false
      return diagnosticSnapshot({ enabled: false })
    })
    installTauri(invoke)

    const oldContainer = await render()
    await click(oldContainer, 'Wake diagnostics')
    await click(oldContainer, 'Start collection')
    expect(backendEnabled).toBe(true)
    await click(oldContainer, 'Stop collection')
    expect(operations).toEqual(['new-start', 'old-stop'])
    const oldRoot = roots[0]
    if (!oldRoot) throw new Error('missing old root')
    await act(async () => { oldRoot.unmount() })

    const newContainer = await render()
    await click(newContainer, 'Wake diagnostics')
    await click(newContainer, 'Start collection')
    expect(operations).toEqual(['new-start', 'old-stop'])

    oldStop.resolve(diagnosticSnapshot({ enabled: false }))
    await act(async () => {
      await oldStop.promise
      for (let index = 0; index < 12; index += 1) await Promise.resolve()
    })
    expect(operations).toEqual(['new-start', 'old-stop', 'old-stop-resolved', 'unmount-cleanup', 'new-start'])
    expect(backendEnabled).toBe(true)
    expect(getButton(newContainer, 'Stop collection')).toBeInstanceOf(HTMLButtonElement)
  })

  it('serializes an old Mark before cleanup and prevents it from changing the new session', async () => {
    const oldMark = deferred<unknown>()
    const operations: string[] = []
    let backendEnabled = false
    let startCount = 0
    const invoke = vi.fn(async (command: string, args?: unknown) => {
      if (command === 'mark_wake_diagnostics') {
        operations.push('old-mark')
        return oldMark.promise.then((response) => {
          operations.push('old-mark-resolved')
          return response
        })
      }
      if ((args as { enabled?: boolean } | undefined)?.enabled === false) {
        operations.push('cleanup')
        backendEnabled = false
        return diagnosticSnapshot({ enabled: false })
      }
      startCount += 1
      operations.push(startCount === 1 ? 'old-start' : 'new-start')
      backendEnabled = true
      return diagnosticSnapshot({ enabled: true })
    })
    installTauri(invoke)

    const oldContainer = await render()
    await click(oldContainer, 'Wake diagnostics')
    await click(oldContainer, 'Start collection')
    await click(oldContainer, 'Mark wake attempt')
    expect(operations).toEqual(['old-start', 'old-mark'])
    const oldRoot = roots[0]
    if (!oldRoot) throw new Error('missing old root')
    await act(async () => { oldRoot.unmount() })

    const newContainer = await render()
    await click(newContainer, 'Wake diagnostics')
    await click(newContainer, 'Start collection')
    expect(operations).toEqual(['old-start', 'old-mark'])
    oldMark.resolve(diagnosticSnapshot({ enabled: true, events: [markerEvent()] }))
    await act(async () => {
      await oldMark.promise
      for (let index = 0; index < 12; index += 1) await Promise.resolve()
    })

    expect(operations).toEqual(['old-start', 'old-mark', 'old-mark-resolved', 'cleanup', 'new-start'])
    expect(backendEnabled).toBe(true)
    expect(getButton(newContainer, 'Stop collection')).toBeInstanceOf(HTMLButtonElement)
    expect(getButton(newContainer, 'Export marked snapshot').disabled).toBe(true)
  })

  it('keeps collection active and Stop available after a failed stop so it can be retried', async () => {
    vi.useFakeTimers()
    let stopAttempts = 0
    const invoke = vi.fn(async (command: string, args?: unknown) => {
      if (command === 'set_wake_diagnostics' && (args as { enabled?: boolean } | undefined)?.enabled === false) {
        stopAttempts += 1
        if (stopAttempts === 1) throw new Error('stop failed')
        return diagnosticSnapshot({ enabled: false })
      }
      return diagnosticSnapshot({ enabled: true })
    })
    installTauri(invoke)
    const container = await render()
    await click(container, 'Wake diagnostics')
    await click(container, 'Start collection')
    await click(container, 'Stop collection')
    expect(container.textContent).toContain('stop failed')
    expect(getButton(container, 'Stop collection')).toBeInstanceOf(HTMLButtonElement)
    await act(async () => { await vi.advanceTimersByTimeAsync(1_000) })
    expect(container.textContent).toContain('stop failed')
    expect(getButton(container, 'Stop collection')).toBeInstanceOf(HTMLButtonElement)
    await click(container, 'Stop collection')
    expect(stopAttempts).toBe(2)
    expect(getButton(container, 'Start collection')).toBeInstanceOf(HTMLButtonElement)
    vi.useRealTimers()
  })

  it('reconciles to stopped when a poll reports that backend collection ended', async () => {
    vi.useFakeTimers()
    const invoke = vi.fn(async (command: string) => command === 'set_wake_diagnostics'
      ? diagnosticSnapshot({ enabled: true }) : diagnosticSnapshot({ enabled: false }))
    installTauri(invoke)
    const container = await render()
    await click(container, 'Wake diagnostics')
    await click(container, 'Start collection')
    await act(async () => { await vi.advanceTimersByTimeAsync(1_000) })
    expect(getButton(container, 'Start collection')).toBeInstanceOf(HTMLButtonElement)
    await act(async () => { await vi.advanceTimersByTimeAsync(3_000) })
    expect(invoke.mock.calls.filter(([command]) => command === 'get_wake_diagnostics')).toHaveLength(1)
    vi.useRealTimers()
  })

  it('disables Start and Mark, but keeps Stop and exports available when externally disabled', async () => {
    const invoke = vi.fn(async (command: string) => command === 'mark_wake_diagnostics'
      ? diagnosticSnapshot({ enabled: true, events: [markerEvent()] })
      : diagnosticSnapshot({ enabled: true }))
    installTauri(invoke)
    const container = await render(true)
    await click(container, 'Wake diagnostics')
    expect(getButton(container, 'Start collection').disabled).toBe(true)
    expect(invoke).not.toHaveBeenCalled()
    const active = await render()
    await click(active, 'Wake diagnostics')
    await click(active, 'Start collection')
    await click(active, 'Mark wake attempt')
    const root = roots[1]
    if (!root) throw new Error('missing active test root')
    await act(async () => { root.render(<WakeDiagnostics disabled />) })
    expect(getButton(active, 'Stop collection').disabled).toBe(false)
    expect(getButton(active, 'Mark false wake').disabled).toBe(true)
    expect(getButton(active, 'Export current snapshot').disabled).toBe(false)
    expect(getButton(active, 'Export marked snapshot').disabled).toBe(false)
  })

  it('reports browser download failures inline', async () => {
    vi.spyOn(URL, 'createObjectURL').mockImplementation(() => { throw new Error('download unavailable') })
    const invoke = vi.fn(async (command: string) => diagnosticSnapshot({ enabled: command === 'set_wake_diagnostics' }))
    installTauri(invoke)
    const container = await render()
    await click(container, 'Wake diagnostics')
    await click(container, 'Start collection')
    await act(async () => { getButton(container, 'Export current snapshot').click() })
    expect(container.textContent).toContain('Export failed: download unavailable')
  })
})

function installTauri(invoke: (command: string, args?: unknown) => Promise<unknown>): void {
  window.__TAURI_INTERNALS__ = { invoke }
}

async function render(disabled = false): Promise<HTMLElement> {
  const container = document.createElement('div')
  document.body.append(container)
  containers.push(container)
  const root = createRoot(container)
  roots.push(root)
  await act(async () => { root.render(<WakeDiagnostics disabled={disabled} />) })
  return container
}

async function click(container: HTMLElement, name: string): Promise<void> {
  if (name === 'Wake diagnostics') {
    await act(async () => { container.querySelector('summary')?.dispatchEvent(new MouseEvent('click', { bubbles: true })); await Promise.resolve() })
  } else {
    await act(async () => { getButton(container, name).click(); await Promise.resolve(); await Promise.resolve() })
  }
}

function getButton(container: HTMLElement, name: string): HTMLButtonElement {
  const button = Array.from(container.querySelectorAll('button')).find((candidate) => candidate.textContent === name)
  if (!(button instanceof HTMLButtonElement)) throw new Error(`button not found: ${name}; ${container.textContent}`)
  return button
}

function diagnosticSnapshot(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    schema_version: 1, enabled: false, model: 'hey-livekit', feature_profile: 'normalized_f32', threshold: 0.68,
    sample_rate_hz: 16_000, window_samples: 32_000, hop_samples: 1_280, warmup_remaining_samples: 0,
    phase: 'sleeping', dropped_events: 0, events: [], ...overrides,
  }
}

function scoreEvent(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return { sequence: 1, at_ms: 1_000, kind: 'score', score: 0.73, sample_position: 32_000,
    rms_dbfs: -20, peak: 0.2, clipped_fraction: 0, inference_ms: 10, ...overrides }
}

function markerEvent(): Record<string, unknown> {
  return { sequence: 2, at_ms: 1_100, kind: 'marker', marker: 'wake_attempt' }
}

function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((resolvePromise) => { resolve = resolvePromise })
  return { promise, resolve }
}

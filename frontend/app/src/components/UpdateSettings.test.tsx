import { act } from 'react'
import { createRoot } from 'react-dom/client'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { UpdateSettings } from './UpdateSettings'
import { useAppUpdates } from '../lib/useAppUpdates'
import type { TauriEvent } from '../lib/tauri'

const roots: ReturnType<typeof createRoot>[] = []
const containers: HTMLElement[] = []

afterEach(() => {
  for (const root of roots) act(() => root.unmount())
  for (const container of containers) container.remove()
  roots.length = 0
  containers.length = 0
  Reflect.deleteProperty(window, '__TAURI_INTERNALS__')
})

describe('UpdateSettings revisioned native state', () => {
  it('registers its listener before hydration and never performs a startup check', async () => {
    const calls: string[] = []
    installTauri({
      listen: async () => { calls.push('listen'); return () => undefined },
      invoke: async (command) => {
        calls.push(command)
        if (command === 'get_update_snapshot') return snapshot({ phase: 'up_to_date', version: null })
        throw new Error(`unexpected command: ${command}`)
      },
    })
    const container = await renderUpdates()
    expect(calls).toEqual(['listen', 'get_update_snapshot'])
    expect(calls).not.toContain('check_for_update')
    expect(container.textContent).toContain('Up to date')
  })

  it('keeps a newer Ready event when delayed hydration returns Downloading', async () => {
    const hydration = deferred<unknown>()
    let handler: (event: TauriEvent) => void = () => undefined
    installTauri({
      listen: async (_event, next) => { handler = next; return () => undefined },
      invoke: async (command) => {
        if (command === 'get_update_snapshot') return hydration.promise
        throw new Error(`unexpected command: ${command}`)
      },
    })
    const container = await renderUpdates(false)
    await act(async () => { handler({ payload: snapshot({ revision: 8, phase: 'ready' }) }) })
    hydration.resolve(snapshot({ revision: 7, phase: 'downloading', operation: 'download', progress_phase: 'downloading' }))
    await act(async () => { await hydration.promise })
    expect(container.textContent).toContain('Update 2.0.0 is ready')
    expect(getButton(container, 'Restart and update')).toBeInstanceOf(HTMLButtonElement)
  })

  it('manual Check exposes Available and does not automatically download', async () => {
    const invoke = vi.fn(async (command: string) => {
      if (command === 'get_update_snapshot') return snapshot({ phase: 'up_to_date', version: null })
      if (command === 'check_for_update') return snapshot({ revision: 3, phase: 'available', notes: 'Details' })
      throw new Error(`unexpected command: ${command}`)
    })
    installTauri({ invoke })
    const container = await renderUpdates()
    await act(async () => { getButton(container, 'Check').click(); await Promise.resolve() })
    expect(container.textContent).toContain('Update 2.0.0 available')
    expect(container.querySelector('.settings-panel__update-notes')?.textContent).toBe('Details')
    expect(invoke.mock.calls.some(([command]) => command === 'download_update')).toBe(false)
  })

  it('uses unified snapshot progress and ignores stale progress revisions', async () => {
    let handler: (event: TauriEvent) => void = () => undefined
    installTauri({
      listen: async (_event, next) => { handler = next; return () => undefined },
      invoke: async () => snapshot({ revision: 2, phase: 'downloading', operation: 'download', progress_phase: 'downloading' }),
    })
    const container = await renderUpdates()
    await act(async () => {
      handler({ payload: snapshot({ revision: 4, phase: 'downloading', operation: 'download', progress_phase: 'downloading', downloaded_bytes: 5_000_000, total_bytes: 10_000_000 }) })
      handler({ payload: snapshot({ revision: 3, phase: 'downloading', operation: 'download', progress_phase: 'downloading', downloaded_bytes: 1, total_bytes: 10_000_000 }) })
    })
    expect(container.textContent).toContain('Downloading: 50% (5.0 / 10.0 MB)')
  })

  it('keeps restart-required state through stale responses and restart failure', async () => {
    const install = deferred<unknown>()
    let handler: (event: TauriEvent) => void = () => undefined
    installTauri({
      listen: async (_event, next) => { handler = next; return () => undefined },
      invoke: async (command) => {
        if (command === 'get_update_snapshot') return snapshot({ revision: 2, phase: 'ready' })
        if (command === 'install_update') return install.promise
        if (command === 'restart_for_update') throw new Error('synthetic restart failure')
        throw new Error(`unexpected command: ${command}`)
      },
    })
    const container = await renderUpdates()
    await act(async () => { getButton(container, 'Restart and update').click(); await Promise.resolve() })
    await act(async () => {
      handler({ payload: snapshot({ revision: 5, phase: 'restart_required', operation: null, error: null, reason: 'Restart required.' }) })
      install.resolve(snapshot({ revision: 4, phase: 'ready' }))
      await install.promise
    })
    await act(async () => { getButton(container, 'Restart VoxGolem').click(); await Promise.resolve() })
    expect(container.textContent).toContain('Restart failed: synthetic restart failure')
    expect(getButton(container, 'Restart VoxGolem')).toBeInstanceOf(HTMLButtonElement)
  })

  it('offers phase-aware retry while retaining the available update', async () => {
    const invoke = vi.fn(async (command: string) => {
      if (command === 'get_update_snapshot') return snapshot({ phase: 'available', error: 'offline' })
      if (command === 'download_update') return snapshot({ revision: 3, phase: 'ready', error: null })
      throw new Error(`unexpected command: ${command}`)
    })
    installTauri({ invoke })
    const container = await renderUpdates()
    expect(container.textContent).toContain('Update failed: offline')
    await act(async () => { getButton(container, 'Retry').click(); await Promise.resolve() })
    expect(invoke).toHaveBeenCalledWith('download_update', undefined)
    expect(container.textContent).toContain('Update 2.0.0 is ready')
  })

  it('Later discards ready state and the preference commits only on successful persistence', async () => {
    let saveAttempts = 0
    const invoke = vi.fn(async (command: string, args?: unknown) => {
      if (command === 'get_update_snapshot') return snapshot({ phase: 'ready' })
      if (command === 'discard_update') return snapshot({ revision: 3, phase: 'idle', version: null, notes: null })
      if (command === 'set_auto_update_download') {
        saveAttempts += 1
        if (saveAttempts === 1) throw new Error('disk unavailable')
        expect(args).toEqual({ enabled: false })
        return snapshot({ revision: 4, phase: 'idle', version: null, auto_download_enabled: false })
      }
      throw new Error(`unexpected command: ${command}`)
    })
    installTauri({ invoke })
    const container = await renderUpdates()
    await act(async () => { getButton(container, 'Later').click(); await Promise.resolve() })
    expect(getButton(container, 'Check')).toBeInstanceOf(HTMLButtonElement)
    const checkbox = getCheckbox(container, 'Download updates automatically')
    await act(async () => { checkbox.click(); await Promise.resolve() })
    expect(checkbox.checked).toBe(true)
    expect(container.textContent).toContain('disk unavailable')
    await act(async () => { getButton(container, 'Retry saving').click(); await Promise.resolve() })
    expect(checkbox.checked).toBe(false)
  })

  it('persists the independent default-on automatic install preference', async () => {
    const invoke = vi.fn(async (command: string, args?: unknown) => {
      if (command === 'get_update_snapshot') return snapshot()
      if (command === 'set_auto_update_install') {
        expect(args).toEqual({ enabled: false })
        return snapshot({ revision: 3, auto_install_enabled: false })
      }
      throw new Error(`unexpected command: ${command}`)
    })
    installTauri({ invoke })
    const container = await renderUpdates()
    const download = getCheckbox(container, 'Download updates automatically')
    const install = getCheckbox(container, 'Install downloaded updates automatically')
    expect(download.checked).toBe(true)
    expect(install.checked).toBe(true)

    await act(async () => { install.click(); await Promise.resolve() })

    expect(download.checked).toBe(true)
    expect(install.checked).toBe(false)
  })

  it('renders update notes as plain text, including empty and long strings', async () => {
    const notes = `${'x'.repeat(5_000)}<b>literal</b>`
    installTauri({ invoke: async () => snapshot({ phase: 'ready', notes }) })
    const container = await renderUpdates()
    expect(container.querySelector('.settings-panel__update-notes')?.textContent).toBe(notes)
    expect(container.querySelector('.settings-panel__update-notes b')).toBeNull()
  })
})

function installTauri(overrides: Partial<NonNullable<typeof window.__TAURI_INTERNALS__>>): void {
  window.__TAURI_INTERNALS__ = {
    invoke: async (command) => {
      if (command === 'get_update_snapshot') return snapshot()
      throw new Error(`unexpected command: ${command}`)
    },
    listen: async () => () => undefined,
    ...overrides,
  }
}

function snapshot(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    revision: 2,
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

async function renderUpdates(waitForHydration = true): Promise<HTMLElement> {
  const container = document.createElement('div')
  document.body.append(container)
  containers.push(container)
  const root = createRoot(container)
  roots.push(root)
  await act(async () => {
    root.render(<UpdateHost />)
    if (waitForHydration) {
      await Promise.resolve()
      await Promise.resolve()
    }
  })
  return container
}

function UpdateHost() {
  return <UpdateSettings updates={useAppUpdates()} />
}

function getButton(container: HTMLElement, name: string): HTMLButtonElement {
  const button = Array.from(container.querySelectorAll('button')).find((candidate) => candidate.textContent === name)
  if (!(button instanceof HTMLButtonElement)) throw new Error(`button not found: ${name}; ${container.textContent}`)
  return button
}

function getCheckbox(container: HTMLElement, name: string): HTMLInputElement {
  const label = Array.from(container.querySelectorAll('label')).find((candidate) => candidate.textContent?.includes(name))
  const checkbox = label?.querySelector<HTMLInputElement>('input[type="checkbox"]')
  if (checkbox === null || checkbox === undefined) throw new Error(`checkbox not found: ${name}`)
  return checkbox
}

function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((resolvePromise) => { resolve = resolvePromise })
  return { promise, resolve }
}

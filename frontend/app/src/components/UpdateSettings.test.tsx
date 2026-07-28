import { act, useState } from 'react'
import type { JSX } from 'react'
import { createRoot } from 'react-dom/client'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { UpdateSettings } from './UpdateSettings'
import { useAppUpdates } from '../lib/useAppUpdates'

const containers: HTMLElement[] = []
const roots: ReturnType<typeof createRoot>[] = []

afterEach(() => {
  for (const root of roots) act(() => root.unmount())
  for (const container of containers) container.remove()
  roots.length = 0
  containers.length = 0
  Reflect.deleteProperty(window, '__TAURI_INTERNALS__')
})

describe('UpdateSettings', () => {
  it('checks automatically but installs and restarts only after separate user actions', async () => {
    const invoke = vi.fn(async (command: string) => {
      if (command === 'check_for_update') return {
        status: 'available',
        current_version: '0.1.0',
        version: '2026.7.27-12',
        notes: 'Safer updates',
      }
      if (command === 'install_update') return { version: '2026.7.27-12' }
      if (command === 'restart_for_update') return null
      throw new Error(`unexpected command: ${command}`)
    })
    window.__TAURI_INTERNALS__ = { invoke }

    const container = await renderUpdates()
    expect(container.textContent).toContain('Update available')
    expect(container.querySelector('[role="status"]')?.textContent).toBe('Update available')
    expect(container.querySelector('[role="alert"]')).toBeNull()
    expect(container.textContent).not.toContain('You have')
    expect(container.textContent).not.toContain('Signed Linux AppImage updates')
    expect(container.querySelectorAll('.settings-panel__update-row')).toHaveLength(1)
    expect(invoke).toHaveBeenCalledWith('check_for_update', undefined)
    expect(invoke).not.toHaveBeenCalledWith('install_update', undefined)

    await act(async () => {
      getButton(container, 'Install update').click()
      await Promise.resolve()
    })
    expect(container.textContent).toContain('Update installed')
    expect(container.querySelector('[role="status"]')?.textContent).toBe('Update installed')
    expect(container.querySelectorAll('[role="status"]')).toHaveLength(1)
    expect(invoke).toHaveBeenCalledWith('install_update', undefined)

    await act(async () => {
      getButton(container, 'Restart').click()
      await Promise.resolve()
    })
    expect(invoke).toHaveBeenCalledWith('restart_for_update', undefined)
  })

  it('keeps unsupported packages and update failures localized and retryable', async () => {
    let checks = 0
    window.__TAURI_INTERNALS__ = {
      invoke: vi.fn(async (command: string) => {
        if (command !== 'check_for_update') throw new Error(`unexpected command: ${command}`)
        checks += 1
        if (checks === 1) throw new Error('release endpoint unavailable')
        return {
          status: 'unsupported',
          current_version: '0.1.0',
          reason: 'Automatic updates require the Linux AppImage.',
        }
      }),
    }

    const container = await renderUpdates()
    expect(container.textContent).toContain('release endpoint unavailable')

    await act(async () => {
      getButton(container, 'Retry').click()
      await Promise.resolve()
    })
    expect(container.textContent).toContain('Automatic updates require the Linux AppImage.')
  })

  it('presents a missing updater-enabled release as informational', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: vi.fn(async (command: string) => {
        if (command !== 'check_for_update') throw new Error(`unexpected command: ${command}`)
        return {
          status: 'unavailable',
          current_version: '0.1.0',
          reason: 'No published updates yet.',
        }
      }),
    }

    const container = await renderUpdates()
    expect(container.textContent).toContain('No published updates yet.')
    expect(container.textContent).not.toContain('Update failed')
    expect(container.querySelector('[role="alert"]')).toBeNull()
  })

  it('preserves an in-flight install when settings closes and does not check twice', async () => {
    let finishInstall: (result: { version: string }) => void = () => undefined
    const pendingInstall = new Promise<{ version: string }>((resolve) => { finishInstall = resolve })
    const invoke = vi.fn(async (command: string) => {
      if (command === 'check_for_update') return {
        status: 'available', current_version: '0.1.0', version: '2026.7.27-12', notes: null,
      }
      if (command === 'install_update') return pendingInstall
      throw new Error(`unexpected command: ${command}`)
    })
    window.__TAURI_INTERNALS__ = { invoke }

    const container = await renderUpdates()
    await act(async () => {
      getButton(container, 'Install update').click()
      await Promise.resolve()
    })
    await act(async () => {
      getButton(container, 'Hide updates').click()
    })
    await act(async () => {
      getButton(container, 'Show updates').click()
    })

    expect(container.textContent).toContain('Downloading and verifying...')
    expect(container.textContent).not.toContain('2026.7.27-12')
    expect(invoke.mock.calls.filter(([command]) => command === 'check_for_update')).toHaveLength(1)

    await act(async () => {
      finishInstall({ version: '2026.7.27-12' })
      await pendingInstall
    })
    expect(container.textContent).toContain('Update installed')
    expect(container.textContent).not.toContain('2026.7.27-12')
  })

  it('reports install failures and retries the original version', async () => {
    let installs = 0
    const invoke = vi.fn(async (command: string) => {
      if (command === 'check_for_update') {
        return { status: 'available', current_version: '0.1.0', version: '2026.7.27-12', notes: null }
      }
      if (command === 'install_update') {
        installs += 1
        if (installs === 1) throw new Error('signature rejected')
        return { version: '2026.7.27-12' }
      }
      throw new Error(`unexpected command: ${command}`)
    })
    window.__TAURI_INTERNALS__ = { invoke }

    const container = await renderUpdates()
    await act(async () => {
      getButton(container, 'Install update').click()
      await Promise.resolve()
    })
    expect(container.querySelector('[role="alert"]')?.textContent).toBe('Install failed: signature rejected')

    await act(async () => {
      getButton(container, 'Retry').click()
      await Promise.resolve()
    })
    expect(invoke.mock.calls.filter(([command]) => command === 'install_update')).toHaveLength(2)
  })

  it('keeps a maximum-length unbroken error and Retry action in the update row', async () => {
    const message = 'x'.repeat(2_048)
    window.__TAURI_INTERNALS__ = {
      invoke: vi.fn(async (command: string) => {
        if (command === 'check_for_update') throw new Error(message)
        throw new Error(`unexpected command: ${command}`)
      }),
    }

    const container = await renderUpdates()
    const row = container.querySelector('.settings-panel__update-row')
    expect(row?.querySelector('[role="alert"]')?.textContent).toBe(`Check failed: ${message}`)
    expect(getButton(container, 'Retry')).toBeInstanceOf(HTMLButtonElement)
  })

  it('reports restart failures and retries restart', async () => {
    let restarts = 0
    const invoke = vi.fn(async (command: string) => {
      if (command === 'check_for_update') {
        return { status: 'available', current_version: '0.1.0', version: '2026.7.27-12', notes: null }
      }
      if (command === 'install_update') return { version: '2026.7.27-12' }
      if (command === 'restart_for_update') {
        restarts += 1
        if (restarts === 1) throw new Error('restart denied')
        return null
      }
      throw new Error(`unexpected command: ${command}`)
    })
    window.__TAURI_INTERNALS__ = { invoke }

    const container = await renderUpdates()
    await act(async () => {
      getButton(container, 'Install update').click()
      await Promise.resolve()
    })
    await act(async () => {
      getButton(container, 'Restart').click()
      await Promise.resolve()
    })
    expect(container.querySelector('[role="alert"]')?.textContent).toBe('Restart failed: restart denied')

    await act(async () => {
      getButton(container, 'Retry').click()
      await Promise.resolve()
    })
    expect(invoke.mock.calls.filter(([command]) => command === 'restart_for_update')).toHaveLength(2)
  })
})

async function renderUpdates(): Promise<HTMLElement> {
  const container = document.createElement('div')
  document.body.append(container)
  containers.push(container)
  const root = createRoot(container)
  roots.push(root)
  await act(async () => {
    root.render(<UpdateTestHost />)
    await Promise.resolve()
  })
  return container
}

function UpdateTestHost(): JSX.Element {
  const updates = useAppUpdates()
  const [visible, setVisible] = useState(true)
  return (
    <div>
      <button type="button" onClick={() => setVisible(false)}>Hide updates</button>
      <button type="button" onClick={() => setVisible(true)}>Show updates</button>
      {visible ? <UpdateSettings updates={updates} /> : null}
    </div>
  )
}

function getButton(container: HTMLElement, name: string): HTMLButtonElement {
  const button = Array.from(container.querySelectorAll('button')).find((candidate) => candidate.textContent === name)
  if (!(button instanceof HTMLButtonElement)) throw new Error(`button not found: ${name}`)
  return button
}

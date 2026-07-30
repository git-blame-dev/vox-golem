import { act, useState } from 'react'
import type { JSX } from 'react'
import { createRoot } from 'react-dom/client'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { UpdateSettings } from './UpdateSettings'
import { useAppUpdates } from '../lib/useAppUpdates'
import type { TauriEvent } from '../lib/tauri'

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
         install_behavior: 'install_then_restart',
      }
      if (command === 'install_update') return { version: '2026.7.27-12' }
      if (command === 'restart_for_update') return null
      throw new Error(`unexpected command: ${command}`)
    })
    window.__TAURI_INTERNALS__ = { invoke }

    const container = await renderUpdates()
    expect(container.textContent).toContain('Update 2026.7.27-12 available')
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
    expect(invoke).toHaveBeenCalledWith('install_update', undefined)

    await act(async () => {
      getButton(container, 'Restart').click()
      await Promise.resolve()
    })
    expect(invoke).toHaveBeenCalledWith('restart_for_update', undefined)
  })

  it('submits one install when the action is activated twice before rendering', async () => {
    let finishInstall: (result: { version: string }) => void = () => undefined
    const pendingInstall = new Promise<{ version: string }>((resolve) => { finishInstall = resolve })
    const invoke = vi.fn(async (command: string) => {
      if (command === 'check_for_update') return {
        status: 'available', current_version: '0.1.0', version: '2026.7.27-12', notes: null, install_behavior: 'install_and_restart',
      }
      if (command === 'install_update') return pendingInstall
      throw new Error(`unexpected command: ${command}`)
    })
    window.__TAURI_INTERNALS__ = { invoke }

    const container = await renderUpdates()
    await act(async () => {
      const install = getButton(container, 'Install and restart')
      install.click()
      install.click()
      await Promise.resolve()
    })

    expect(invoke.mock.calls.filter(([command]) => command === 'install_update')).toHaveLength(1)
    await act(async () => {
      finishInstall({ version: '2026.7.27-12' })
      await pendingInstall
    })
  })

  it('submits one restart and disables the action while it is pending', async () => {
    let finishRestart: () => void = () => undefined
    const pendingRestart = new Promise<void>((resolve) => { finishRestart = resolve })
    const invoke = vi.fn(async (command: string) => {
      if (command === 'check_for_update') return {
        status: 'available', current_version: '0.1.0', version: '2026.7.27-12', notes: null, install_behavior: 'install_then_restart',
      }
      if (command === 'install_update') return { version: '2026.7.27-12' }
      if (command === 'restart_for_update') return pendingRestart
      throw new Error(`unexpected command: ${command}`)
    })
    window.__TAURI_INTERNALS__ = { invoke }

    const container = await renderUpdates()
    await act(async () => {
      getButton(container, 'Install update').click()
      await Promise.resolve()
    })
    await act(async () => {
      const restart = getButton(container, 'Restart')
      restart.click()
      restart.click()
      await Promise.resolve()
    })

    expect(invoke.mock.calls.filter(([command]) => command === 'restart_for_update')).toHaveLength(1)
    expect(getButton(container, 'Restart').disabled).toBe(true)
    await act(async () => {
      finishRestart()
      await pendingRestart
    })
  })

  it('renders filtered native download and verification progress', async () => {
    let progressHandler: (event: TauriEvent) => void = () => undefined
    let finishInstall: (result: { version: string }) => void = () => undefined
    const pendingInstall = new Promise<{ version: string }>((resolve) => { finishInstall = resolve })
    window.__TAURI_INTERNALS__ = {
      invoke: vi.fn(async (command: string) => {
        if (command === 'check_for_update') return {
          status: 'available', current_version: '0.1.0', version: '2026.7.27-12', notes: null, install_behavior: 'install_and_restart',
        }
        if (command === 'install_update') return pendingInstall
        throw new Error(`unexpected command: ${command}`)
      }),
      listen: async (event, handler) => {
        if (event === 'app-update-progress') progressHandler = handler
        return () => undefined
      },
    }

    const container = await renderUpdates()
    await act(async () => {
      getButton(container, 'Install and restart').click()
      await Promise.resolve()
    })
    expect(container.textContent).toContain('Downloading and verifying...')

    await act(async () => {
      progressHandler({ payload: { version: 'other-version', phase: 'progress', downloaded_bytes: 9_000_000, total_bytes: 10_000_000 } })
      progressHandler({ payload: { version: '2026.7.27-12', phase: 'progress', downloaded_bytes: -1 } })
    })
    expect(container.textContent).toContain('Downloading and verifying...')

    await act(async () => {
      progressHandler({ payload: { version: '2026.7.27-12', phase: 'started', downloaded_bytes: 0, total_bytes: 10_000_000 } })
    })
    expect(container.textContent).toContain('Downloading: 0% (0.0 / 10.0 MB)')
    await act(async () => {
      progressHandler({ payload: { version: '2026.7.27-12', phase: 'progress', downloaded_bytes: 5_000_000, total_bytes: 10_000_000 } })
    })
    expect(container.textContent).toContain('Downloading: 50% (5.0 / 10.0 MB)')
    await act(async () => {
      progressHandler({ payload: { version: '2026.7.27-12', phase: 'verifying', downloaded_bytes: 10_000_000, total_bytes: 10_000_000 } })
    })
    expect(container.textContent).toContain('Verifying 10.0 MB...')
    await act(async () => {
      progressHandler({ payload: { version: '2026.7.27-12', phase: 'installing', downloaded_bytes: 10_000_000, total_bytes: 10_000_000 } })
    })
    expect(container.textContent).toContain('Starting installer...')

    await act(async () => {
      finishInstall({ version: '2026.7.27-12' })
      await pendingInstall
    })
  })

  it('ignores native progress-listener setup failures', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: vi.fn(async () => ({ status: 'up_to_date', current_version: '0.1.0' })),
      listen: async () => { throw new Error('listener unavailable') },
    }

    const container = await renderUpdates()
    await act(async () => { await Promise.resolve() })
    expect(container.textContent).toContain('Up to date')
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
    expect(container.textContent).toContain('Update failed: release endpoint unavailable')
    expect(container.textContent).not.toContain('Check failed:')

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

  it('disables an available update while application work is active', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: vi.fn(async (command: string) => {
        if (command !== 'check_for_update') throw new Error(`unexpected command: ${command}`)
        return {
          status: 'available',
          current_version: '0.1.0',
          version: '2026.7.27-12',
          notes: null,
          install_behavior: 'install_and_restart',
        }
      }),
    }

    const container = await renderUpdates(true)
    expect(getButton(container, 'Install and restart').disabled).toBe(true)
    expect(container.textContent).toContain('Update 2026.7.27-12 available. Finish active work before installing.')
  })

  it('preserves an in-flight install when settings closes and does not check twice', async () => {
    let finishInstall: (result: { version: string }) => void = () => undefined
    const pendingInstall = new Promise<{ version: string }>((resolve) => { finishInstall = resolve })
    const invoke = vi.fn(async (command: string) => {
      if (command === 'check_for_update') return {
         status: 'available', current_version: '0.1.0', version: '2026.7.27-12', notes: null, install_behavior: 'install_then_restart',
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
})

async function renderUpdates(installationDisabled = false): Promise<HTMLElement> {
  const container = document.createElement('div')
  document.body.append(container)
  containers.push(container)
  const root = createRoot(container)
  roots.push(root)
  await act(async () => {
    root.render(<UpdateTestHost installationDisabled={installationDisabled} />)
    await Promise.resolve()
  })
  return container
}

function UpdateTestHost({ installationDisabled }: { readonly installationDisabled: boolean }): JSX.Element {
  const updates = useAppUpdates()
  const [visible, setVisible] = useState(true)
  return (
    <div>
      <button type="button" onClick={() => setVisible(false)}>Hide updates</button>
      <button type="button" onClick={() => setVisible(true)}>Show updates</button>
      {visible ? <UpdateSettings updates={updates} installationDisabled={installationDisabled} /> : null}
    </div>
  )
}

function getButton(container: HTMLElement, name: string): HTMLButtonElement {
  const button = Array.from(container.querySelectorAll('button')).find((candidate) => candidate.textContent === name)
  if (!(button instanceof HTMLButtonElement)) throw new Error(`button not found: ${name}`)
  return button
}

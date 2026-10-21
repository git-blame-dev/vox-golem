import { act } from 'react'
import { createRoot } from 'react-dom/client'
import { afterEach, expect, it, vi } from 'vitest'
import { listenCapturePlayback } from './captures'
import { useCaptureHistory } from './useCaptureHistory'

afterEach(() => { delete window.__TAURI_INTERNALS__ })

function transport(register: () => Promise<unknown> = async () => undefined) {
  const callbacks = new Map<number, (value: unknown) => void>()
  const calls: Array<[string, Record<string, unknown>]> = []
  let id = 40
  window.__TAURI_INTERNALS__ = {
    invoke: async (command: string, args?: unknown) => {
      // Serialize the real Channel, as the production IPC transport does.
      calls.push([command, JSON.parse(JSON.stringify(args ?? {})) as Record<string, unknown>])
      if (command === 'plugin:capture|register_listener') return register()
      if (command === 'plugin:capture|remove_listener') return undefined
      if (command === 'plugin:capture|snapshot') return { available: true, captures: [], notesFolderUri: null, error: null }
      throw new Error('Unexpected synthetic IPC command')
    },
    ...{ transformCallback: (callback: (value: unknown) => void) => { callbacks.set(++id, callback); return id },
      unregisterCallback: (callbackId: number) => { callbacks.delete(callbackId) } },
  }
  return { calls, callbacks }
}

it('uses the real Tauri Channel serialization, projects messages and removes its exact channel', async () => {
  const { calls, callbacks } = transport()
  const changed = vi.fn()
  const stop = await listenCapturePlayback(changed)
  expect(calls).toEqual([['plugin:capture|register_listener', { event: 'playback', handler: '__CHANNEL__:41' }]])
  const message = { revision: 7, captureId: 'synthetic-capture', phase: 'completed' }
  callbacks.get(41)!({ index: 0, message })
  expect(changed).toHaveBeenCalledWith(message)
  stop()
  await Promise.resolve()
  expect(calls[1]).toEqual(['plugin:capture|remove_listener', { event: 'playback', channelId: 41 }])
})

it('the real history hook removes a listener whose registration resolves after unmount', async () => {
  let registered!: () => void
  const { calls } = transport(() => new Promise<void>((resolve) => { registered = resolve }))
  function HistoryLifetime() { useCaptureHistory(); return null }
  const container = document.createElement('div')
  const root = createRoot(container)
  await act(async () => { root.render(<HistoryLifetime />) })
  act(() => root.unmount())
  await act(async () => { registered() })
  expect(calls.filter(([command]) => command === 'plugin:capture|remove_listener')).toEqual([
    ['plugin:capture|remove_listener', { event: 'playback', channelId: 41 }],
  ])
})

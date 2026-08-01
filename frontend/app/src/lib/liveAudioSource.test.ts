import { afterEach, describe, expect, it, vi } from 'vitest'
import { listAudioInputDevices, startLiveAudioSource } from './liveAudioSource'

afterEach(() => {
  Reflect.deleteProperty(window, '__TAURI_INTERNALS__')
})

describe('native live audio source', () => {
  it('lists validated native input devices', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: vi.fn(async (command) => command === 'list_audio_input_devices'
        ? [{ device_id: 'mic-id', label: 'Studio microphone' }]
        : null),
    }

    await expect(listAudioInputDevices()).resolves.toEqual([
      { deviceId: 'mic-id', label: 'Studio microphone' },
    ])
  })

  it('starts native capture and delivers only matching frames sequentially', async () => {
    const handlers = new Map<string, (event: { payload: unknown }) => void>()
    const invoke = vi.fn<(command: string, args?: unknown) => Promise<unknown>>(async (command) => {
      if (command === 'reserve_native_microphone_capture_id') return 41
      if (command === 'start_native_microphone') return { fell_back_to_default: false }
      return null
    })
    window.__TAURI_INTERNALS__ = {
      invoke,
      listen: vi.fn(async (event, handler) => {
        handlers.set(event, handler)
        return vi.fn()
      }),
    }
    const received: number[][] = []
    const statuses: string[] = []
    const source = await startLiveAudioSource({
      onFrame: async (frame) => { received.push([...frame]) },
      onError: () => undefined,
      onStatus: (status) => statuses.push(status),
    })
    const startArgs = invoke.mock.calls.find(([command]) => command === 'start_native_microphone')?.[1]
    const captureId = (startArgs as { captureId: number }).captureId
    const frameHandler = handlers.get('native-microphone-frame')
    expect(frameHandler).toBeDefined()
    expect(captureId).toBe(41)

    frameHandler?.({ payload: { capture_id: captureId + 1, frame: [9] } })
    frameHandler?.({ payload: { capture_id: captureId, frame: [1, 2] } })
    frameHandler?.({ payload: { capture_id: captureId, frame: [3, 4] } })
    await vi.waitFor(() => expect(received).toEqual([[1, 2], [3, 4]]))
    expect(statuses).toEqual(['starting_native_input', 'native_input_started', 'first_frame'])

    source.stop()
    expect(invoke).toHaveBeenCalledWith('stop_native_microphone', { captureId })
  })

  it('uses process-lifetime native IDs after the frontend module reloads', async () => {
    const reservedIds = [401, 902]
    const invoke = vi.fn<(command: string, args?: unknown) => Promise<unknown>>(async (command) => {
      if (command === 'reserve_native_microphone_capture_id') return reservedIds.shift()
      if (command === 'start_native_microphone') return { fell_back_to_default: false }
      return null
    })
    window.__TAURI_INTERNALS__ = {
      invoke,
      listen: vi.fn(async () => vi.fn()),
    }

    const first = await startLiveAudioSource({ onFrame: () => undefined, onError: () => undefined })
    vi.resetModules()
    const reloaded = await import('./liveAudioSource')
    const second = await reloaded.startLiveAudioSource({ onFrame: () => undefined, onError: () => undefined })
    const captureIds = invoke.mock.calls
      .filter(([command]) => command === 'start_native_microphone')
      .map(([, args]) => (args as { captureId: number }).captureId)

    expect(captureIds).toEqual([401, 902])
    first.stop()
    second.stop()
  })

  it('reports only matching native capture failures and cleans up both listeners', async () => {
    const handlers = new Map<string, (event: { payload: unknown }) => void>()
    const unlistenFrame = vi.fn()
    const unlistenTerminal = vi.fn()
    const onError = vi.fn()
    window.__TAURI_INTERNALS__ = {
      invoke: vi.fn(async (command) => {
        if (command === 'reserve_native_microphone_capture_id') return 42
        if (command === 'start_native_microphone') return { fell_back_to_default: false }
        return null
      }),
      listen: vi.fn(async (event, handler) => {
        handlers.set(event, handler)
        return event === 'native-microphone-frame' ? unlistenFrame : unlistenTerminal
      }),
    }

    const source = await startLiveAudioSource({
      onFrame: () => undefined,
      onError,
    })
    const terminalHandler = handlers.get('native-microphone-terminal')
    const frameHandler = handlers.get('native-microphone-frame')
    expect(terminalHandler).toBeDefined()
    expect(frameHandler).toBeDefined()
    const startCall = vi.mocked(window.__TAURI_INTERNALS__.invoke).mock.calls
      .find(([command]) => command === 'start_native_microphone')
    const captureId = (startCall?.[1] as { captureId: number }).captureId

    terminalHandler?.({ payload: { capture_id: captureId + 1, message: 'stale failure' } })
    expect(onError).not.toHaveBeenCalled()
    terminalHandler?.({ payload: { capture_id: captureId, message: 'device removed' } })
    expect(onError).toHaveBeenCalledOnce()
    expect(onError).toHaveBeenCalledWith(expect.objectContaining({ message: 'device removed' }))
    expect(unlistenFrame).toHaveBeenCalledOnce()
    expect(unlistenTerminal).toHaveBeenCalledOnce()

    frameHandler?.({ payload: { capture_id: captureId, frame: [1, 2] } })
    source.stop()
    expect(onError).toHaveBeenCalledOnce()
  })

  it('stops capture instead of retaining an unbounded frame backlog', async () => {
    const handlers = new Map<string, (event: { payload: unknown }) => void>()
    let releaseFirstFrame: () => void = () => undefined
    const firstFrameBlocked = new Promise<void>((resolve) => {
      releaseFirstFrame = resolve
    })
    const onFrame = vi.fn(async () => firstFrameBlocked)
    const onError = vi.fn()
    const invoke = vi.fn<(command: string, args?: unknown) => Promise<unknown>>(async (command) => {
      if (command === 'reserve_native_microphone_capture_id') return 43
      if (command === 'start_native_microphone') return { fell_back_to_default: false }
      return null
    })
    window.__TAURI_INTERNALS__ = {
      invoke,
      listen: vi.fn(async (event, handler) => {
        handlers.set(event, handler)
        return vi.fn()
      }),
    }
    await startLiveAudioSource({ onFrame, onError })
    const startCall = invoke.mock.calls.find(([command]) => command === 'start_native_microphone')
    const captureId = (startCall?.[1] as { captureId: number }).captureId
    const frameHandler = handlers.get('native-microphone-frame')

    for (let index = 0; index < 40; index += 1) {
      frameHandler?.({ payload: { capture_id: captureId, frame: [index] } })
    }

    await vi.waitFor(() => expect(onError).toHaveBeenCalledOnce())
    expect(onError).toHaveBeenCalledWith(expect.objectContaining({ message: expect.stringContaining('fell behind') }))
    expect(invoke).toHaveBeenCalledWith('stop_native_microphone', { captureId })
    releaseFirstFrame()
    await Promise.resolve()
    expect(onFrame).toHaveBeenCalledOnce()
  })

  it('cancels native startup before the start command settles', async () => {
    let resolveStart: (payload: unknown) => void = () => undefined
    const startPending = new Promise<unknown>((resolve) => {
      resolveStart = resolve
    })
    const invoke = vi.fn<(command: string, args?: unknown) => Promise<unknown>>(async (command) => {
      if (command === 'reserve_native_microphone_capture_id') return 44
      if (command === 'start_native_microphone') return startPending
      return null
    })
    window.__TAURI_INTERNALS__ = {
      invoke,
      listen: vi.fn(async () => vi.fn()),
    }
    const controller = new AbortController()
    const sourcePending = startLiveAudioSource({
      signal: controller.signal,
      onFrame: () => undefined,
      onError: () => undefined,
    })
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith(
      'start_native_microphone',
      expect.objectContaining({ captureId: expect.any(Number) }),
    ))
    const startCall = invoke.mock.calls.find(([command]) => command === 'start_native_microphone')
    const captureId = (startCall?.[1] as { captureId: number }).captureId

    controller.abort()

    await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith('stop_native_microphone', { captureId }))
    resolveStart({ fell_back_to_default: false })
    const source = await sourcePending
    source.stop()
    expect(invoke.mock.calls.filter(([command]) => command === 'stop_native_microphone')).toHaveLength(1)
  })

  it('reports stale selected-device fallback from the native service', async () => {
    const onSelectedDeviceFallback = vi.fn()
    window.__TAURI_INTERNALS__ = {
      invoke: vi.fn(async (command) => {
        if (command === 'reserve_native_microphone_capture_id') return 45
        if (command === 'start_native_microphone') return { fell_back_to_default: true }
        return null
      }),
      listen: vi.fn(async () => vi.fn()),
    }

    const source = await startLiveAudioSource({
      deviceId: 'missing microphone',
      onFrame: () => undefined,
      onError: () => undefined,
      onSelectedDeviceFallback,
    })

    expect(onSelectedDeviceFallback).toHaveBeenCalledWith('missing microphone')
    source.stop()
  })

  it('cleans up the listener when native startup fails', async () => {
    const unlistenFrame = vi.fn()
    const unlistenTerminal = vi.fn()
    window.__TAURI_INTERNALS__ = {
      invoke: vi.fn(async (command) => {
        if (command === 'reserve_native_microphone_capture_id') return 46
        if (command === 'start_native_microphone') throw new Error('input unavailable')
        return null
      }),
      listen: vi.fn(async (event) => event === 'native-microphone-frame'
        ? unlistenFrame
        : unlistenTerminal),
    }

    await expect(startLiveAudioSource({
      onFrame: () => undefined,
      onError: () => undefined,
    })).rejects.toThrow('input unavailable')
    expect(unlistenFrame).toHaveBeenCalledOnce()
    expect(unlistenTerminal).toHaveBeenCalledOnce()
  })
})

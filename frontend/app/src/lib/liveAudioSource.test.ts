import { afterEach, describe, expect, it, vi } from 'vitest'
import { listAudioInputDevices, startLiveAudioSource } from './liveAudioSource'

afterEach(() => { delete window.__TAURI_INTERNALS__ })
describe('native microphone control handle', () => {
  it.each(['completed', 'failed'] as const)('exposes the same physical-retirement promise to every Stop caller: %s', async (outcome) => {
    let resolve!: (value: unknown) => void
    let reject!: (error: Error) => void
    const pending = new Promise((yes, no) => { resolve = yes; reject = no })
    const onError = vi.fn()
    const invoke = vi.fn(async (command: string) => {
      if (command === 'reserve_native_microphone_capture_id') return 21
      if (command === 'start_native_microphone') return { fell_back_to_default: false }
      return pending
    })
    window.__TAURI_INTERNALS__ = { invoke }
    const source = await startLiveAudioSource({ onError })
    const retired = source.stop()
    expect(source.stop()).toBe(retired)
    let settled = false
    void retired.then(() => { settled = true }, () => { settled = true })
    await Promise.resolve()
    expect(settled).toBe(false)
    if (outcome === 'completed') { resolve(null); await expect(retired).resolves.toBeUndefined() }
    else { reject(new Error('Synthetic retirement failed')); await expect(retired).rejects.toThrow('Synthetic retirement failed') }
    expect(settled).toBe(true)
    expect(onError).toHaveBeenCalledTimes(outcome === 'failed' ? 1 : 0)
    expect(invoke.mock.calls.filter(([command]) => command === 'stop_native_microphone')).toHaveLength(1)
  })
  it('reserves process lifetime identity and starts/stops without any frame subscription', async () => {
    const listen = vi.fn()
    const invoke = vi.fn(async (command: string) => {
      if (command === 'reserve_native_microphone_capture_id') return 17
      if (command === 'start_native_microphone') return { fell_back_to_default: false }
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke, listen }
    const source = await startLiveAudioSource({ deviceId: 'synthetic-input', onError: vi.fn() })
    expect(source.captureId).toBe(17)
    expect(invoke.mock.calls).toEqual([['reserve_native_microphone_capture_id'], ['start_native_microphone', { captureId: 17, deviceId: 'synthetic-input' }]])
    expect(listen).not.toHaveBeenCalled()
    source.stop(); source.stop()
    expect(invoke).toHaveBeenLastCalledWith('stop_native_microphone', { captureId: 17 })
    expect(invoke).toHaveBeenCalledTimes(3)
  })
  it('detach and subsequent renderer abort are observation loss, not native capture cancellation', async () => {
    const invoke = vi.fn(async (command: string) => command === 'reserve_native_microphone_capture_id' ? 18 : { fell_back_to_default: false })
    window.__TAURI_INTERNALS__ = { invoke }
    const abort = new AbortController()
    const source = await startLiveAudioSource({ signal: abort.signal, onError: vi.fn() })
    source.detach(); abort.abort(); source.stop()
    expect(invoke).toHaveBeenCalledTimes(2)
  })
  it('preserves explicit startup abort and selected device fallback', async () => {
    const invoke = vi.fn(async (command: string) => command === 'reserve_native_microphone_capture_id' ? 19 : { fell_back_to_default: true })
    window.__TAURI_INTERNALS__ = { invoke }
    const fallback = vi.fn()
    const abort = new AbortController()
    const source = await startLiveAudioSource({ deviceId: 'synthetic-removed', signal: abort.signal, onError: vi.fn(), onSelectedDeviceFallback: fallback })
    expect(fallback).toHaveBeenCalledWith('synthetic-removed')
    abort.abort(); source.stop()
    expect(invoke.mock.calls.filter(([command]) => command === 'stop_native_microphone')).toHaveLength(1)
  })
  it('rejects unsafe process identities without starting capture', async () => {
    const invoke = vi.fn().mockResolvedValue(Number.MAX_SAFE_INTEGER + 1)
    window.__TAURI_INTERNALS__ = { invoke }
    await expect(startLiveAudioSource({ onError: vi.fn() })).rejects.toThrow(/capture ID/)
    expect(invoke).toHaveBeenCalledTimes(1)
  })
  it('lists native input selection without requesting capture', async () => {
    const invoke = vi.fn().mockResolvedValue([{ device_id: 'synthetic-input', label: 'Synthetic microphone' }])
    window.__TAURI_INTERNALS__ = { invoke }
    expect(await listAudioInputDevices()).toEqual([{ deviceId: 'synthetic-input', label: 'Synthetic microphone' }])
    expect(invoke).toHaveBeenCalledWith('list_audio_input_devices')
  })
})

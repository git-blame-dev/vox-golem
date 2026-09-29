import { act } from 'react'
import { createRoot } from 'react-dom/client'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { WakeClipSettings } from './WakeClipSettings'

afterEach(() => { Reflect.deleteProperty(window, '__TAURI_INTERNALS__') })

describe('WakeClipSettings', () => {
  it('starts enabled and updates the directory and true/false labels without touching the microphone', async () => {
    const invoke = vi.fn(async (command: string, args?: unknown): Promise<unknown> => {
      if (command === 'get_wake_clip_settings') return { enabled: true, directory: '/tmp/synthetic-clips' }
      if (command === 'set_wake_clip_settings') return args
      if (command === 'list_wake_clips') return [{ id: 'sample-1', profile: 'hey_livekit-0123456789abcdef', model_file: 'hey_livekit.onnx', model_revision: '0123456789abcdef', created_ms: 42, confidence: 0.8, label: 'unreviewed', sample_rate_hz: 16000, sample_count: 1600 }]
      if (command === 'label_wake_clip') return { id: 'sample-1', profile: 'hey_livekit-0123456789abcdef', model_file: 'hey_livekit.onnx', model_revision: '0123456789abcdef', created_ms: 42, confidence: 0.8, label: (args as { label: string }).label, sample_rate_hz: 16000, sample_count: 1600 }
      throw new Error(`unexpected ${command}`)
    })
    window.__TAURI_INTERNALS__ = { invoke }
    const container = document.createElement('div')
    document.body.append(container)
    const root = createRoot(container)
    try {
      await act(async () => { root.render(<WakeClipSettings />) })
      const input = container.querySelector<HTMLInputElement>('input[aria-label="Wake clip directory"]')!
      expect(input.value).toBe('/tmp/synthetic-clips')
      const checkbox = container.querySelector<HTMLInputElement>('input[aria-label="Save wake clips"]')!
      expect(checkbox.checked).toBe(true)
      await act(async () => { checkbox.click() })
      expect(invoke).toHaveBeenCalledWith('set_wake_clip_settings', { enabled: false, directory: '/tmp/synthetic-clips' })
      const select = container.querySelector<HTMLSelectElement>('select[aria-label="Clip label"]')!
      await act(async () => { select.value = 'false_wake'; select.dispatchEvent(new Event('change', { bubbles: true })) })
      expect(invoke).toHaveBeenCalledWith('label_wake_clip', { profile: 'hey_livekit-0123456789abcdef', id: 'sample-1', label: 'false_wake' })
      expect(invoke.mock.calls.some(([command]) => command === 'start_native_microphone')).toBe(false)
    } finally {
      await act(async () => { root.unmount() })
      container.remove()
    }
  })

  it('fills the newest page from older clips after deletion without showing a phantom next page', async () => {
    const clip = (id: string) => ({ id, profile: 'hey_livekit-0123456789abcdef', model_file: id, model_revision: '0123456789abcdef', created_ms: 42, confidence: 0.8, label: 'unreviewed', sample_rate_hz: 16000, sample_count: 1600 })
    let deleted = false
    const invoke = vi.fn(async (command: string, args?: unknown): Promise<unknown> => {
      if (command === 'get_wake_clip_settings') return { enabled: true, directory: '/tmp/synthetic-clips' }
      if (command === 'list_wake_clips') {
        const offset = (args as { offset: number }).offset
        if (offset === 0) return deleted
          ? [...Array.from({ length: 99 }, (_, index) => clip(`new-${index + 1}`)), clip('old-101')]
          : Array.from({ length: 100 }, (_, index) => clip(`new-${index}`))
        return offset === 100 && !deleted ? [clip('old-101')] : []
      }
      if (command === 'delete_wake_clip') { deleted = true; return null }
      if (command === 'label_wake_clip') return { ...clip('old-101'), label: (args as { label: string }).label }
      throw new Error(`unexpected ${command}`)
    })
    window.__TAURI_INTERNALS__ = { invoke }
    const container = document.createElement('div')
    document.body.append(container)
    const root = createRoot(container)
    try {
      await act(async () => { root.render(<WakeClipSettings />) })
      const older = Array.from(container.querySelectorAll('button')).find((button) => button.textContent === 'Older clips')!
      expect(older.disabled).toBe(false)
      await act(async () => { older.click() })
      expect(invoke).toHaveBeenCalledWith('list_wake_clips', { offset: 100 })
      expect(container.textContent).toContain('old-101')
      const newer = Array.from(container.querySelectorAll('button')).find((button) => button.textContent === 'Newer clips')!
      await act(async () => { newer.click() })
      const firstDelete = Array.from(container.querySelectorAll<HTMLButtonElement>('li button')).find((button) => button.textContent === 'Delete')!
      await act(async () => { firstDelete.click() })
      expect(invoke).toHaveBeenCalledWith('delete_wake_clip', { profile: 'hey_livekit-0123456789abcdef', id: 'new-0' })
      expect(older.disabled).toBe(true)
      expect(container.textContent).toContain('old-101')
      const select = Array.from(container.querySelectorAll<HTMLSelectElement>('select[aria-label="Clip label"]')).at(-1)!
      await act(async () => { select.value = 'true_wake'; select.dispatchEvent(new Event('change', { bubbles: true })) })
      expect(invoke).toHaveBeenCalledWith('label_wake_clip', { profile: 'hey_livekit-0123456789abcdef', id: 'old-101', label: 'true_wake' })
    } finally {
      await act(async () => { root.unmount() })
      container.remove()
    }
  })

  it('keeps the newest requested page when list requests resolve out of order', async () => {
    const clip = (id: string) => ({ id, profile: 'hey_livekit-0123456789abcdef', model_file: id, model_revision: '0123456789abcdef', created_ms: 42, confidence: 0.8, label: 'unreviewed' })
    let resolveOlder!: (value: unknown) => void
    let resolveRefresh!: (value: unknown) => void
    let deferred = false
    let hundredOffsetRequests = 0
    const invoke = vi.fn((command: string, args?: unknown): Promise<unknown> => {
      if (command === 'get_wake_clip_settings') return Promise.resolve({ enabled: true, directory: '/tmp/synthetic-clips' })
      if (command === 'list_wake_clips') {
        const offset = (args as { offset: number }).offset
        if (offset === 100) {
          hundredOffsetRequests += 1
          if (hundredOffsetRequests === 1 || hundredOffsetRequests > 2) return Promise.resolve([clip('older-available')])
          return new Promise((resolve) => { resolveOlder = resolve })
        }
        if (offset === 0 && deferred) return new Promise((resolve) => { resolveRefresh = resolve })
        return Promise.resolve(offset === 0 ? Array.from({ length: 100 }, (_, index) => clip(`newest-${index}`)) : [])
      }
      throw new Error(`unexpected ${command}`)
    })
    window.__TAURI_INTERNALS__ = { invoke }
    const container = document.createElement('div')
    document.body.append(container)
    const root = createRoot(container)
    try {
      await act(async () => { root.render(<WakeClipSettings />) })
      deferred = true
      const older = Array.from(container.querySelectorAll('button')).find((button) => button.textContent === 'Older clips')!
      await act(async () => { older.click() })
      const refresh = Array.from(container.querySelectorAll('button')).find((button) => button.textContent === 'Refresh clips')!
      await act(async () => { refresh.click() })
      await act(async () => { resolveRefresh([clip('freshest')]) })
      await act(async () => { resolveOlder([clip('stale')]) })
      expect(container.textContent).toContain('freshest')
      expect(container.textContent).not.toContain('stale')
      expect(Array.from(container.querySelectorAll('button')).find((button) => button.textContent === 'Newer clips')?.disabled).toBe(true)
    } finally {
      await act(async () => { root.unmount() })
      container.remove()
    }
  })
})

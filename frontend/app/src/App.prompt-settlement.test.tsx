import { act } from 'react'
import { readFileSync } from 'node:fs'
import { createRoot } from 'react-dom/client'
import { afterEach, describe, expect, it, vi } from 'vitest'
import App from './App'
import { appSnapshot, canonicalTurn, defaultAssistantSettings, readyStartupState } from './test/sessionFixtures'

const smokeSource = readFileSync('../../scripts/android-e2e-smoke.py', 'utf8')
const observationQuery = smokeSource.match(/OBSERVE = """([\s\S]*?)"""/)?.[1]
if (observationQuery === undefined) throw new Error('Smoke helper observation query missing')
const observe = new Function('window', 'document', `return ${observationQuery}`) as (window: Window, document: Document) => Record<string, unknown>

afterEach(() => { delete window.__TAURI_INTERNALS__; window.history.replaceState(null, ''); vi.restoreAllMocks() })

describe('prompt terminal presentation and speech', () => {
  it.each(['enabled', 'disabled', 'stopped'] as const)('reconciles a nonblank completed answer with speech %s', async (policy) => {
    const container = document.createElement('div'); document.body.append(container)
    const root = createRoot(container)
    let finish!: (value: unknown) => void
    let requestId = ''
    let transcript: Record<string, unknown>[] = []
    let ttsEnabled = true
    const unexpected: string[] = []
    const invoke = vi.fn(async (command: string, args?: unknown): Promise<unknown> => {
      if (command === 'chatgpt_auth') { expect(args).toEqual({ action: 'status' }); return { phase: 'signed_out' } }
      if (command === 'get_startup_state') return readyStartupState({ voice_input_available: false, tts_enabled: true, prompt_cancellation_available: true })
      if (command === 'get_app_snapshot') return appSnapshot(transcript, { tts_enabled: ttsEnabled })
      if (command === 'get_assistant_settings') return defaultAssistantSettings()
      if (command === 'submit_prompt') { requestId = (args as { requestId: string }).requestId; return new Promise((resolve) => { finish = resolve }) }
      if (command === 'set_tts_enabled') { ttsEnabled = (args as { enabled: boolean }).enabled; return { enabled: ttsEnabled } }
      if (command === 'cancel_prompt') return true
      if (command === 'get_ui_theme') return { theme: 'dark' }
      if (command === 'get_ui_text_size') return { size: 'medium' }
      if (command === 'get_update_snapshot') return { revision: 1, phase: 'up_to_date', operation: null, current_version: 'synthetic-version', version: null, notes: null, progress_phase: null, downloaded_bytes: 0, total_bytes: null, error: null, reason: null, auto_download_enabled: false, auto_install_enabled: false }
      if (command === 'get_wake_clip_settings') return { enabled: false, directory: '/tmp/synthetic-clips' }
      if (command === 'list_wake_clips') return []
      if (command === 'list_audio_input_devices') return []
      if (['record_frontend_runtime_diagnostic', 'write_runtime_log', 'check_for_update'].includes(command)) return null
      unexpected.push(command)
      throw new Error(`Unexpected fixture command: ${command}`)
    })
    window.__TAURI_INTERNALS__ = { invoke, createChannel: <T,>(send: (value: T) => void) => ({ send }) }
    try {
      await act(async () => { root.render(<App />) })
      const composer = container.querySelector<HTMLTextAreaElement>('#promptComposer')!
      await act(async () => {
        Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')!.set!.call(composer, 'Synthetic pending question')
        composer.dispatchEvent(new Event('input', { bubbles: true }))
      })
      const send = container.querySelector<HTMLButtonElement>('button[type="submit"]')!
      expect(send.disabled).toBe(false)
      await act(async () => { send.click() })
      expect(invoke.mock.calls.filter(([command]) => command === 'submit_prompt')).toHaveLength(1)
      expect(requestId).not.toBe('')
      expect(container.querySelector('.message__thinking')).not.toBeNull()
      Object.assign(window, { __voxSmoke: { ids: [], prompt: 'Synthetic pending question' } })
      expect(observe(window, document)).toMatchObject({ admitted: true, request: requestId, outcome: 'pending', stage: 'running', kind: 'text', nonblank: false })
      if (policy === 'disabled') {
        await act(async () => { container.querySelector<HTMLButtonElement>('[aria-label="Settings"]')!.click() })
        await act(async () => { container.querySelector<HTMLInputElement>('#tts-toggle')!.click() })
        expect(ttsEnabled).toBe(false)
        await act(async () => { container.querySelector<HTMLButtonElement>('[aria-label="Close settings"]')!.click() })
      } else if (policy === 'stopped') {
        await act(async () => { container.querySelector<HTMLButtonElement>('button[aria-label="Stop"]')!.click() })
        expect(invoke.mock.calls.filter(([command]) => command === 'cancel_prompt')).toHaveLength(1)
      }
      transcript = [canonicalTurn('1', '2', 'Synthetic pending question', 'Synthetic completed answer.', requestId)]
      await act(async () => { finish({ request_id: requestId, outcome: 'completed', runtime_phase: 'sleeping' }) })
      expect(container.querySelector('.answer-stage__answer')?.textContent).toBe('Synthetic completed answer.')
      expect(container.querySelector('.message__thinking')).toBeNull()
      expect(observe(window, document)).toEqual({ admitted: true, request: requestId, outcome: 'completed', stage: 'completed', kind: 'text', runtimeStatus: 'sleeping', nonblank: true })
      expect(container.querySelector('.shell')?.getAttribute('data-runtime-status')).toBe('sleeping')
      expect(invoke.mock.calls.filter(([command]) => command === 'speak_local_tts')).toHaveLength(0)
      expect(invoke.mock.calls.filter(([command]) => command === 'set_native_audio_active')).toEqual([])
      expect(unexpected).toEqual([])
      expect(invoke.mock.calls.filter(([command]) => command === 'chatgpt_auth')).toEqual(policy === 'disabled' ? [['chatgpt_auth', { action: 'status' }]] : [])
    } finally {
      await act(async () => { root.unmount() }); container.remove()
      expect(invoke.mock.calls.filter(([command]) => command === 'set_native_audio_active')).toEqual([])
      Reflect.deleteProperty(window, '__voxSmoke')
    }
  })

  it('persists the desktop header speech preference without synthesizing or replaying streamed paragraphs in the renderer', async () => {
    const container = document.createElement('div'); document.body.append(container)
    const root = createRoot(container)
    let finish!: (value: unknown) => void
    let streamChannel: ((value: unknown) => void) | undefined
    let requestId = ''
    let transcript: Record<string, unknown>[] = []
    let ttsEnabled = false
    const invoke = vi.fn(async (command: string, args?: unknown): Promise<unknown> => {
      if (command === 'get_startup_state') return readyStartupState({ voice_input_available: false, tts_enabled: false, prompt_cancellation_available: true })
      if (command === 'get_app_snapshot') return appSnapshot(transcript, { tts_enabled: ttsEnabled })
      if (command === 'get_assistant_settings') return defaultAssistantSettings()
      if (command === 'set_tts_enabled') { ttsEnabled = (args as { enabled: boolean }).enabled; return { enabled: ttsEnabled } }
      if (command === 'submit_prompt') {
        requestId = (args as { requestId: string }).requestId
        streamChannel = (args as { updates?: { send: (value: unknown) => void } }).updates?.send
        return new Promise((resolve) => { finish = resolve })
      }
      if (command === 'get_ui_theme') return { theme: 'dark' }
      if (command === 'get_ui_text_size') return { size: 'medium' }
      if (command === 'get_update_snapshot') return { revision: 1, phase: 'up_to_date', operation: null, current_version: 'synthetic-version', version: null, notes: null, progress_phase: null, downloaded_bytes: 0, total_bytes: null, error: null, reason: null, auto_download_enabled: false, auto_install_enabled: false }
      if (command === 'get_wake_clip_settings') return { enabled: false, directory: '/tmp/synthetic-clips' }
      if (command === 'list_wake_clips') return []
      if (command === 'list_audio_input_devices') return []
      if (['record_frontend_runtime_diagnostic', 'write_runtime_log', 'check_for_update'].includes(command)) return null
      throw new Error(`Unexpected fixture command: ${command}`)
    })
    window.__TAURI_INTERNALS__ = { invoke, createChannel: <T,>(send: (value: T) => void) => ({ send }) }
    try {
      await act(async () => { root.render(<App />) })
      const speakerButton = container.querySelector<HTMLButtonElement>('button[aria-label="Speak replies"]')!
      expect(speakerButton).not.toBeNull()
      expect(speakerButton.getAttribute('aria-pressed')).toBe('false')
      await act(async () => { speakerButton.click() })
      expect(ttsEnabled).toBe(true)
      expect(speakerButton.getAttribute('aria-pressed')).toBe('true')

      const composer = container.querySelector<HTMLTextAreaElement>('#promptComposer')!
      await act(async () => {
        Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')!.set!.call(composer, 'Synthetic streaming question')
        composer.dispatchEvent(new Event('input', { bubbles: true }))
      })
      const send = container.querySelector<HTMLButtonElement>('button[type="submit"]')!
      await act(async () => { send.click() })
      expect(requestId).not.toBe('')

      await act(async () => {
        streamChannel?.({ kind: 'text', text: 'First paragraph spoken lead.\n\nSecond paragraph detailed context.' })
      })
      expect(invoke.mock.calls.filter(([command]) => command === 'speak_local_tts')).toEqual([])

      transcript = [canonicalTurn('1', '2', 'Synthetic streaming question', 'First paragraph spoken lead.\n\nSecond paragraph detailed context.', requestId)]
      await act(async () => { finish({ request_id: requestId, outcome: 'completed', runtime_phase: 'sleeping' }) })
      expect(invoke.mock.calls.filter(([command]) => command === 'speak_local_tts')).toEqual([])
    } finally {
      await act(async () => { root.unmount() }); container.remove()
    }
  })
})

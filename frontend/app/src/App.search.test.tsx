import { act } from 'react'
import { createRoot } from 'react-dom/client'
import type { Root } from 'react-dom/client'
import { afterEach, describe, expect, it, vi } from 'vitest'
import * as tauri from './lib/tauri'
import App from './App'

let root: Root | null = null
let container: HTMLElement | null = null
afterEach(() => { act(() => root?.unmount()); container?.remove(); delete window.__TAURI_INTERNALS__; vi.restoreAllMocks() })

const capabilities = ['custom_provider', 'opencode', 'local_fast', 'local_quality', 'qwen_prediction', 'wake_word', 'vad', 'parakeet', 'tts', 'deep', 'review']
  .map((id) => ({ id, state: 'available', reason: 'Synthetic capability', actual_provider: null }))

function fixture(deepEnabled = false, writeMode?: (mode: string) => Promise<void>, webEnabled = true, writeSettings?: () => Promise<void>) {
  const settings = { instant: 'custom-sol-high', deep: 'opencode-sol-high', review: 'custom-sol-high', deep_enabled: deepEnabled, review_enabled: false, prefetch: false, completion: false, web_enabled: webEnabled, speech_network_fallback: false, speech_language: 'en-AU', speech_tts_provider: 'synthetic.tts.engine', speech_tts_voice: 'synthetic-installed-voice' }
  const snapshot = () => ({
    session_id: '1', runtime_phase: 'sleeping', transcript: [], active_request: null, capabilities,
    model_options: ['custom-sol-high', 'custom-sol-none', 'custom-luna-none', 'custom-luna-low', 'opencode-sol-high', 'local-fast'].map((id) => ({ id, label: id, available: true, unavailable_reason: null })),
    preferences: { ...settings, response_profile: 'fast', tts_enabled: false, ui_theme: 'dark', ui_text_size: 'medium' },
  })
  const requests: Array<{ requestId: string; searchMode: string; updates: { send: (payload: unknown) => void }; finish: () => void }> = []
  const modeCalls: string[] = []
  window.__TAURI_INTERNALS__ = {
    createChannel: <T,>(send: (message: T) => void) => ({ send }),
    invoke: async (command, args) => {
      if (command === 'get_app_snapshot' || command === 'reset_session') return snapshot()
      if (command === 'get_startup_state') return {
        kind: 'ready', cue_asset_paths: { start_listening: 'synthetic-start.wav', stop_listening: 'synthetic-stop.wav' },
        runtime_phase: 'sleeping', voice_input_available: false, voice_input_error: 'Synthetic no audio', silence_timeout_ms: 1500,
        selected_response_profile: 'fast', supported_response_profiles: [], tts_enabled: false, prompt_cancellation_available: true, capabilities,
      }
      if (command === 'get_assistant_settings') return settings
      if (command === 'set_assistant_settings') { await writeSettings?.(); Object.assign(settings, (args as { settings: unknown }).settings); return settings }
      if (command === 'set_search_mode') {
        const mode = (args as { mode: string }).mode
        modeCalls.push(mode)
        await writeMode?.(mode)
        return null
      }
      if (command === 'submit_prompt') return new Promise((resolve) => {
        const request = args as typeof requests[number]
        requests.push({ ...request, finish: () => resolve({ request_id: request.requestId, runtime_phase: 'sleeping', outcome: 'cancelled' }) })
      })
      return null
    },
  }
  return { requests, modeCalls, settings }
}

async function render() {
  container = document.createElement('div')
  document.body.append(container)
  root = createRoot(container)
  await act(async () => { root?.render(<App />) })
  return container
}

async function enterPrompt(container: HTMLElement) {
  const composer = container.querySelector('textarea')!
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set?.call(composer, 'Synthetic search question')
    composer.dispatchEvent(new Event('input', { bubbles: true }))
  })
}

describe('admitted search mode and generation-owned presentation', () => {
  it('preserves desktop speech selection strings through ordinary model changes without advertising SDK selectors', async () => {
    window.history.replaceState(null, '')
    const { settings } = fixture()
    const container = await render()
    await act(async () => { const instant = container.querySelector<HTMLSelectElement>('#composerInstantSelect')!; instant.value = 'custom-sol-none'; instant.dispatchEvent(new Event('change', { bubbles: true })) })
    expect(settings).toMatchObject({ instant: 'custom-sol-none', speech_language: 'en-AU', speech_tts_provider: 'synthetic.tts.engine', speech_tts_voice: 'synthetic-installed-voice' })
    await act(async () => { container.querySelector<HTMLButtonElement>('[aria-label="Settings"]')!.click() })
    expect(container.querySelector('[aria-label="Speech language"]')).toBeNull()
    expect(container.querySelector('[aria-label="TTS engine"]')).toBeNull()
    expect(container.querySelector('[aria-label="TTS voice"]')).toBeNull()
    expect(container.querySelector('[aria-label="Allow speech network fallback"]')).toBeNull()
    window.history.replaceState(null, '')
  })
  it('persists web-off independently of search effort, preserves speech policy and admits ordinary chat without verification', async () => {
    window.history.replaceState(null, '')
    const { requests, settings } = fixture()
    const container = await render()
    const selector = container.querySelector<HTMLSelectElement>('[aria-label="Search mode"]')!
    await act(async () => { selector.value = 'research'; selector.dispatchEvent(new Event('change', { bubbles: true })) })
    await act(async () => { container.querySelector<HTMLButtonElement>('[aria-label="Settings"]')!.click() })
    const web = container.querySelector<HTMLInputElement>('[aria-label="Allow web access"]')!
    expect(web.checked).toBe(true)
    expect(container.textContent).toContain('Parallel is primary, with Exa as fallback')
    await act(async () => { web.click() })
    expect(settings.web_enabled).toBe(false)
    expect(settings.speech_network_fallback).toBe(false)
    expect(selector.disabled).toBe(true)
    expect(selector.value).toBe('quick')
    expect(container.textContent).toContain('Normal chat remains available')
    expect(container.textContent).toContain('answers are not web-verified')
    await enterPrompt(container)
    await act(async () => { container.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    expect(requests[0]?.searchMode).toBe('quick')
    expect(web.disabled).toBe(true)
    await act(async () => { requests[0]!.finish() })
    // Reattachment uses the backend's persisted permission, not an ephemeral UI default.
    await act(async () => { root?.unmount() }); container.remove()
    const reattached = await render()
    expect(reattached.querySelector<HTMLSelectElement>('[aria-label="Search mode"]')!.disabled).toBe(true)
    window.history.replaceState(null, '')
  })

  it('blocks admission during a web preference write and rolls back a rejected save', async () => {
    window.history.replaceState(null, '')
    let reject: (error: Error) => void = () => undefined
    fixture(false, undefined, true, () => new Promise((_resolve, fail) => { reject = fail }))
    const container = await render()
    await enterPrompt(container)
    await act(async () => { container.querySelector<HTMLButtonElement>('[aria-label="Settings"]')!.click() })
    const web = container.querySelector<HTMLInputElement>('[aria-label="Allow web access"]')!
    await act(async () => { web.click() })
    expect(web.checked).toBe(false)
    expect(web.disabled).toBe(true)
    expect(container.querySelector<HTMLButtonElement>('button[type="submit"]')!.disabled).toBe(true)
    await act(async () => { reject(new Error('Synthetic policy persistence failure')) })
    expect(web.checked).toBe(true)
    expect(web.disabled).toBe(false)
    expect(container.querySelector<HTMLButtonElement>('button[type="submit"]')!.disabled).toBe(false)
    expect(container.textContent).toContain('Assistant settings failed')
    window.history.replaceState(null, '')
  })
  it('aligns the native voice loop with ephemeral Quick when a native UI attaches', async () => {
    vi.spyOn(tauri, 'isNativeTauriRuntime').mockReturnValue(true)
    const { modeCalls } = fixture()
    const container = await render()
    expect(modeCalls).toEqual(['quick'])
    expect(container.querySelector<HTMLSelectElement>('[aria-label="Search mode"]')!.value).toBe('quick')
  })
  it('waits for the native mode acknowledgement before admitting typed or voice work and retains Quick on failure', async () => {
    let fail: ((error: Error) => void) | undefined
    fixture(false, (mode) => mode === 'quick' ? Promise.resolve() : new Promise((_resolve, reject) => { fail = reject }))
    const container = await render()
    await enterPrompt(container)
    const selector = container.querySelector<HTMLSelectElement>('[aria-label="Search mode"]')!
    await act(async () => { selector.value = 'research'; selector.dispatchEvent(new Event('change', { bubbles: true })) })
    expect(selector.disabled).toBe(true)
    expect(container.querySelector<HTMLButtonElement>('button[type="submit"]')!.disabled).toBe(true)
    await act(async () => { fail?.(new Error('Synthetic native rejection')) })
    expect(selector.value).toBe('quick')
    expect(selector.disabled).toBe(false)
    expect(container.textContent).toContain('Search mode not changed')
  })

  it('preserves the ephemeral selection across model settings changes but admits Quick on unsupported routes', async () => {
    const { requests } = fixture()
    const container = await render()
    const selector = container.querySelector<HTMLSelectElement>('[aria-label="Search mode"]')!
    await act(async () => { selector.value = 'verify'; selector.dispatchEvent(new Event('change', { bubbles: true })) })
    const instant = container.querySelector<HTMLSelectElement>('#composerInstantSelect')!
    await act(async () => { instant.value = 'opencode-sol-high'; instant.dispatchEvent(new Event('change', { bubbles: true })) })
    expect(selector.value).toBe('quick')
    expect(selector.disabled).toBe(true)
    await enterPrompt(container)
    await act(async () => { container.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    expect(requests[0]?.searchMode).toBe('quick')
    await act(async () => { requests[0]!.finish() })
    await act(async () => { instant.value = 'custom-sol-high'; instant.dispatchEvent(new Event('change', { bubbles: true })) })
    expect(selector.value).toBe('verify')
    expect(selector.disabled).toBe(false)
  })

  it('captures Verify, disables selection while busy, and resets New to Quick without accepting late old search activity', async () => {
    const { requests, modeCalls } = fixture()
    const container = await render()
    const selector = container.querySelector<HTMLSelectElement>('[aria-label="Search mode"]')!
    expect(selector.value).toBe('quick')
    expect(selector.disabled).toBe(false)
    await act(async () => { selector.value = 'verify'; selector.dispatchEvent(new Event('change', { bubbles: true })) })
    expect(modeCalls).toEqual(['verify'])
    expect(selector.value).toBe('verify')
    await enterPrompt(container)
    await act(async () => { container.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    expect(requests[0]?.searchMode).toBe('verify')
    expect(selector.disabled).toBe(true)
    for (const message of ['Searching', 'Reading', 'Fallback', 'Partial']) {
      await act(async () => { requests[0]!.updates.send({ kind: 'status', message }) })
      expect(container.querySelector('.composer__status')?.textContent).toBe(message)
    }
    await act(async () => { container.querySelector<HTMLButtonElement>('[aria-label="New Chat"]')!.click() })
    expect(selector.value).toBe('quick')
    expect(modeCalls).toEqual(['verify', 'quick'])
    await enterPrompt(container)
    await act(async () => { container.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    expect(requests[1]?.searchMode).toBe('quick')
    await act(async () => {
      requests[1]!.updates.send({ kind: 'status', message: 'Reading' })
      requests[0]!.updates.send({ kind: 'status', message: 'Synthetic stale fallback' })
      requests[0]!.finish()
    })
    expect(container.querySelector('.composer__status')?.textContent).toBe('Reading')
    await act(async () => { requests[1]!.finish() })
  })

  it('disables native mode selection and explains exclusive OpenCode Deep ownership', async () => {
    const { requests, modeCalls } = fixture(true)
    const container = await render()
    const selector = container.querySelector<HTMLSelectElement>('[aria-label="Search mode"]')!
    expect(selector.disabled).toBe(true)
    expect(container.textContent).toContain('OpenCode Deep owns research for this turn.')
    await act(async () => { selector.value = 'research'; selector.dispatchEvent(new Event('change', { bubbles: true })) })
    expect(modeCalls).toEqual([])
    await enterPrompt(container)
    await act(async () => { container.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    expect(requests[0]?.searchMode).toBe('quick')
    await act(async () => { requests[0]!.finish() })
  })
})

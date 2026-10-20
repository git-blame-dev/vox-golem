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

function fixture(deepEnabled = false, webEnabled = true, writeSettings?: () => Promise<void>) {
  const settings = { instant: 'custom-sol-high', deep: 'opencode-sol-high', review: 'custom-sol-high', deep_enabled: deepEnabled, review_enabled: false, prefetch: false, completion: false, web_enabled: webEnabled, speech_network_fallback: false, speech_language: 'en-AU', speech_tts_provider: 'synthetic.tts.engine', speech_tts_voice: 'synthetic-installed-voice' }
  const snapshot = () => ({
    session_id: '1', runtime_phase: 'sleeping', transcript: [], active_request: null, capabilities,
    model_options: ['custom-sol-high', 'custom-sol-none', 'custom-luna-none', 'custom-luna-low', 'opencode-sol-high', 'local-fast'].map((id) => ({ id, label: id, available: true, unavailable_reason: null })),
    preferences: { ...settings, response_profile: 'fast', tts_enabled: false, ui_theme: 'dark', ui_text_size: 'medium' },
  })
  const requests: Array<{ requestId: string; prompt: string; updates: { send: (payload: unknown) => void }; finish: () => void }> = []
  const commands: string[] = []
  window.__TAURI_INTERNALS__ = {
    createChannel: <T,>(send: (message: T) => void) => ({ send }),
    invoke: async (command, args) => {
      commands.push(command)
      if (command === 'get_app_snapshot' || command === 'reset_session') return snapshot()
      if (command === 'get_startup_state') return {
        kind: 'ready', cue_asset_paths: { start_listening: 'synthetic-start.wav', stop_listening: 'synthetic-stop.wav' },
        runtime_phase: 'sleeping', voice_input_available: false, voice_input_error: 'Synthetic no audio', silence_timeout_ms: 1500,
        selected_response_profile: 'fast', supported_response_profiles: [], tts_enabled: false, prompt_cancellation_available: true, capabilities,
      }
      if (command === 'get_assistant_settings') return settings
      if (command === 'set_assistant_settings') { await writeSettings?.(); Object.assign(settings, (args as { settings: unknown }).settings); return settings }
      if (command === 'submit_prompt') return new Promise((resolve) => {
        const request = args as typeof requests[number]
        requests.push({ ...request, finish: () => resolve({ request_id: request.requestId, runtime_phase: 'sleeping', outcome: 'cancelled' }) })
      })
      return null
    },
  }
  return { requests, commands, settings }
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

describe('Web research permission and generation-owned presentation', () => {
  it.each(['', ' \n'])('publishes first nonblank text without waiting for a frame (prefix=%j)', async (prefix) => {
    const frames: FrameRequestCallback[] = []
    vi.spyOn(window, 'requestAnimationFrame').mockImplementation((callback) => { frames.push(callback); return frames.length })
    const cancelFrame = vi.spyOn(window, 'cancelAnimationFrame').mockImplementation(() => undefined)
    const { requests } = fixture()
    const container = await render()
    await enterPrompt(container)
    await act(async () => { container.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    if (prefix !== '') await act(async () => { requests[0]!.updates.send({ kind: 'text', text: prefix }) })
    await act(async () => { requests[0]!.updates.send({ kind: 'text', text: 'Synthetic first text' }) })
    expect(container.querySelector('.answer-stage__answer')?.textContent?.trim()).toBe('Synthetic first text')
    if (prefix !== '') {
      expect(cancelFrame).toHaveBeenCalledWith(1)
      await act(async () => { frames[0]!(16) })
      expect(container.querySelector('.answer-stage__answer')?.textContent?.trim()).toBe('Synthetic first text')
    }
    const priorFrameCount = frames.length
    await act(async () => { requests[0]!.updates.send({ kind: 'text', text: ' plus later text' }) })
    expect(frames).toHaveLength(priorFrameCount + 1)
    expect(container.querySelector('.answer-stage__answer')?.textContent?.trim()).toBe('Synthetic first text')
    await act(async () => { frames.at(-1)!(32) })
    expect(container.querySelector('.answer-stage__answer')?.textContent?.trim()).toBe('Synthetic first text plus later text')
    await act(async () => { requests[0]!.finish() })
  })

  it('retains cumulative sources and first titles when a later read returns only a subset', async () => {
    const { requests } = fixture()
    const container = await render()
    await enterPrompt(container)
    await act(async () => { container.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    await act(async () => {
      requests[0]!.updates.send({ kind: 'sources', sources: [
        { url: 'https://example.test/source-a', title: 'Synthetic first title' },
        { url: 'https://example.test/source-b', title: 'Synthetic second source' },
      ] })
      requests[0]!.updates.send({ kind: 'sources', sources: [
        { url: 'https://example.test/source-a', title: 'Synthetic later title' },
      ] })
    })
    const links = Array.from(container.querySelectorAll<HTMLAnchorElement>('.answer-stage__sources a'))
    expect(links.map((link) => link.getAttribute('href'))).toEqual(['https://example.test/source-a', 'https://example.test/source-b'])
    expect(links.map((link) => link.textContent)).toEqual(['Synthetic first title', 'Synthetic second source'])
    await act(async () => { requests[0]!.finish() })
  })

  it('bounds cumulative sources to the native limit and ignores late sources and frames from a retired request', async () => {
    const frames: FrameRequestCallback[] = []
    vi.spyOn(window, 'requestAnimationFrame').mockImplementation((callback) => { frames.push(callback); return frames.length })
    vi.spyOn(window, 'cancelAnimationFrame').mockImplementation(() => undefined)
    const { requests } = fixture()
    const container = await render()
    await enterPrompt(container)
    await act(async () => { container.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    const sources = Array.from({ length: 40 }, (_, index) => ({ url: `https://example.test/source-${index}`, title: `Synthetic source ${index}` }))
    await act(async () => {
      requests[0]!.updates.send({ kind: 'sources', sources: sources.slice(0, 24) })
      requests[0]!.updates.send({ kind: 'sources', sources: sources.slice(16) })
      requests[0]!.updates.send({ kind: 'text', text: 'Synthetic first text' })
      requests[0]!.updates.send({ kind: 'text', text: ' retired frame' })
    })
    expect(container.querySelectorAll('.answer-stage__sources a')).toHaveLength(32)
    expect(Array.from(container.querySelectorAll<HTMLAnchorElement>('.answer-stage__sources a')).map((link) => link.getAttribute('href'))).toEqual(sources.slice(0, 32).map((source) => source.url))
    await act(async () => { container.querySelector<HTMLButtonElement>('[aria-label="New Chat"]')!.click() })
    await enterPrompt(container)
    await act(async () => { container.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    await act(async () => {
      requests[1]!.updates.send({ kind: 'text', text: 'Synthetic new answer' })
      requests[1]!.updates.send({ kind: 'sources', sources: [{ url: 'https://example.test/new-source', title: 'Synthetic new source' }] })
      requests[0]!.updates.send({ kind: 'text', text: 'Synthetic stale text' })
      requests[0]!.updates.send({ kind: 'sources', sources: sources.slice(0, 1) })
      for (const callback of frames) callback(48)
    })
    expect(container.textContent).not.toContain('retired frame')
    expect(container.textContent).not.toContain('Synthetic stale text')
    expect(Array.from(container.querySelectorAll<HTMLAnchorElement>('.answer-stage__sources a')).map((link) => link.getAttribute('href'))).toEqual(['https://example.test/new-source'])
    await act(async () => { requests[0]!.finish(); requests[1]!.finish() })
  })

  it('preserves desktop speech selection strings through ordinary model changes without advertising SDK selectors', async () => {
    window.history.replaceState(null, '')
    const { settings } = fixture()
    const container = await render()
    await act(async () => { const instant = container.querySelector<HTMLSelectElement>('#composerInstantSelect')!; instant.value = 'custom-sol-high'; instant.dispatchEvent(new Event('change', { bubbles: true })) })
    expect(settings).toMatchObject({ instant: 'custom-sol-high', speech_language: 'en-AU', speech_tts_provider: 'synthetic.tts.engine', speech_tts_voice: 'synthetic-installed-voice' })
    await act(async () => { container.querySelector<HTMLButtonElement>('[aria-label="Settings"]')!.click() })
    expect(container.querySelector('[aria-label="Speech language"]')).toBeNull()
    expect(container.querySelector('[aria-label="TTS engine"]')).toBeNull()
    expect(container.querySelector('[aria-label="TTS voice"]')).toBeNull()
    expect(container.querySelector('[aria-label="Allow speech network fallback"]')).toBeNull()
    window.history.replaceState(null, '')
  })
  it('persists web-off, preserves speech policy and admits ordinary chat without verification', async () => {
    window.history.replaceState(null, '')
    const { requests, settings } = fixture()
    const container = await render()
    expect(container.querySelector('[aria-label="Search mode"]')).toBeNull()
    await act(async () => { container.querySelector<HTMLButtonElement>('[aria-label="Settings"]')!.click() })
    const web = container.querySelector<HTMLInputElement>('[aria-label="Allow web access"]')!
    expect(web.checked).toBe(true)
    expect(web.closest('label')?.textContent).toContain('Web research')
    expect(container.querySelectorAll('[aria-label="Allow web access"]')).toHaveLength(1)
    expect(container.textContent).toContain('Exa is primary, with Parallel as fallback')
    await act(async () => { web.click() })
    expect(settings.web_enabled).toBe(false)
    expect(settings.speech_network_fallback).toBe(false)
    expect(container.textContent).toContain('answers are not web-verified')
    await enterPrompt(container)
    await act(async () => { container.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    expect(requests[0]).not.toHaveProperty('searchMode')
    expect(requests[0]?.prompt).toBe('Synthetic search question')
    expect(web.disabled).toBe(true)
    await act(async () => { requests[0]!.finish() })
    // Reattachment uses the backend's persisted permission, not an ephemeral UI default.
    await act(async () => { root?.unmount() }); container.remove()
    const reattached = await render()
    expect(reattached.querySelector('[aria-label="Search mode"]')).toBeNull()
    await act(async () => { reattached.querySelector<HTMLButtonElement>('[aria-label="Settings"]')!.click() })
    expect(reattached.querySelector<HTMLInputElement>('[aria-label="Allow web access"]')!.checked).toBe(false)
    window.history.replaceState(null, '')
  })

  it('blocks admission during a web preference write and rolls back a rejected save', async () => {
    window.history.replaceState(null, '')
    let reject: (error: Error) => void = () => undefined
    fixture(false, true, () => new Promise((_resolve, fail) => { reject = fail }))
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

  it('admits native startup without a mode command or manual mode hints', async () => {
    vi.spyOn(tauri, 'isNativeTauriRuntime').mockReturnValue(true)
    const { commands } = fixture()
    const container = await render()
    expect(commands).not.toContain('set_search_mode')
    expect(container.querySelector('[aria-label="Search mode"]')).toBeNull()
    expect(container.textContent).not.toMatch(/Quick|Verify|automatic Quick/)
    await act(async () => { container.querySelector<HTMLButtonElement>('[aria-label="Settings"]')!.click() })
    expect(container.textContent).not.toMatch(/Quick|Verify|Search effort/)
  })

  it('admits chat across model changes without adding a mode or substituting the selected model', async () => {
    const { requests, settings, commands } = fixture()
    const container = await render()
    expect(container.querySelector('[aria-label="Search mode"]')).toBeNull()
    const instant = container.querySelector<HTMLSelectElement>('#composerInstantSelect')!
    await act(async () => { instant.value = 'opencode-sol-high'; instant.dispatchEvent(new Event('change', { bubbles: true })) })
    await enterPrompt(container)
    await act(async () => { container.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    expect(requests[0]).not.toHaveProperty('searchMode')
    expect(settings.instant).toBe('opencode-sol-high')
    await act(async () => { requests[0]!.finish() })
    await act(async () => { instant.value = 'custom-sol-high'; instant.dispatchEvent(new Event('change', { bubbles: true })) })
    await enterPrompt(container)
    await act(async () => { container.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    expect(requests[1]).not.toHaveProperty('searchMode')
    expect(settings.instant).toBe('custom-sol-high')
    expect(commands).not.toContain('set_search_mode')
    await act(async () => { requests[1]!.finish() })
    await act(async () => { instant.value = 'local-fast'; instant.dispatchEvent(new Event('change', { bubbles: true })) })
    await enterPrompt(container)
    await act(async () => { container.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    expect(requests[2]?.prompt).toBe('Synthetic search question')
    expect(requests[2]).not.toHaveProperty('searchMode')
    expect(settings.instant).toBe('local-fast')
    expect(settings.web_enabled).toBe(true)
    await act(async () => { requests[2]!.finish() })
  })

  it('presents search status and citations, starts New Chat without a mode write, and ignores stale search activity', async () => {
    const { requests, commands } = fixture()
    const container = await render()
    expect(container.querySelector('[aria-label="Search mode"]')).toBeNull()
    await enterPrompt(container)
    await act(async () => { container.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    expect(requests[0]).not.toHaveProperty('searchMode')
    for (const message of ['Searching', 'Reading', 'Fallback', 'Partial']) {
      await act(async () => { requests[0]!.updates.send({ kind: 'status', message }) })
      expect(container.querySelector('.composer [role="status"].sr-only')?.textContent).toBe(message)
      expect(container.querySelector('.composer__status')).toBeNull()
      expect(container.querySelector('.answer-stage__statuses')).toBeNull()
      expect(container.querySelector('.message__thinking')?.textContent).toBe(message)
    }
    await act(async () => {
      requests[0]!.updates.send({ kind: 'text', text: 'Synthetic grounded answer' })
      requests[0]!.updates.send({ kind: 'sources', sources: [{ url: 'https://example.test/source', title: 'Synthetic citation' }] })
    })
    const sourceLink = container.querySelector<HTMLAnchorElement>('.answer-stage__sources a')
    expect(sourceLink?.getAttribute('href')).toBe('https://example.test/source')
    expect(sourceLink?.textContent).toBe('Synthetic citation')
    await act(async () => { container.querySelector<HTMLButtonElement>('[aria-label="New Chat"]')!.click() })
    expect(commands).not.toContain('set_search_mode')
    await enterPrompt(container)
    await act(async () => { container.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    expect(requests[1]).not.toHaveProperty('searchMode')
    await act(async () => {
      requests[1]!.updates.send({ kind: 'status', message: 'Reading' })
      requests[0]!.updates.send({ kind: 'status', message: 'Synthetic stale fallback' })
      requests[0]!.finish()
    })
    expect(container.querySelector('.composer [role="status"].sr-only')?.textContent).toBe('Reading')
    expect(container.querySelector('.composer__status')).toBeNull()
    await act(async () => { requests[1]!.finish() })
  })

  it('admits the selected stages when OpenCode Deep is enabled without a mode selector', async () => {
    const { requests, commands } = fixture(true)
    const container = await render()
    expect(container.querySelector('[aria-label="Search mode"]')).toBeNull()
    expect(commands).not.toContain('set_search_mode')
    await enterPrompt(container)
    await act(async () => { container.querySelector<HTMLButtonElement>('button[type="submit"]')!.click() })
    expect(requests[0]).not.toHaveProperty('searchMode')
    await act(async () => { requests[0]!.finish() })
  })
})

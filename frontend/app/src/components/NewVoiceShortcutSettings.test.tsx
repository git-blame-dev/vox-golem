import { act } from 'react'
import { createRoot } from 'react-dom/client'
import { expect, it, vi } from 'vitest'
import { NewVoiceShortcutSettings } from './NewVoiceShortcutSettings'

it('requests the one-icon native launcher action without claiming confirmation early or starting a mic', async () => {
  vi.useFakeTimers()
  let confirmed = false
  const invoke = vi.fn(async (command: string) => ({ supported: true, pinned: confirmed, requested: command === 'pin_new_voice_shortcut' && !confirmed }))
  window.__TAURI_INTERNALS__ = { invoke }
  const container = document.createElement('div'); document.body.append(container); const root = createRoot(container)
  try {
    await act(async () => { root.render(<NewVoiceShortcutSettings />) })
    expect(container.querySelector('section')?.getAttribute('aria-label')).toBe('Ask shortcut')
    expect(container.querySelector('strong')?.textContent).toBe('Ask')
    expect(container.textContent).toContain('current conversation')
    expect(container.textContent).toContain('finalized utterance automatically sends once')
    expect(container.textContent).toContain('Stop cancels without sending')
    expect(container.textContent).not.toMatch(/blank voice draft|Stop & send|new conversation/)
    expect(container.querySelector('button')?.textContent).toBe('Add Ask to Home screen')
    await act(async () => { container.querySelector<HTMLButtonElement>('button')!.click() })
    expect(container.textContent).toContain('Pin requested')
    expect(container.textContent).not.toContain('shortcut is pinned')
    expect(invoke).toHaveBeenCalledWith('pin_new_voice_shortcut', undefined)
    expect(invoke.mock.calls.some(([command]) => command === 'start_assistant_voice' || command === 'reset_session')).toBe(false)
    confirmed = true
    await act(async () => { await vi.advanceTimersByTimeAsync(1000) })
    expect(container.textContent).toContain('shortcut is pinned')
  } finally { act(() => root.unmount()); container.remove(); delete window.__TAURI_INTERNALS__; vi.useRealTimers() }
})

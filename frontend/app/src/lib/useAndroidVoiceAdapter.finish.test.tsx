import { act } from 'react'
import { createRoot } from 'react-dom/client'
import { expect, it, vi } from 'vitest'
import { useAndroidVoiceAdapter, type AndroidVoiceAdapterView } from './useAndroidVoiceAdapter'

it('scopes an owned finish to its admitted ticket but preserves explicit unowned finish', async () => {
  const wire = { kind: 'voice_status', available: true, reason: 'Synthetic recognizer', mode: 'dictation', dictation: 'idle', loop: 'idle', ttsEnabled: false, ttsAvailable: false, ttsPhase: 'idle', error: null }
  const native = vi.fn(async (name: string): Promise<unknown> => name === 'assistant_voice_status' || name === 'start_assistant_voice' ? wire : null)
  window.__TAURI_INTERNALS__ = { invoke: native, createChannel: () => ({}) }
  let voice!: AndroidVoiceAdapterView
  const noop = () => undefined
  const stopRef = { current: null }
  function Harness() {
    voice = useAndroidVoiceAdapter({ draft: '', disabled: false, generationAvailable: true, stopRef, onDraft: noop, onSubmit: noop, onActivity: noop, onSettled: noop })
    return null
  }
  const container = document.createElement('div')
  const root = createRoot(container)
  try {
    await act(async () => { root.render(<Harness />) })
    await act(async () => { voice.finishDictation() })
    expect(native).toHaveBeenCalledWith('finish_assistant_dictation', undefined)
    await act(async () => { await voice.start(false) })
    const start = native.mock.calls.find(([name]) => name === 'start_assistant_voice')
    expect(start).toBeDefined()
    const ticket = (start as unknown as [string, { sessionId: string }])[1].sessionId
    expect(ticket).not.toBe('')
    native.mockClear()
    await act(async () => { voice.finishDictation() })
    expect(native).toHaveBeenCalledWith('finish_assistant_dictation', { sessionId: ticket })
    native.mockClear()
    await act(async () => { root.unmount() })
    expect(native.mock.calls.some(([name]) => name === 'finish_assistant_dictation')).toBe(false)
  } finally {
    delete window.__TAURI_INTERNALS__
  }
})

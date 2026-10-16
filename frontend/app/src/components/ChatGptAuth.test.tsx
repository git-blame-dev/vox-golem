import { act } from 'react'
import { createRoot } from 'react-dom/client'
import { afterEach, expect, it, vi } from 'vitest'
import { ChatGptAuth } from './ChatGptAuth'
import { invokeAndroidCommand } from '../lib/tauri'

let root: ReturnType<typeof createRoot> | undefined
let container: HTMLDivElement
afterEach(() => {
  if (root) act(() => root?.unmount())
  container?.remove()
  Reflect.deleteProperty(window, '__TAURI_INTERNALS__')
  vi.useRealTimers()
})
async function render(invoke: (command: string, args?: unknown) => Promise<unknown>, changed = vi.fn()) {
  window.__TAURI_INTERNALS__ = { invoke }
  container = document.createElement('div')
  document.body.append(container)
  root = createRoot(container)
  await act(async () => { root?.render(<ChatGptAuth onChanged={changed} />) })
  return changed
}
async function click(label: string) {
  const button = [...container.querySelectorAll('button')].find((item) => item.textContent === label)
  expect(button).toBeDefined()
  await act(async () => { button?.click() })
}
it('resumes pending auth, polls without overlap, and refreshes on connection', async () => {
  vi.useFakeTimers()
  let resolve: (value: unknown) => void = () => undefined
  const invoke = vi.fn().mockResolvedValueOnce({ phase: 'pending', userCode: 'TEST-CODE', verificationUrl: null })
    .mockImplementationOnce(() => new Promise((done) => { resolve = done }))
  const changed = await render(invoke)
  expect(container.querySelector('input')?.value).toBe('TEST-CODE')
  await act(async () => { await vi.advanceTimersByTimeAsync(2000) })
  await act(async () => { await vi.advanceTimersByTimeAsync(6000) })
  expect(invoke).toHaveBeenCalledTimes(2)
  await act(async () => { resolve({ phase: 'connected' }) })
  expect(changed).toHaveBeenCalledTimes(1)
  await act(async () => { await vi.advanceTimersByTimeAsync(6000) })
  expect(invoke).toHaveBeenCalledTimes(2)
})
it('fences late start replies after cancellation', async () => {
  let finish: (value: unknown) => void = () => undefined
  const invoke = vi.fn(async (command: string, args?: unknown) => {
    if (command === 'open_chatgpt_sign_in') return null
    const action = (args as { action: string }).action
    if (action === 'start') return new Promise((done) => { finish = done })
    return { phase: 'signed_out' }
  })
  await render(invoke)
  await click('Continue with ChatGPT')
  await click('Cancel')
  await act(async () => { finish({ phase: 'pending', userCode: 'LATE-CODE' }) })
  expect(container.querySelector('input')).toBeNull()
})
it('fails closed on malformed native data and unavailable status without leaking errors', async () => {
  await render(async () => ({ phase: 'pending', userCode: 'x'.repeat(1000), verificationUrl: 'https://invalid.example' }))
  expect(container.textContent).toContain('ChatGPT sign-in unavailable')
  expect(container.textContent).not.toContain('https://invalid.example')
  await click('Retry')
  expect(container.textContent).not.toContain('Connected')
})
it('opens the native fixed page without arguments and copies only the public code', async () => {
  const invoke = vi.fn(async (command: string) => command === 'chatgpt_auth'
    ? { phase: 'pending', userCode: 'TEST-CODE', verificationUrl: 'https://auth.openai.com/codex/device', error: null }
    : null)
  const writeText = vi.fn().mockResolvedValue(undefined)
  Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } })
  await render(invoke)
  await click('Open sign-in page')
  expect(invoke).toHaveBeenCalledWith('open_chatgpt_sign_in', undefined)
  await click('Copy code')
  expect(writeText).toHaveBeenCalledWith('TEST-CODE')
})
it('stops polling on unmount without cancelling native auth, and resumes on reopen', async () => {
  vi.useFakeTimers()
  const invoke = vi.fn(async () => ({ phase: 'pending', userCode: 'TEST-CODE' }))
  await render(invoke)
  act(() => root?.unmount())
  root = undefined
  container.remove()
  await act(async () => { await vi.advanceTimersByTimeAsync(6000) })
  expect(invoke).toHaveBeenCalledTimes(1)
  await render(invoke)
  expect(invoke).toHaveBeenCalledTimes(2)
  expect(invoke.mock.calls.every((call) => (call as unknown[])[1] && ((call as unknown[])[1] as { action: string }).action === 'status')).toBe(true)
})
it('does not report sign-out or connection success when the native command fails', async () => {
  const changed = vi.fn()
  const invoke = vi.fn().mockResolvedValueOnce({ phase: 'connected', userCode: null, verificationUrl: null, error: null })
    .mockRejectedValueOnce(new Error('synthetic private failure'))
  await render(invoke, changed)
  expect(changed).toHaveBeenCalledTimes(1)
  await click('Sign out')
  expect(changed).toHaveBeenCalledTimes(1)
  expect(container.textContent).toContain('ChatGPT sign-in unavailable')
  expect(container.textContent).not.toContain('synthetic private failure')
})
it('handles unavailable preview commands without polling or raw error output', async () => {
  vi.useFakeTimers()
  const invoke = vi.fn().mockRejectedValue(new Error('synthetic unsupported command'))
  await render(invoke)
  await act(async () => { await vi.advanceTimersByTimeAsync(6000) })
  expect(invoke).toHaveBeenCalledTimes(1)
  expect(container.textContent).toContain('Retry')
  expect(container.textContent).not.toContain('synthetic unsupported')
})
it('forwards both shared auth commands through the Android allowlist', async () => {
  const invoke = vi.fn().mockResolvedValue({ phase: 'signed_out' })
  window.__TAURI_INTERNALS__ = { invoke }
  await invokeAndroidCommand('chatgpt_auth', { action: 'status' })
  await invokeAndroidCommand('open_chatgpt_sign_in')
  expect(invoke.mock.calls).toEqual([
    ['chatgpt_auth', { action: 'status' }],
    ['open_chatgpt_sign_in', undefined],
  ])
})
it('ignores an old status completion after cancel and a new attempt', async () => {
  vi.useFakeTimers()
  let finish: (value: unknown) => void = () => undefined
  const invoke = vi.fn().mockResolvedValueOnce({ phase: 'pending', userCode: 'OLD-CODE' })
    .mockImplementationOnce(() => new Promise((done) => { finish = done }))
    .mockResolvedValueOnce({ phase: 'signed_out' })
    .mockResolvedValueOnce({ phase: 'pending', userCode: 'NEW-CODE' })
  const changed = await render(invoke)
  await act(async () => { await vi.advanceTimersByTimeAsync(2000) })
  await click('Cancel')
  await click('Continue with ChatGPT')
  await act(async () => { finish({ phase: 'connected' }) })
  expect(container.querySelector('input')?.value).toBe('NEW-CODE')
  expect(changed).not.toHaveBeenCalled()
})
it('ignores in-flight replies after unmount', async () => {
  let finish: (value: unknown) => void = () => undefined
  const invoke = vi.fn(() => new Promise((done) => { finish = done }))
  const changed = await render(invoke)
  act(() => root?.unmount())
  root = undefined
  await act(async () => { finish({ phase: 'connected' }) })
  expect(changed).not.toHaveBeenCalled()
})

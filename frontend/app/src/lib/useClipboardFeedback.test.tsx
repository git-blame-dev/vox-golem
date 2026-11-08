import { act } from 'react'
import { createRoot } from 'react-dom/client'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { useClipboardFeedback } from './useClipboardFeedback'

const container = document.createElement('div')
let root: ReturnType<typeof createRoot>
function Harness() {
  const { copied, copy } = useClipboardFeedback()
  return <button onClick={() => { void copy('  Synthetic text\n') }}>{copied ? 'Copied' : 'Copy'}</button>
}
beforeEach(() => {
  vi.useFakeTimers()
  document.body.append(container)
  root = createRoot(container)
})
afterEach(() => {
  act(() => root.unmount())
  container.remove()
  vi.useRealTimers()
  vi.restoreAllMocks()
})

it('preserves clipboard text and extends feedback after another successful copy', async () => {
  const writeText = vi.fn().mockResolvedValue(undefined)
  Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } })
  act(() => root.render(<Harness />))
  const button = container.querySelector('button')!
  await act(async () => button.click())
  expect(writeText).toHaveBeenCalledWith('  Synthetic text\n')
  expect(button.textContent).toBe('Copied')
  await act(async () => vi.advanceTimersByTimeAsync(1000))
  await act(async () => button.click())
  await act(async () => vi.advanceTimersByTimeAsync(1000))
  expect(button.textContent).toBe('Copied')
  await act(async () => vi.advanceTimersByTimeAsync(500))
  expect(button.textContent).toBe('Copy')
})

it('does not report success when both clipboard routes fail and removes the fallback field', async () => {
  Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: vi.fn().mockRejectedValue(new Error('Synthetic clipboard rejection')) } })
  Object.defineProperty(document, 'execCommand', { configurable: true, value: vi.fn(() => { throw new Error('Synthetic fallback rejection') }) })
  act(() => root.render(<Harness />))
  await act(async () => container.querySelector('button')!.click())
  expect(container.textContent).toBe('Copy')
  expect(document.querySelector('textarea')).toBeNull()
})

it('retires pending feedback when unmounted before clipboard completion', async () => {
  let complete!: () => void
  Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: vi.fn(() => new Promise<void>((resolve) => { complete = resolve })) } })
  act(() => root.render(<Harness />))
  act(() => container.querySelector('button')!.click())
  act(() => root.render(null))
  const timersBefore = vi.getTimerCount()
  await act(async () => complete())
  expect(vi.getTimerCount()).toBe(timersBefore)
})

it.each(['absent', 'rejecting'])('uses the copy fallback when the Clipboard API is %s', async (kind) => {
  Object.defineProperty(navigator, 'clipboard', { configurable: true, value: kind === 'absent' ? undefined : {
    writeText: vi.fn().mockRejectedValue(new Error('Synthetic clipboard rejection')),
  } })
  const fallback = vi.fn(() => {
    expect(document.querySelector('textarea')?.value).toBe('  Synthetic text\n')
    return true
  })
  Object.defineProperty(document, 'execCommand', { configurable: true, value: fallback })
  act(() => root.render(<Harness />))
  await act(async () => container.querySelector('button')!.click())
  expect(fallback).toHaveBeenCalledWith('copy')
  expect(container.textContent).toBe('Copied')
  expect(document.querySelector('textarea')).toBeNull()
})

it('clears active success feedback on unmount', async () => {
  Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: vi.fn().mockResolvedValue(undefined) } })
  act(() => root.render(<Harness />))
  const timersBefore = vi.getTimerCount()
  await act(async () => container.querySelector('button')!.click())
  expect(vi.getTimerCount()).toBe(timersBefore + 1)
  act(() => root.render(null))
  expect(vi.getTimerCount()).toBe(timersBefore)
})

import { act } from 'react'
import { createRoot } from 'react-dom/client'
import type { Root } from 'react-dom/client'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { PromptComposer } from './PromptComposer'

const roots: Root[] = []
const containers: HTMLDivElement[] = []

afterEach(() => {
  for (const root of roots) act(() => root.unmount())
  for (const container of containers) container.remove()
  roots.length = 0
  containers.length = 0
  vi.restoreAllMocks()
})

function render(props: Parameters<typeof PromptComposer>[0]) {
  const container = document.createElement('div')
  document.body.append(container)
  const root = createRoot(container)
  roots.push(root)
  containers.push(container)
  act(() => root.render(<PromptComposer onChange={() => undefined} {...props} />))
  return container
}

function textarea(container: HTMLElement) {
  return container.querySelector('textarea') as HTMLTextAreaElement
}

function setMetrics(input: HTMLTextAreaElement, scrollHeight: number) {
  vi.spyOn(window, 'getComputedStyle').mockReturnValue({
    lineHeight: '20px',
    paddingTop: '4px',
    paddingBottom: '4px',
    borderTopWidth: '1px',
    borderBottomWidth: '1px',
  } as CSSStyleDeclaration)
  Object.defineProperty(input, 'scrollHeight', { configurable: true, value: scrollHeight })
}

describe('PromptComposer', () => {
  it('starts at two lines, grows to four, caps, and shrinks without moving selection', () => {
    vi.spyOn(window, 'getComputedStyle').mockReturnValue({
      lineHeight: '20px',
      paddingTop: '4px',
      paddingBottom: '4px',
      borderTopWidth: '1px',
      borderBottomWidth: '1px',
    } as CSSStyleDeclaration)
    const container = render({ value: 'draft', maxRows: 4 })
    const input = textarea(container)
    setMetrics(input, 42)
    act(() => roots[0]!.render(<PromptComposer onChange={() => undefined} value="draft" maxRows={4} />))
    expect(input.style.height).toBe('50px')

    Object.defineProperty(input, 'scrollHeight', { configurable: true, value: 62 })
    act(() => roots[0]!.render(<PromptComposer onChange={() => undefined} value="draft changed" maxRows={4} />))
    expect(input.style.height).toBe('64px')
    expect(input.style.overflowY).toBe('hidden')

    Object.defineProperty(input, 'scrollHeight', { configurable: true, value: 88 })
    act(() => window.dispatchEvent(new Event('resize')))
    expect(input.style.height).toBe('90px')
    expect(input.style.overflowY).toBe('hidden')

    Object.defineProperty(input, 'scrollHeight', { configurable: true, value: 142 })
    input.setSelectionRange(2, 4)
    act(() => window.dispatchEvent(new Event('resize')))
    expect(input.style.height).toBe('90px')
    expect(input.style.overflowY).toBe('auto')
    expect(input.selectionStart).toBe(2)
    expect(input.selectionEnd).toBe(4)

    Object.defineProperty(input, 'scrollHeight', { configurable: true, value: 22 })
    act(() => roots[0]!.render(<PromptComposer onChange={() => undefined} value="draft" maxRows={4} />))
    expect(input.style.height).toBe('50px')
    expect(input.style.overflowY).toBe('hidden')
  })

  it('remeasures when typography changes without losing the draft or selection', () => {
    const container = render({ value: 'draft text', maxRows: 4, typographyRevision: 'medium' })
    const input = textarea(container)
    setMetrics(input, 142)
    input.setSelectionRange(2, 6)
    act(() => roots[0]!.render(<PromptComposer onChange={() => undefined} value="draft text" maxRows={4} typographyRevision="large" />))
    expect(input.value).toBe('draft text')
    expect(input.selectionStart).toBe(2)
    expect(input.selectionEnd).toBe(6)
    expect(input.style.height).toBe('90px')
  })

  it('does nothing when browser metrics are unavailable', () => {
    const container = render({ value: 'draft', maxRows: 4 })
    expect(textarea(container).style.height).toBe('')
  })

  it('keeps the ghost suffix out of the controlled textarea value', () => {
    const container = render({ value: 'draft', ghostSuffix: ' completion', 'aria-label': 'Prompt' })
    expect(textarea(container).value).toBe('draft')
    expect(container.querySelector('[aria-hidden="true"]')?.textContent).toBe('draft completion')
    expect(textarea(container).className).toBe('')
  })

  it('applies the supplied class to the textarea rather than its wrapper', () => {
    const container = render({ value: '', className: 'composer__input' })
    expect(textarea(container).className).toBe('composer__input')
    expect(container.querySelector('.prompt-composer')?.classList.contains('composer__input')).toBe(false)
  })

  it('accepts a visible suffix with Tab and forwards other keys', () => {
    const accept = vi.fn()
    const keyDown = vi.fn()
    const container = render({ value: 'draft', ghostSuffix: ' completion', onAcceptCompletion: accept, onKeyDown: keyDown })
    const input = textarea(container)
    input.focus()
    input.setSelectionRange(5, 5)
    act(() => input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Tab', bubbles: true })))
    expect(accept).toHaveBeenCalledWith(' completion')
    expect(keyDown).toHaveBeenCalled()
  })

  it('dismisses a visible completion with Escape', () => {
    const dismiss = vi.fn()
    const container = render({ value: 'draft', ghostSuffix: ' completion', onDismissCompletion: dismiss })
    const bubbled = vi.fn()
    window.addEventListener('keydown', bubbled)
    const input = textarea(container)
    input.focus()
    input.setSelectionRange(5, 5)
    act(() => input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })))
    expect(dismiss).toHaveBeenCalledOnce()
    expect(bubbled).not.toHaveBeenCalled()
    window.removeEventListener('keydown', bubbled)
  })

  it('does not trap completion keys without matching handlers', () => {
    const container = render({ value: 'draft', ghostSuffix: ' completion' })
    const input = textarea(container)
    for (const key of ['Tab', 'Escape']) {
      const event = new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true })
      act(() => input.dispatchEvent(event))
      expect(event.defaultPrevented).toBe(false)
    }
  })

  it('hides the suffix when the cursor moves away from the end or a selection exists', () => {
    const container = render({ value: 'draft', ghostSuffix: ' completion' })
    const input = textarea(container)
    input.focus()
    input.setSelectionRange(2, 2)
    act(() => input.dispatchEvent(new Event('keyup', { bubbles: true })))
    expect(container.querySelector('[aria-hidden="true"]')).toBeNull()
    input.setSelectionRange(0, 5)
    act(() => input.dispatchEvent(new Event('select', { bubbles: true })))
    expect(container.querySelector('[aria-hidden="true"]')).toBeNull()
  })

  it('hides capped ghost text while scrolled and preserves scroll callbacks and draft selection', () => {
    const dismiss = vi.fn()
    const onScroll = vi.fn()
    const container = render({ value: 'line one\nline two\nline three', ghostSuffix: ' completion', maxRows: 2, onDismissCompletion: dismiss, onScroll })
    const input = textarea(container)
    setMetrics(input, 100)
    input.setSelectionRange(input.value.length, input.value.length)
    input.scrollTop = 20
    act(() => input.dispatchEvent(new Event('scroll', { bubbles: true })))
    expect(container.querySelector('.prompt-composer__ghost')).toBeNull()
    expect(onScroll).toHaveBeenCalledOnce()
    expect(dismiss).toHaveBeenCalledOnce()
    expect(input.value).toBe('line one\nline two\nline three')
    expect(input.selectionStart).toBe(input.value.length)
  })

  it('announces partial transcripts and keeps the visual ghost decorative', () => {
    const container = render({ value: '', ghostSuffix: ' suggestion', partialTranscript: 'speaking now', 'aria-label': 'Prompt' })
    expect(container.querySelector('[aria-live="polite"]')?.textContent).toBe('speaking now')
    expect(container.querySelector('[aria-hidden="true"]')).toBeTruthy()
    expect(textarea(container).getAttribute('aria-label')).toBe('Prompt')
  })
})

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

describe('PromptComposer', () => {
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

  it('announces partial transcripts and keeps the visual ghost decorative', () => {
    const container = render({ value: '', ghostSuffix: ' suggestion', partialTranscript: 'speaking now', 'aria-label': 'Prompt' })
    expect(container.querySelector('[aria-live="polite"]')?.textContent).toBe('speaking now')
    expect(container.querySelector('[aria-hidden="true"]')).toBeTruthy()
    expect(textarea(container).getAttribute('aria-label')).toBe('Prompt')
  })
})

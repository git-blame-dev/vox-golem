import { act } from 'react'
import { createRoot } from 'react-dom/client'
import type { Root } from 'react-dom/client'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { SearchModeSelect } from './SearchModeSelect'
import type { SearchMode } from '../lib/searchMode'

let root: Root | null = null
let container: HTMLElement | null = null
afterEach(() => { act(() => root?.unmount()); container?.remove() })

function render(disabled = false, reason?: string, iconOnly = false, value: SearchMode = 'quick') {
  const onChange = vi.fn()
  container = document.createElement('div')
  document.body.append(container)
  root = createRoot(container)
  act(() => root?.render(<SearchModeSelect value={value} disabled={disabled} reason={reason} iconOnly={iconOnly} onChange={onChange} />))
  return { container, onChange }
}

describe('SearchModeSelect', () => {
  it.each([false, true])('omits the manual 3-mode dropdown when iconOnly=%s because automatic search routing owns mode escalation', (iconOnly) => {
    for (const value of ['quick', 'verify', 'research'] as const) {
      const { container: rendered, onChange } = render(false, undefined, iconOnly, value)
      expect(rendered.querySelector('[aria-label="Search mode"]')).toBeNull()
      expect(rendered.querySelector('select')).toBeNull()
      expect(rendered.querySelector('.search-mode')).toBeNull()
      expect(onChange).not.toHaveBeenCalled()
    }
  })

  it.each([false, true])('omits manual mode selector DOM when disabled or given a reason with iconOnly=%s', (iconOnly) => {
    const { container: rendered, onChange } = render(true, 'OpenCode Deep owns research for this turn.', iconOnly, 'research')
    expect(rendered.querySelector('[aria-label="Search mode"]')).toBeNull()
    expect(rendered.querySelector('select')).toBeNull()
    expect(rendered.textContent).toBe('')
    expect(onChange).not.toHaveBeenCalled()
  })
})

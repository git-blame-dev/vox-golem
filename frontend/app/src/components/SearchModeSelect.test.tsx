import { act } from 'react'
import { readFileSync } from 'node:fs'
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
  return { select: container.querySelector('select')!, onChange }
}

describe('SearchModeSelect', () => {
  it.each([false, true])('offers the same native choices and emits only known modes with iconOnly=%s', (iconOnly) => {
    const { select, onChange } = render(false, undefined, iconOnly)
    expect(select.getAttribute('aria-label')).toBe('Search mode')
    expect(Array.from(select.options).map((option) => option.textContent)).toEqual(['Quick', 'Verify', 'Research'])
    act(() => { select.value = 'verify'; select.dispatchEvent(new Event('change', { bubbles: true })) })
    expect(onChange).toHaveBeenCalledWith('verify')
    act(() => { select.value = ''; select.dispatchEvent(new Event('change', { bubbles: true })) })
    expect(onChange).toHaveBeenCalledTimes(1)
  })

  it.each([false, true])('explains disabled research ownership and rejects bypassed changes with iconOnly=%s', (iconOnly) => {
    const reason = 'OpenCode Deep owns research for this turn.'
    const { select, onChange } = render(true, reason, iconOnly)
    expect(select.disabled).toBe(true)
    const hint = document.getElementById(select.getAttribute('aria-describedby')!)
    expect(hint?.textContent).toContain(reason)
    expect(hint?.classList.contains('sr-only')).toBe(iconOnly)
    if (iconOnly) expect(select.getAttribute('title')).toContain(reason)
    act(() => { select.value = 'research'; select.dispatchEvent(new Event('change', { bubbles: true })) })
    expect(onChange).not.toHaveBeenCalled()
  })

  it('retains the visible Search label and reason when iconOnly is omitted', () => {
    const onChange = vi.fn()
    const { select } = render(true, 'Synthetic disabled reason')
    act(() => root?.render(<SearchModeSelect value="quick" disabled reason="Synthetic disabled reason" onChange={onChange} />))
    expect(select.closest('label')?.firstChild?.textContent).toBe('Search:')
    expect(container!.querySelector('.native-icon-select')).toBeNull()
    expect(container!.querySelector('.search-mode__reason')?.textContent).toBe('Synthetic disabled reason')
    expect(container!.querySelector('.search-mode__reason')?.classList.contains('sr-only')).toBe(false)
  })

  it('renders only a decorative collapsed icon and publishes the current mode through native semantics, description and title', () => {
    const { select, onChange } = render(false, undefined, true, 'verify')
    const icon = select.closest('.native-icon-select')!
    expect(icon).not.toBeNull()
    expect(icon.querySelector('svg[data-icon-kind="search"][aria-hidden="true"]')).not.toBeNull()
    expect(container!.querySelector('.search-mode__label')).toBeNull()
    expect(select.selectedOptions[0]?.textContent).toBe('Verify')
    expect(select.getAttribute('title')).toBe('Search mode: Verify')
    expect(icon.getAttribute('title')).toBe('Search mode: Verify')
    expect(container!.querySelector('.search-mode')?.getAttribute('data-search-mode')).toBe('verify')
    const description = document.getElementById(select.getAttribute('aria-describedby')!)!
    expect(description.textContent).toBe('Search mode: Verify')
    expect(description.classList.contains('sr-only')).toBe(true)
    act(() => root?.render(<SearchModeSelect value="research" disabled={false} iconOnly onChange={onChange} />))
    expect(select.selectedOptions[0]?.textContent).toBe('Research')
    expect(select.getAttribute('title')).toBe('Search mode: Research')
    expect(description.textContent).toBe('Search mode: Research')
    expect(container!.querySelector('.search-mode')?.getAttribute('data-search-mode')).toBe('research')
    expect(container!.querySelectorAll('select')).toHaveLength(1)
  })

  it('keeps the selected native value and explains temporary busy gating without a visible status row', () => {
    const { select } = render(true, undefined, true, 'research')
    expect(select.disabled).toBe(true)
    expect(select.selectedOptions[0]?.textContent).toBe('Research')
    expect(select.getAttribute('title')).toBe('Search mode: Research. Temporarily unavailable')
    const description = document.getElementById(select.getAttribute('aria-describedby')!)!
    expect(description.textContent).toBe(select.getAttribute('title'))
    expect(description.classList.contains('sr-only')).toBe(true)
    expect(container!.querySelector('.search-mode__label')).toBeNull()
  })

  it('overlays a transparent native picker over the full 48px icon target', () => {
    const { select } = render(false, undefined, true)
    const sheet = new CSSStyleSheet()
    sheet.replaceSync(readFileSync('src/components/SearchModeSelect.css', 'utf8'))
    const rules = Array.from(sheet.cssRules).filter((rule): rule is CSSStyleRule => rule instanceof CSSStyleRule)
    const targetRule = rules.find((rule) => rule.selectorText === '.search-mode__icon-control')!
    expect(select.parentElement!.matches(targetRule.selectorText)).toBe(true)
    expect(targetRule.style.position).toBe('relative')
    expect(targetRule.style.minWidth).toBe('48px')
    expect(targetRule.style.minHeight).toBe('48px')
    const selectRule = rules.find((rule) => rule.selectorText === '.search-mode__icon-control select')!
    expect(select.matches(selectRule.selectorText)).toBe(true)
    expect(selectRule.style.position).toBe('absolute')
    expect(selectRule.style.getPropertyValue('inset')).toBe('0')
    expect(selectRule.style.width).toBe('100%')
    expect(selectRule.style.height).toBe('100%')
    expect(selectRule.style.boxSizing).toBe('border-box')
    expect(selectRule.style.opacity).toBe('0')
  })
})

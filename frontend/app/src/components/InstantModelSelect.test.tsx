import { act } from 'react'
import { createRoot } from 'react-dom/client'
import type { Root } from 'react-dom/client'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { InstantModelSelect } from './InstantModelSelect'
import type { AssistantOption, InstantChoice } from '../lib/assistantSettings'

let root: Root | null = null
let container: HTMLElement | null = null

afterEach(() => {
  act(() => root?.unmount())
  container?.remove()
})

function renderSelect(options: readonly AssistantOption<InstantChoice>[], disabled = false, canRetry: (value: InstantChoice) => boolean = () => false, variant?: 'default' | 'pill' | 'icon') {
  const onChange = vi.fn()
  container = document.createElement('div')
  document.body.append(container)
  root = createRoot(container)
  act(() => root?.render(<InstantModelSelect {...(variant ? { variant } : {})} id="syntheticInstant" label="Instant model" value="custom-sol-high" options={options} disabled={disabled} canRetry={canRetry} onChange={onChange} />))
  const select = container.querySelector('select')!
  return { select, onChange }
}

describe('InstantModelSelect', () => {
  it('exposes the full selected label in a pill with only one native focus target', () => {
    const label = 'Synthetic model with a deliberately long descriptive label'
    const { select } = renderSelect([{ value: 'custom-sol-high', label, available: true }], false, undefined, 'pill')
    const pill = select.closest('.composer__model-pill')!
    expect(pill).not.toBeNull()
    expect(pill.querySelector('.composer__model-pill-label')?.textContent).toBe(label)
    expect(pill.querySelector('.composer__model-pill-chevron')).not.toBeNull()
    expect(select.selectedOptions[0]?.textContent).toBe(label)
    expect(pill.querySelectorAll('select, button, [tabindex]').length).toBe(1)
    act(() => select.focus())
    expect(document.activeElement).toBe(select)
  })

  it('keeps an absent selected value visibly unavailable without silently selecting another model', () => {
    const { select } = renderSelect([{ value: 'local-fast', label: 'Synthetic other model', available: true }], false, undefined, 'pill')
    expect(select.closest('.composer__model-pill')?.querySelector('.composer__model-pill-label')?.textContent).toBe('Selected model unavailable')
    expect(select.value).toBe('')
  })
  it.each([false, true].flatMap((disabled) => (['icon', 'pill'] as const).map((variant) => ({ disabled, variant }))))('keeps native $variant availability, retry and disabled guards (disabled: $disabled)', ({ disabled, variant }) => {
    const { select, onChange } = renderSelect([
      { value: 'custom-sol-high', label: 'Synthetic selected model', available: true },
      { value: 'local-fast', label: 'Synthetic retry model', available: false, reason: 'Synthetic warming' },
      { value: 'custom-luna-none', label: 'Synthetic unavailable model', available: false },
    ], disabled, (value) => value === 'local-fast', variant)
    expect(select.closest('.native-icon-select')?.querySelector('svg')).toBeTruthy()
    expect(select.getAttribute('aria-label')).toBe('Instant model')
    expect(select.title).toBe('Synthetic selected model')
    act(() => { select.value = 'local-fast'; select.dispatchEvent(new Event('change', { bubbles: true })) })
    expect(onChange.mock.calls).toEqual(disabled ? [] : [['local-fast']])
  })
  it('reports an empty allowlist as unavailable rather than a perpetual loading state or an implicit fallback', () => {
    const { select, onChange } = renderSelect([])
    expect(select.value).toBe('')
    expect(select.selectedOptions[0]?.textContent).toBe('Models unavailable')
    expect(select.selectedOptions[0]?.disabled).toBe(true)
    expect(onChange).not.toHaveBeenCalled()
  })

  it('rejects an unavailable model even when a change event bypasses the disabled option', () => {
    const { select, onChange } = renderSelect([
      { value: 'custom-sol-high', label: 'Synthetic selected model', available: true },
      { value: 'custom-luna-none', label: 'Synthetic unavailable model', available: false, reason: 'Synthetic provider offline' },
    ])
    act(() => { select.value = 'custom-luna-none'; select.dispatchEvent(new Event('change', { bubbles: true })) })
    expect(onChange).not.toHaveBeenCalled()
    expect(select.options[1]?.disabled).toBe(true)
    expect(select.options[1]?.title).toBe('Synthetic provider offline')
  })
})

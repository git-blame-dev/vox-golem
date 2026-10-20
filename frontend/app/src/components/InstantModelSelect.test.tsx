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

function renderSelect(options: readonly AssistantOption<InstantChoice>[], disabled = false, canRetry: (value: InstantChoice) => boolean = () => false, variant?: 'default' | 'pill' | 'icon', value: InstantChoice = 'custom-sol-high') {
  const onChange = vi.fn()
  container = document.createElement('div')
  document.body.append(container)
  root = createRoot(container)
  act(() => root?.render(<InstantModelSelect {...(variant ? { variant } : {})} id="syntheticInstant" label="Instant model" value={value} options={options} disabled={disabled} canRetry={canRetry} onChange={onChange} />))
  const select = container.querySelector('select')!
  return { select, onChange, container }
}

describe('InstantModelSelect', () => {
  it('exposes the full selected label in a pill with a single trigger button and opens the vertical model + horizontal effort popup', () => {
    const label = 'Synthetic model with a deliberately long descriptive label'
    const { select, container: rootEl, onChange } = renderSelect([
      { value: 'custom-sol-high', label, available: true },
      { value: 'custom-sol-low', label: 'Custom: GPT-6.1 Sol Low', available: true },
      { value: 'agy-argon-high', label: 'Agy: Gemini 4 Argon High', available: true },
      { value: 'agy-argon-low', label: 'Agy: Gemini 4 Argon Low', available: true },
    ], false, undefined, 'pill')
    const pill = select.closest('.composer__model-pill')!
    expect(pill).not.toBeNull()
    expect(pill.querySelector('.composer__model-pill-label')?.textContent).toBe(label)
    expect(pill.querySelector('.composer__model-pill-chevron')).not.toBeNull()
    expect(select.selectedOptions[0]?.textContent).toBe(label)

    const trigger = pill.querySelector<HTMLButtonElement>('.composer__model-pill-trigger')!
    expect(trigger).not.toBeNull()
    act(() => trigger.click())

    const popup = rootEl.querySelector('.composer__model-popup')!
    expect(popup).not.toBeNull()
    const modelButtons = Array.from(popup.querySelectorAll<HTMLButtonElement>('.composer__model-popup-item'))
    expect(modelButtons.map((b) => b.textContent)).toEqual([label, 'Gemini 4 Argon'])

    const effortButtons = Array.from(popup.querySelectorAll<HTMLButtonElement>('.composer__model-popup-effort-chip'))
    expect(effortButtons.map((b) => b.textContent)).toEqual(['High', 'Low'])

    act(() => modelButtons[1]!.click())
    expect(onChange).toHaveBeenCalledWith('agy-argon-high')
    expect(rootEl.querySelector('.composer__model-popup')).not.toBeNull()

    act(() => effortButtons[1]!.click())
    expect(onChange).toHaveBeenCalledWith('custom-sol-low')
    expect(rootEl.querySelector('.composer__model-popup')?.getAttribute('data-state')).toBe('closing')
  })

  it('displays concise model and effort in the single pill label for catalog models', () => {
    const { select } = renderSelect([
      { value: 'custom-sol-high', label: 'Custom: GPT-6.1 Sol High', available: true },
      { value: 'custom-sol-low', label: 'Custom: GPT-6.1 Sol Low', available: true },
    ], false, undefined, 'pill')
    expect(select.closest('.composer__model-pill')?.querySelector('.composer__model-pill-label')?.textContent).toBe('GPT-6.1 Sol High')
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
      { value: 'custom-sol-none', label: 'Synthetic unavailable model', available: false },
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
      { value: 'custom-sol-none', label: 'Synthetic unavailable model', available: false, reason: 'Synthetic provider offline' },
    ])
    act(() => { select.value = 'custom-sol-none'; select.dispatchEvent(new Event('change', { bubbles: true })) })
    expect(onChange).not.toHaveBeenCalled()
    expect(select.options[1]?.disabled).toBe(true)
    expect(select.options[1]?.title).toBe('Synthetic provider offline')
  })
})

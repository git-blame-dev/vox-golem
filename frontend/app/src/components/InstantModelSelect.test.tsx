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
  vi.useRealTimers()
})

function renderSelect(options: readonly AssistantOption<InstantChoice>[], disabled = false, canRetry: (value: InstantChoice) => boolean = () => false, variant?: 'default' | 'pill' | 'icon', value: InstantChoice = 'custom-sol-high', fastMode = false, onFastModeChange?: (fastMode: boolean) => void) {
  const onChange = vi.fn()
  container = document.createElement('div')
  document.body.append(container)
  root = createRoot(container)
  act(() => root?.render(<InstantModelSelect {...(variant ? { variant } : {})} id="syntheticInstant" label="Instant model" value={value} options={options} disabled={disabled} canRetry={canRetry} onChange={onChange} fastMode={fastMode} {...(onFastModeChange ? { onFastModeChange } : {})} />))
  const select = container.querySelector('select')!
  return { select, onChange, container }
}

describe('InstantModelSelect', () => {
  it.each(['Escape', 'selection'] as const)('restores actual keyboard focus after %s and keeps closing controls disabled', (reason) => {
    vi.useFakeTimers()
    const { container: el } = renderSelect([{ value: 'local-fast', label: 'Synthetic model', available: true }], false, undefined, 'pill', 'local-fast')
    const trigger = el.querySelector<HTMLButtonElement>('button')!
    trigger.focus()
    act(() => trigger.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true })))
    const option = el.querySelector<HTMLButtonElement>('[role="option"]')!
    expect(document.activeElement).toBe(option)
    act(() => {
      if (reason === 'Escape') option.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
      else option.click()
    })
    expect(document.activeElement).toBe(trigger)
    expect(option.disabled).toBe(true)
    act(() => vi.advanceTimersByTime(160))
    expect(document.activeElement).toBe(trigger)
    expect(el.querySelector('[role="dialog"]')).toBeNull()
  })

  it('does not steal outside focus and cancels an old close on reopen', () => {
    vi.useFakeTimers()
    const { container: el } = renderSelect([{ value: 'local-fast', label: 'Synthetic model', available: true }], false, undefined, 'pill', 'local-fast')
    const trigger = el.querySelector<HTMLButtonElement>('button')!
    act(() => trigger.click())
    const outside = document.createElement('button')
    el.append(outside)
    act(() => outside.focus())
    act(() => outside.dispatchEvent(new Event('pointerdown', { bubbles: true })))
    expect(el.querySelector('[role="dialog"]')?.getAttribute('data-state')).toBe('closing')
    expect(document.activeElement).toBe(outside)
    act(() => trigger.click())
    act(() => vi.advanceTimersByTime(160))
    expect(el.querySelector('[role="dialog"]')?.getAttribute('data-state')).toBe('open')
    expect(document.activeElement).toBe(outside)
  })

  it('does not restore an unavailable trigger', () => {
    vi.useFakeTimers()
    const options = [{ value: 'local-fast' as const, label: 'Synthetic model', available: true }]
    const { container: el } = renderSelect(options, false, undefined, 'pill', 'local-fast')
    const trigger = el.querySelector<HTMLButtonElement>('button')!
    act(() => trigger.click())
    const option = el.querySelector<HTMLButtonElement>('[role="option"]')!
    option.focus()
    act(() => root?.render(<InstantModelSelect variant="pill" id="syntheticInstant" label="Instant model" value="local-fast" options={options} disabled canRetry={() => false} onChange={vi.fn()} />))
    act(() => option.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })))
    act(() => vi.advanceTimersByTime(160))
    expect(document.activeElement).not.toBe(trigger)
  })

  it('preserves navigation focus and clears a pending close on unmount', () => {
    vi.useFakeTimers()
    const { container: el } = renderSelect([{ value: 'local-fast', label: 'Synthetic model', available: true }], false, undefined, 'pill', 'local-fast')
    const trigger = el.querySelector<HTMLButtonElement>('button')!
    act(() => trigger.click())
    const option = el.querySelector<HTMLButtonElement>('[role="option"]')!
    option.focus()
    const outside = document.createElement('button')
    el.append(outside)
    act(() => outside.focus())
    expect(document.activeElement).toBe(outside)
    expect(option.disabled).toBe(true)
    act(() => root?.unmount())
    root = null
    expect(vi.getTimerCount()).toBe(0)
    act(() => vi.advanceTimersByTime(160))
    expect(document.activeElement).not.toBe(trigger)
  })

  it('exposes the full selected label in a pill with a single trigger button and opens the vertical model + horizontal effort popup', () => {
    const label = 'Synthetic model with a deliberately long descriptive label'
    const onFastModeChange = vi.fn()
    const { select, container: rootEl, onChange } = renderSelect([
      { value: 'custom-sol-high', label, available: true },
      { value: 'custom-sol-low', label: 'Custom: GPT-6.1 Sol Low', available: true },
      { value: 'agy-argon-high', label: 'Agy: Gemini 4 Argon High', available: true },
      { value: 'agy-argon-low', label: 'Agy: Gemini 4 Argon Low', available: true },
    ], false, undefined, 'pill', 'custom-sol-high', false, onFastModeChange)
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

    const effortButtons = Array.from(popup.querySelectorAll<HTMLButtonElement>('.composer__model-popup-effort-row .composer__model-popup-effort-chip'))
    expect(effortButtons.map((b) => b.textContent)).toEqual(['High', 'Low'])

    const fastChips = Array.from(popup.querySelectorAll<HTMLButtonElement>('.composer__model-popup-fast-chip'))
    expect(fastChips.map((b) => b.textContent)).toEqual(['Off', 'On'])
    expect(fastChips[0]?.getAttribute('aria-pressed')).toBe('true')
    expect(fastChips[1]?.getAttribute('aria-pressed')).toBe('false')

    act(() => modelButtons[1]!.click())
    expect(onChange).toHaveBeenCalledWith('agy-argon-high')
    expect(rootEl.querySelector('.composer__model-popup')).toBe(popup)
    expect(rootEl.querySelector('.composer__model-popup')?.getAttribute('data-state')).toBe('open')

    act(() => fastChips[1]!.click())
    expect(onFastModeChange).toHaveBeenCalledWith(true)
    expect(rootEl.querySelector('.composer__model-popup')?.getAttribute('data-state')).toBe('closing')

    act(() => trigger.click())
    expect(rootEl.querySelector('.composer__model-popup')?.getAttribute('data-state')).toBe('open')

    act(() => effortButtons[1]!.click())
    expect(onChange).toHaveBeenCalledWith('custom-sol-low')
    expect(rootEl.querySelector('.composer__model-popup')?.getAttribute('data-state')).toBe('closing')
  })

  it('keeps an open popup mounted when disabled transiently flips during an in-flight model change', () => {
    const options: readonly AssistantOption<InstantChoice>[] = [
      { value: 'custom-sol-high', label: 'Custom: GPT-6.1 Sol High', available: true },
      { value: 'custom-sol-low', label: 'Custom: GPT-6.1 Sol Low', available: true },
      { value: 'custom-luna6-high', label: 'Custom: GPT-6 Luna High', available: true },
      { value: 'custom-luna-low', label: 'Custom: GPT-6 Luna Low', available: true },
    ]
    const onChange = vi.fn()
    const { select, container: rootEl } = renderSelect(options, false, undefined, 'pill', 'custom-sol-high')
    const trigger = select.closest('.composer__model-pill')!.querySelector<HTMLButtonElement>('.composer__model-pill-trigger')!
    act(() => trigger.click())

    const popupBefore = rootEl.querySelector('.composer__model-popup')!
    expect(popupBefore).not.toBeNull()
    expect(popupBefore.getAttribute('data-state')).toBe('open')

    act(() => root?.render(<InstantModelSelect variant="pill" id="syntheticInstant" label="Instant model" value="custom-luna6-high" options={options} disabled={true} canRetry={() => false} onChange={onChange} />))
    expect(rootEl.querySelector('.composer__model-popup')).toBe(popupBefore)
    expect(rootEl.querySelector('.composer__model-popup')?.getAttribute('data-state')).toBe('open')

    act(() => root?.render(<InstantModelSelect variant="pill" id="syntheticInstant" label="Instant model" value="custom-luna6-high" options={options} disabled={false} canRetry={() => false} onChange={onChange} />))
    expect(rootEl.querySelector('.composer__model-popup')).toBe(popupBefore)
    expect(rootEl.querySelector('.composer__model-popup')?.getAttribute('data-state')).toBe('open')
  })

  it('hides the fast toggle when the selected model family does not support fast mode', () => {
    const onFastModeChange = vi.fn()
    const { select, container: rootEl } = renderSelect([
      { value: 'agy-argon-high', label: 'Agy: Gemini 4 Argon High', available: true },
      { value: 'agy-argon-low', label: 'Agy: Gemini 4 Argon Low', available: true },
    ], false, undefined, 'pill', 'agy-argon-high', true, onFastModeChange)
    const trigger = select.closest('.composer__model-pill')!.querySelector<HTMLButtonElement>('.composer__model-pill-trigger')!
    act(() => trigger.click())
    expect(rootEl.querySelector('.composer__model-popup-fast-row')).toBeNull()
  })

  it('displays concise model and effort in the single pill label for catalog models', () => {
    const { select } = renderSelect([
      { value: 'custom-sol-high', label: 'Custom: GPT-6.1 Sol High', available: true },
      { value: 'custom-sol-low', label: 'Custom: GPT-6.1 Sol Low', available: true },
    ], false, undefined, 'pill', 'custom-sol-high', true)
    expect(select.closest('.composer__model-pill')?.querySelector('.composer__model-pill-label')?.textContent).toBe('GPT-6.1 Sol High · Fast')
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

import type { AssistantOption, InstantChoice } from '../lib/assistantSettings'
import { ControlIcon } from './ControlIcon'

interface InstantModelSelectProps {
  readonly id: string
  readonly label: string
  readonly value: InstantChoice
  readonly options: readonly AssistantOption<InstantChoice>[]
  readonly disabled: boolean
  readonly canRetry: (value: InstantChoice) => boolean
  readonly onChange: (value: InstantChoice) => void
  readonly variant?: 'default' | 'pill' | 'icon'
}

export function InstantModelSelect({ id, label, value, options, disabled, canRetry, onChange, variant = 'default' }: InstantModelSelectProps) {
  const selected = options.find((option) => option.value === value)
  const selectedLabel = selected?.label ?? (options.length === 0 ? 'Models unavailable' : 'Selected model unavailable')
  const select = (
    <select id={id} aria-label={label} title={selected?.label ?? 'Models unavailable'} value={selected ? value : ''} disabled={disabled} onChange={(event) => {
      if (disabled) return
      const option = options.find((candidate) => candidate.value === event.target.value)
      if (option && (option.available || canRetry(option.value))) onChange(option.value)
    }}>
      {!selected ? <option value="" disabled>{options.length === 0 ? 'Models unavailable' : 'Selected model unavailable'}</option> : null}
      {options.map((option) => <option key={option.value} value={option.value} disabled={!option.available && !canRetry(option.value)} title={option.reason}>
        {option.label}{option.available ? '' : ` — ${option.reason ?? 'Unavailable'}`}
      </option>)}
    </select>
  )
  if (variant === 'default') return select
  return <span className={variant === 'pill' ? 'composer__model-pill native-icon-select' : 'native-icon-select shell__icon-control'} title={selectedLabel}>
    <ControlIcon kind="model" />
    {variant === 'pill' ? <>
      <span className="composer__model-pill-label" aria-hidden="true">{selectedLabel}</span>
      <svg className="composer__model-pill-chevron" viewBox="0 0 16 16" aria-hidden="true" focusable="false"><path d="M4 6l4 4 4-4" /></svg>
    </> : null}
    {select}
  </span>
}

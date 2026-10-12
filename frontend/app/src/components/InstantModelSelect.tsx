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
  readonly iconOnly?: boolean
}

export function InstantModelSelect({ id, label, value, options, disabled, canRetry, onChange, iconOnly = false }: InstantModelSelectProps) {
  const selected = options.find((option) => option.value === value)
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
  return iconOnly ? <span className="native-icon-select shell__icon-control" title={selected?.label ?? 'Models unavailable'}><ControlIcon kind="model" />{select}</span> : select
}

import { useId } from 'react'
import { SEARCH_MODES, SEARCH_MODE_LABELS } from '../lib/searchMode'
import type { SearchMode } from '../lib/searchMode'
import { ControlIcon } from './ControlIcon'
import './SearchModeSelect.css'

export function SearchModeSelect({ value, disabled, reason, onChange, iconOnly = false }: {
  value: SearchMode; disabled: boolean; reason?: string | undefined; onChange: (mode: SearchMode) => void; iconOnly?: boolean
}) {
  const hintId = useId()
  const description = `Search mode: ${SEARCH_MODE_LABELS[value]}${reason ? `. ${reason}` : disabled ? '. Temporarily unavailable' : ''}`
  const select = <select aria-label="Search mode" value={value} disabled={disabled} title={description} aria-describedby={iconOnly || reason ? hintId : undefined}
    onChange={(event) => {
      const mode = event.target.value as SearchMode
      if (!disabled && SEARCH_MODES.includes(mode)) onChange(mode)
    }}>
    {SEARCH_MODES.map((mode) => <option key={mode} value={mode}>{SEARCH_MODE_LABELS[mode]}</option>)}
  </select>
  return <div className={iconOnly ? 'search-mode search-mode--icon-only' : 'search-mode'} data-search-mode={iconOnly ? value : undefined}>
    {iconOnly ? <span className="native-icon-select shell__icon-control search-mode__icon-control" title={description}>
      <ControlIcon kind="search" /><span className="shell__overflow-label" aria-hidden="true">Search: {SEARCH_MODE_LABELS[value]}</span>{select}
    </span> : <label className="search-mode__label">Search:{select}</label>}
    {iconOnly ? <span id={hintId} className="search-mode__reason sr-only">{description}</span> : reason ? <details className="search-mode__help">
      <summary>Search info</summary>
      <p id={hintId} className="search-mode__reason">{reason}</p>
    </details> : null}
  </div>
}

import type { SearchMode } from '../lib/searchMode'

export interface SearchModeSelectProps {
  readonly value: SearchMode
  readonly disabled: boolean
  readonly reason?: string | undefined
  readonly onChange: (mode: SearchMode) => void
  readonly iconOnly?: boolean | undefined
}

export function SearchModeSelect(props: SearchModeSelectProps): null {
  void props
  return null
}

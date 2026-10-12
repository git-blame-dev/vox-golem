import type { ReactNode } from 'react'

/** Native disclosure keeps secondary controls reachable without shrinking primary targets. */
export function PhoneActionOverflow({ children }: { readonly children: ReactNode }) {
  return <details className="shell__overflow" onKeyDown={(event) => {
    if (event.key !== 'Escape' || !event.currentTarget.open) return
    event.preventDefault()
    event.stopPropagation()
    event.currentTarget.open = false
    event.currentTarget.querySelector('summary')?.focus()
  }}>
    <summary className="shell__control shell__icon-control" aria-label="More actions" title="More actions">
      <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false"><circle cx="5" cy="12" r="1" /><circle cx="12" cy="12" r="1" /><circle cx="19" cy="12" r="1" /></svg>
    </summary>
    <div className="shell__overflow-actions" role="group" aria-label="Secondary actions">{children}</div>
  </details>
}

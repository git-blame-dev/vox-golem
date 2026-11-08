import { useId, useRef } from 'react'
import type { ReactNode } from 'react'
import { ControlIcon } from '../components/ControlIcon'

/** A disclosure of ordinary actions, preserving native Tab navigation. */
export function ActionMenu({ children, label = 'More actions', icon = 'more', id: suppliedId }: {
  readonly children: ReactNode; readonly label?: string; readonly icon?: 'more' | 'conversation'; readonly id?: string
}) {
  const id = useId()
  const details = useRef<HTMLDetailsElement>(null)
  const close = () => { if (details.current) details.current.open = false }
  return <details id={suppliedId} className="ui-action-menu" ref={details} onBlur={(event) => {
    // A child dialog restores focus to its invoker; keep that invoker visible.
    const childDialogOpen = event.currentTarget.querySelector('[aria-haspopup="dialog"][aria-expanded="true"]') !== null
    if (!childDialogOpen && event.relatedTarget instanceof Node && !event.currentTarget.contains(event.relatedTarget)) close()
  }} onKeyDown={(event) => {
    if (event.key !== 'Escape' || !details.current?.open) return
    event.preventDefault()
    event.stopPropagation()
    close()
    details.current.querySelector('summary')?.focus()
  }}>
    <summary id={id} role="button" aria-label={label} title={icon === 'more' ? undefined : label} className={`ui-button ui-button--text${icon === 'more' ? ' ui-icon-button' : ''}`}>
      <ControlIcon kind={icon} />{icon !== 'more' ? <span>{label}</span> : null}
    </summary>
    <div className="ui-action-menu__panel" role="group" aria-label={label === 'More actions' ? 'Additional actions' : label}>{children}</div>
  </details>
}

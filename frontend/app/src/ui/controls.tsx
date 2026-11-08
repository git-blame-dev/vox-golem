import { forwardRef, useId } from 'react'
import type { ButtonHTMLAttributes, DetailsHTMLAttributes, InputHTMLAttributes, SelectHTMLAttributes, ReactNode } from 'react'

type ButtonProps = ButtonHTMLAttributes<HTMLButtonElement> & {
  readonly variant?: 'text' | 'tonal' | 'filled'
}

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button({
  type = 'button', variant = 'tonal', className = '', ...props
}, ref) {
  return <button {...props} ref={ref} type={type} className={`ui-button ui-button--${variant} ${className}`} />
})

export const IconButton = forwardRef<HTMLButtonElement, ButtonProps & { readonly label: string }>(function IconButton({
  label, className = '', variant = 'text', ...props
}, ref) {
  return <Button {...props} ref={ref} variant={variant} aria-label={label} title={props.title ?? label} className={`ui-icon-button ${className}`} />
})

export function SettingSection({ title, children, description }: { readonly title: string; readonly children: ReactNode; readonly description?: string }) {
  const id = useId()
  return <section className="ui-setting-section" aria-labelledby={id}>
    <h3 id={id}>{title}</h3>
    {description ? <p className="ui-supporting-text">{description}</p> : null}
    {children}
  </section>
}

function SettingCopy({ title, description }: { readonly title: string; readonly description?: string }) {
  return <span className="ui-setting-row__copy"><span className="ui-setting-row__title">{title}</span>{description ? <small>{description}</small> : null}</span>
}

export function SettingRow({ title, description, children }: { readonly title: string; readonly description?: string; readonly children?: ReactNode }) {
  return <div className="ui-setting-row"><SettingCopy title={title} {...(description === undefined ? {} : { description })} />{children}</div>
}

export function SettingLink({ title, description, className = '', ...props }: ButtonHTMLAttributes<HTMLButtonElement> & { readonly title: string; readonly description?: string }) {
  return <button {...props} type="button" className={`ui-setting-row ui-setting-link ${className}`}><SettingCopy title={title} {...(description === undefined ? {} : { description })} /><svg viewBox="0 0 24 24" aria-hidden="true"><path d="m9 5 7 7-7 7" /></svg></button>
}

const describedBy = (...ids: readonly (string | undefined)[]) => ids.filter(Boolean).join(' ') || undefined

export function SelectSetting({ label, description, children, ...props }: SelectHTMLAttributes<HTMLSelectElement> & { readonly label: string; readonly description?: string }) {
  const id = useId()
  return <label className="ui-setting-select"><span className="ui-setting-row__title">{label}</span>{description ? <small id={`${id}-help`}>{description}</small> : null}<select {...props} aria-label={props['aria-label'] ?? label} aria-describedby={describedBy(props['aria-describedby'], description ? `${id}-help` : undefined)}>{children}</select></label>
}

export function SwitchField({ label, description, ...props }: Omit<InputHTMLAttributes<HTMLInputElement>, 'type' | 'role' | 'children'> & {
  readonly label: string; readonly description?: string
}) {
  const id = useId()
  return <label className="ui-switch-field">
    <span><span>{label}</span>{description ? <small id={id}>{description}</small> : null}</span>
    <input {...props} type="checkbox" role="switch" aria-label={props['aria-label'] ?? label} aria-describedby={describedBy(props['aria-describedby'], description ? id : undefined)} />
  </label>
}

export function Disclosure({ title, children, ...props }: Omit<DetailsHTMLAttributes<HTMLDetailsElement>, 'title'> & { readonly title: string }) {
  const id = useId()
  return <details {...props}>
    <summary id={`${id}-trigger`} role="button" aria-controls={`${id}-content`}>{title}</summary>
    <div id={`${id}-content`}>{children}</div>
  </details>
}

import type { JSX } from 'react'
import type { AppUpdateController, UpdateState } from '../lib/useAppUpdates'

interface UpdateSettingsProps {
  readonly updates: AppUpdateController
}

export function UpdateSettings({ updates }: UpdateSettingsProps): JSX.Element {
  const presentation = updatePresentation(updates)

  return (
    <section className="settings-panel__updates" aria-labelledby="updates-title">
      <div className="settings-panel__update-row">
        <div className="settings-panel__update-copy">
          <strong id="updates-title">Updates</strong>
          <p
            className={presentation.error ? 'settings-panel__update-error' : undefined}
            role={presentation.error ? 'alert' : presentation.pending ? 'status' : undefined}
          >
            {presentation.status}
          </p>
        </div>
        {presentation.action}
      </div>
    </section>
  )
}

function updatePresentation(updates: AppUpdateController): {
  readonly status: string
  readonly action: JSX.Element | null
  readonly error?: boolean
  readonly pending?: boolean
} {
  const { state } = updates
  const action = (label: string, run: () => Promise<void>): JSX.Element => (
    <button type="button" className="shell__control" onClick={() => void run()}>{label}</button>
  )

  if (state.kind === 'checking') return { status: 'Checking...', action: null, pending: true }
  if (state.kind === 'browser') return { status: 'Available in the packaged app', action: null }
  if (state.kind === 'error') {
    return { status: `Check failed: ${state.message}`, action: action('Retry', updates.check), error: true }
  }
  if (state.kind === 'installing') {
    return { status: 'Downloading and verifying...', action: null, pending: true }
  }
  if (state.kind === 'installed') {
    return { status: 'Update installed', action: action('Restart', updates.restart) }
  }

  return resultPresentation(state, updates, action)
}

function resultPresentation(
  state: Extract<UpdateState, { readonly kind: 'result' }>,
  updates: AppUpdateController,
  action: (label: string, run: () => Promise<void>) => JSX.Element,
): { readonly status: string; readonly action: JSX.Element | null; readonly pending?: boolean } {
  const { result } = state
  if (result.status === 'available') {
    return { status: 'Update available', action: action('Install update', () => updates.install(result.version)) }
  }
  if (result.status === 'installing') {
    return { status: 'Downloading and verifying...', action: null, pending: true }
  }
  if (result.status === 'installed') {
    return { status: 'Update installed', action: action('Restart', updates.restart) }
  }
  if (result.status === 'up_to_date') {
    return { status: 'Up to date', action: action('Check', updates.check) }
  }
  if (result.status === 'unavailable') {
    return { status: result.reason, action: action('Check', updates.check) }
  }
  return { status: result.reason, action: null }
}

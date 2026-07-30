import type { JSX } from 'react'
import type { AppUpdateController, UpdateState } from '../lib/useAppUpdates'

interface UpdateSettingsProps {
  readonly updates: AppUpdateController
  readonly installationDisabled?: boolean
}

export function UpdateSettings({ updates, installationDisabled = false }: UpdateSettingsProps): JSX.Element {
  const presentation = updatePresentation(updates, installationDisabled)

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

function updatePresentation(updates: AppUpdateController, installationDisabled: boolean): {
  readonly status: string
  readonly action: JSX.Element | null
  readonly error?: boolean
  readonly pending?: boolean
} {
  const { state } = updates
  const action = (label: string, run: () => Promise<void>, disabled = false): JSX.Element => (
    <button type="button" className="shell__control" onClick={() => void run()} disabled={disabled}>{label}</button>
  )

  if (state.kind === 'checking') return { status: 'Checking...', action: null, pending: true }
  if (state.kind === 'browser') return { status: 'Available in the packaged app', action: null }
  if (state.kind === 'error') {
    return { status: `Update failed: ${state.message}`, action: action('Retry', updates.check), error: true }
  }
  if (state.kind === 'installing') {
    return { status: progressStatus(state.progress), action: null, pending: true }
  }
  if (state.kind === 'installed') {
    return { status: 'Update installed', action: state.installBehavior === 'install_and_restart' ? null : action('Restart', updates.restart, updates.restartPending) }
  }

  return resultPresentation(state, updates, action, installationDisabled)
}

function resultPresentation(
  state: Extract<UpdateState, { readonly kind: 'result' }>,
  updates: AppUpdateController,
  action: (label: string, run: () => Promise<void>, disabled?: boolean) => JSX.Element,
  installationDisabled: boolean,
): { readonly status: string; readonly action: JSX.Element | null; readonly pending?: boolean } {
  const { result } = state
  if (result.status === 'available') {
    const label = result.installBehavior === 'install_and_restart' ? 'Install and restart' : 'Install update'
    return {
      status: installationDisabled
        ? `Update ${result.version} available. Finish active work before installing.`
        : `Update ${result.version} available`,
      action: action(label, () => updates.install(result.version, result.installBehavior), installationDisabled),
    }
  }
  if (result.status === 'installing') {
    return { status: progressStatus(updates.progress), action: null, pending: true }
  }
  if (result.status === 'installed') {
    return { status: 'Update installed', action: result.installBehavior === 'install_and_restart' ? null : action('Restart', updates.restart, updates.restartPending) }
  }
  if (result.status === 'up_to_date') {
    return { status: 'Up to date', action: action('Check', updates.check) }
  }
  if (result.status === 'unavailable') {
    return { status: result.reason, action: action('Check', updates.check) }
  }
  return { status: result.reason, action: null }
}

function progressStatus(progress: AppUpdateController['progress']): string {
  if (!progress) return 'Downloading and verifying...'
  const downloaded = formatMb(progress.downloadedBytes)
  if (progress.phase === 'verifying') return `Verifying ${downloaded} MB...`
  if (progress.phase === 'installing') return 'Starting installer...'
  if (progress.totalBytes === undefined || progress.totalBytes === 0) return `Downloading: ${downloaded} MB`
  const percent = Math.min(100, Math.floor(progress.downloadedBytes / progress.totalBytes * 100))
  return `Downloading: ${percent}% (${downloaded} / ${formatMb(progress.totalBytes)} MB)`
}

function formatMb(bytes: number): string { return (bytes / 1_000_000).toFixed(1) }

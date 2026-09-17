import type { JSX } from 'react'
import type { UpdateSnapshot } from '../lib/appUpdates'
import { retryCurrentOperation } from '../lib/useAppUpdates'
import type { AppUpdateController } from '../lib/useAppUpdates'

interface UpdateSettingsProps {
  readonly updates: AppUpdateController
  readonly installationDisabled?: boolean
}

export function UpdateSettings({ updates, installationDisabled = false }: UpdateSettingsProps): JSX.Element {
  const snapshot = updates.state.kind === 'snapshot' ? updates.state.snapshot : null
  const presentation = updatePresentation(updates, snapshot, installationDisabled)

  return (
    <section className="settings-panel__updates" aria-labelledby="updates-title">
      <label className="settings-panel__update-toggle">
        <input
          type="checkbox"
          checked={snapshot?.autoDownloadEnabled ?? true}
          disabled={updates.autoDownloadSaving || snapshot === null}
          onChange={(event) => void updates.setAutoDownloadEnabled(event.currentTarget.checked)}
        />
        Download updates automatically
      </label>
      <label className="settings-panel__update-toggle">
        <input
          type="checkbox"
          checked={snapshot?.autoInstallEnabled ?? true}
          disabled={updates.autoInstallSaving || snapshot === null}
          onChange={(event) => void updates.setAutoInstallEnabled(event.currentTarget.checked)}
        />
        Install downloaded updates automatically
      </label>
      {updates.autoDownloadSaving ? <p role="status">Saving automatic update preference...</p> : null}
      {updates.autoDownloadError !== null ? (
        <div className="settings-panel__update-preference-error" role="alert">
          <p>{updates.autoDownloadError}</p>
          <button type="button" className="shell__control" disabled={updates.autoDownloadSaving} onClick={() => void updates.retryAutoDownloadSave()}>
            Retry saving
          </button>
        </div>
      ) : null}
      {updates.autoInstallSaving ? <p role="status">Saving automatic install preference...</p> : null}
      {updates.autoInstallError !== null ? (
        <div className="settings-panel__update-preference-error" role="alert">
          <p>{updates.autoInstallError}</p>
          <button type="button" className="shell__control" disabled={updates.autoInstallSaving} onClick={() => void updates.retryAutoInstallSave()}>
            Retry saving
          </button>
        </div>
      ) : null}
      <div className="settings-panel__update-row">
        <div className="settings-panel__update-copy">
          <strong id="updates-title">Updates</strong>
          <p className={presentation.error ? 'settings-panel__update-error' : undefined} role={presentation.error ? 'alert' : presentation.pending ? 'status' : undefined}>
            {presentation.status}
          </p>
        </div>
        {presentation.action}
      </div>
      {snapshot?.notes !== null && snapshot?.notes !== undefined && hasUpdateMetadata(snapshot) ? (
        <p className="settings-panel__update-notes">{snapshot.notes}</p>
      ) : null}
    </section>
  )
}

function updatePresentation(
  updates: AppUpdateController,
  snapshot: UpdateSnapshot | null,
  installationDisabled: boolean,
): { readonly status: string; readonly action: JSX.Element | null; readonly error?: boolean; readonly pending?: boolean } {
  const action = (label: string, run: () => Promise<void>, disabled = false): JSX.Element => (
    <button type="button" className="shell__control" onClick={() => void run()} disabled={disabled}>{label}</button>
  )
  if (updates.state.kind === 'browser') return { status: 'Available in the packaged app', action: null }
  if (snapshot === null) {
    return updates.actionError === null
      ? { status: 'Loading update status...', action: null, pending: true }
      : { status: `Update failed: ${updates.actionError}`, action: action('Retry', updates.check), error: true }
  }
  if (updates.actionError !== null) {
    return {
      status: updates.actionError.startsWith('Restart failed:') ? updates.actionError : `Update failed: ${updates.actionError}`,
      action: snapshot.phase === 'restart_required'
        ? action('Restart VoxGolem', updates.restart, updates.actionPending)
        : action('Retry', () => retryCurrentOperation(updates), updates.actionPending),
      error: true,
    }
  }
  if (snapshot.phase === 'restart_required') {
    const status = snapshot.error === null
      ? (snapshot.reason ?? 'Restart is required before using VoxGolem again.')
      : `${snapshot.reason ?? 'Restart is required before using VoxGolem again.'} ${snapshot.error}`
    return { status, action: action('Restart VoxGolem', updates.restart, updates.actionPending), error: snapshot.error !== null }
  }
  if (snapshot.error !== null) {
    return {
      status: `Update failed: ${snapshot.error}`,
      action: action('Retry', () => retryCurrentOperation(updates), updates.actionPending || (installationDisabled && snapshot.phase === 'ready')),
      error: true,
    }
  }
  if (snapshot.operation === 'check' || snapshot.phase === 'checking') {
    return { status: 'Checking...', action: null, pending: true }
  }
  if (snapshot.operation === 'download' || snapshot.phase === 'downloading') {
    return { status: progressStatus(snapshot), action: null, pending: true }
  }
  if (snapshot.operation === 'install' || snapshot.phase === 'installing') {
    return { status: 'Starting installer...', action: null, pending: true }
  }
  if (snapshot.phase === 'ready') {
    return {
      status: `Update ${snapshot.version ?? ''} is ready`,
      action: (
        <span className="settings-panel__update-actions">
          {action('Restart and update', updates.install, installationDisabled || updates.actionPending)}
          {action('Later', updates.later, updates.actionPending)}
        </span>
      ),
    }
  }
  if (snapshot.phase === 'available') {
    return { status: `Update ${snapshot.version ?? ''} available`, action: action('Download', updates.download, updates.actionPending) }
  }
  if (snapshot.phase === 'up_to_date') return { status: 'Up to date', action: action('Check', updates.check) }
  if (snapshot.phase === 'unavailable') return { status: snapshot.reason ?? 'Update information is unavailable.', action: action('Check', updates.check) }
  if (snapshot.phase === 'unsupported') return { status: snapshot.reason ?? 'Automatic updates are unsupported.', action: null }
  return { status: 'Update postponed for this session', action: action('Check', updates.check) }
}

function hasUpdateMetadata(snapshot: UpdateSnapshot): boolean {
  return snapshot.phase === 'available' || snapshot.phase === 'downloading' || snapshot.phase === 'ready'
}

function progressStatus(snapshot: UpdateSnapshot): string {
  if (snapshot.progressPhase === 'verifying') return `Verifying ${formatMb(snapshot.downloadedBytes)} MB...`
  if (snapshot.progressPhase === 'installing') return 'Starting installer...'
  if (snapshot.totalBytes === null || snapshot.totalBytes === 0) {
    return snapshot.downloadedBytes === 0 ? 'Downloading and verifying...' : `Downloading: ${formatMb(snapshot.downloadedBytes)} MB`
  }
  const percent = Math.min(100, Math.floor(snapshot.downloadedBytes / snapshot.totalBytes * 100))
  return `Downloading: ${percent}% (${formatMb(snapshot.downloadedBytes)} / ${formatMb(snapshot.totalBytes)} MB)`
}

function formatMb(bytes: number): string {
  return (bytes / 1_000_000).toFixed(1)
}

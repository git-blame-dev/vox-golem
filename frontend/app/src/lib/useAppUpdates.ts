import { useCallback, useEffect, useRef, useState } from 'react'
import {
  checkForUpdate,
  discardUpdate,
  downloadUpdate,
  getUpdateSnapshot,
  installUpdate,
  parseUpdateSnapshot,
  restartForUpdate,
  retryOperation,
  selectFreshSnapshot,
  setAutoUpdateDownload,
} from './appUpdates'
import type { UpdateSnapshot } from './appUpdates'
import { getTauriInternals } from './tauri'

export type UpdateState =
  | { readonly kind: 'loading' }
  | { readonly kind: 'snapshot'; readonly snapshot: UpdateSnapshot }
  | { readonly kind: 'browser' }

export interface AppUpdateController {
  readonly state: UpdateState
  readonly check: () => Promise<void>
  readonly download: () => Promise<void>
  readonly install: () => Promise<void>
  readonly later: () => Promise<void>
  readonly restart: () => Promise<void>
  readonly actionPending: boolean
  readonly actionError: string | null
  readonly autoDownloadSaving: boolean
  readonly autoDownloadError: string | null
  readonly setAutoDownloadEnabled: (enabled: boolean) => Promise<void>
  readonly retryAutoDownloadSave: () => Promise<void>
}

export function useAppUpdates(): AppUpdateController {
  const [state, setState] = useState<UpdateState>(() =>
    getTauriInternals() === null ? { kind: 'browser' } : { kind: 'loading' },
  )
  const snapshotRef = useRef<UpdateSnapshot | null>(null)
  const operationInFlight = useRef(false)
  const [actionPending, setActionPending] = useState(false)
  const [actionError, setActionError] = useState<string | null>(null)
  const [autoDownloadSaving, setAutoDownloadSaving] = useState(false)
  const [autoDownloadError, setAutoDownloadError] = useState<string | null>(null)
  const desiredAutoDownload = useRef(true)
  const preferenceWriteInFlight = useRef(false)

  const acceptSnapshot = useCallback((incoming: UpdateSnapshot): void => {
    const selected = selectFreshSnapshot(snapshotRef.current, incoming)
    if (selected !== snapshotRef.current) {
      if (snapshotRef.current === null) desiredAutoDownload.current = selected.autoDownloadEnabled
      snapshotRef.current = selected
      setState({ kind: 'snapshot', snapshot: selected })
      setActionError(null)
    }
  }, [])

  useEffect(() => {
    const tauri = getTauriInternals()
    if (tauri === null) return
    let active = true
    let unlisten: (() => void) | undefined
    const hydrate = async (): Promise<void> => {
      if (tauri.listen) {
        try {
          const dispose = await tauri.listen('app-update-state', (event) => {
            if (!active) return
            try { acceptSnapshot(parseUpdateSnapshot(event.payload)) } catch { /* Ignore malformed native events. */ }
          })
          if (!active) {
            dispose()
            return
          }
          unlisten = dispose
        } catch {
          // Hydration still provides a useful read-only state if events are unavailable.
        }
      }
      try {
        const initial = await getUpdateSnapshot()
        if (active) acceptSnapshot(initial)
      } catch (error) {
        if (active) setActionError(displayError(error))
      }
    }
    void hydrate()
    return () => {
      active = false
      unlisten?.()
    }
  }, [acceptSnapshot])

  const runSnapshotCommand = useCallback(async (command: () => Promise<UpdateSnapshot>): Promise<void> => {
    if (operationInFlight.current) return
    operationInFlight.current = true
    setActionPending(true)
    setActionError(null)
    try {
      acceptSnapshot(await command())
    } catch (error) {
      setActionError(displayError(error))
    } finally {
      operationInFlight.current = false
      setActionPending(false)
    }
  }, [acceptSnapshot])

  const check = useCallback(() => runSnapshotCommand(checkForUpdate), [runSnapshotCommand])
  const download = useCallback(() => runSnapshotCommand(downloadUpdate), [runSnapshotCommand])
  const install = useCallback(() => runSnapshotCommand(installUpdate), [runSnapshotCommand])
  const later = useCallback(() => runSnapshotCommand(discardUpdate), [runSnapshotCommand])

  const restart = useCallback(async (): Promise<void> => {
    if (operationInFlight.current) return
    operationInFlight.current = true
    setActionPending(true)
    setActionError(null)
    try {
      await restartForUpdate()
    } catch (error) {
      setActionError(`Restart failed: ${displayError(error)}`)
    } finally {
      operationInFlight.current = false
      setActionPending(false)
    }
  }, [])

  const setAutoDownloadEnabled = useCallback(async (enabled: boolean): Promise<void> => {
    if (preferenceWriteInFlight.current) return
    preferenceWriteInFlight.current = true
    desiredAutoDownload.current = enabled
    setAutoDownloadSaving(true)
    setAutoDownloadError(null)
    try {
      acceptSnapshot(await setAutoUpdateDownload(enabled))
    } catch (error) {
      setAutoDownloadError(`Automatic update preference was not saved: ${displayError(error)}`)
    } finally {
      preferenceWriteInFlight.current = false
      setAutoDownloadSaving(false)
    }
  }, [acceptSnapshot])

  const retryAutoDownloadSave = useCallback(
    () => setAutoDownloadEnabled(desiredAutoDownload.current),
    [setAutoDownloadEnabled],
  )

  return {
    state,
    check,
    download,
    install,
    later,
    restart,
    actionPending,
    actionError,
    autoDownloadSaving,
    autoDownloadError,
    setAutoDownloadEnabled,
    retryAutoDownloadSave,
  }
}

export function retryCurrentOperation(updates: AppUpdateController): Promise<void> {
  if (updates.state.kind !== 'snapshot') return updates.check()
  switch (retryOperation(updates.state.snapshot)) {
    case 'download': return updates.download()
    case 'install': return updates.install()
    case 'check': return updates.check()
  }
}

function displayError(error: unknown): string {
  if (error instanceof Error) return error.message
  if (typeof error === 'string') return error
  return String(error)
}

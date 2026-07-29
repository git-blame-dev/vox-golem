import { useCallback, useEffect, useRef, useState } from 'react'
import { checkForUpdate, installUpdate, restartForUpdate } from './appUpdates'
import type { UpdateCheckResult } from './appUpdates'
import { getTauriInternals, isAndroidRuntime } from './tauri'

export type UpdateState =
  | { readonly kind: 'checking' }
  | { readonly kind: 'result'; readonly result: UpdateCheckResult }
  | { readonly kind: 'installing'; readonly version: string }
  | { readonly kind: 'installed'; readonly version: string }
  | {
      readonly kind: 'error'
      readonly operation: 'check' | 'install' | 'restart'
      readonly message: string
      readonly version?: string
    }
  | { readonly kind: 'browser' }
  | { readonly kind: 'unsupported'; readonly reason: string }

export interface AppUpdateController {
  readonly state: UpdateState
  readonly check: () => Promise<void>
  readonly install: (version: string) => Promise<void>
  readonly restart: () => Promise<void>
}

export function useAppUpdates(): AppUpdateController {
  const [state, setState] = useState<UpdateState>(() => initialUpdateState(getTauriInternals() !== null, isAndroidRuntime))
  const requestRevision = useRef(0)

  const loadUpdate = useCallback(async (revision: number): Promise<void> => {
    try {
      const result = await checkForUpdate()
      if (revision === requestRevision.current) setState({ kind: 'result', result })
    } catch (error) {
      if (revision === requestRevision.current) {
        setState({ kind: 'error', operation: 'check', message: displayError(error) })
      }
    }
  }, [])

  const check = useCallback(async (): Promise<void> => {
    if (isAndroidRuntime) { setState({ kind: 'unsupported', reason: 'Desktop updates are not available on Android' }); return }
    const revision = ++requestRevision.current
    setState({ kind: 'checking' })
    await loadUpdate(revision)
  }, [loadUpdate])

  useEffect(() => {
    if (getTauriInternals() === null || isAndroidRuntime) return
    const revision = ++requestRevision.current
    void loadUpdate(revision)
    return () => { requestRevision.current += 1 }
  }, [loadUpdate])

  const install = useCallback(async (version: string): Promise<void> => {
    if (isAndroidRuntime) { setState({ kind: 'unsupported', reason: 'Desktop updates are not available on Android' }); return }
    const revision = ++requestRevision.current
    setState({ kind: 'installing', version })
    try {
      const installedVersion = await installUpdate()
      if (revision === requestRevision.current) setState({ kind: 'installed', version: installedVersion })
    } catch (error) {
      if (revision === requestRevision.current) {
        setState({ kind: 'error', operation: 'install', version, message: displayError(error) })
      }
    }
  }, [])

  const restart = useCallback(async (): Promise<void> => {
    if (isAndroidRuntime) { setState({ kind: 'unsupported', reason: 'Desktop updates are not available on Android' }); return }
    const revision = ++requestRevision.current
    try {
      await restartForUpdate()
    } catch (error) {
      if (revision === requestRevision.current) {
        setState({ kind: 'error', operation: 'restart', message: displayError(error) })
      }
    }
  }, [])

  return { state, check, install, restart }
}

export function initialUpdateState(native: boolean, android: boolean): UpdateState {
  if (android) return { kind: 'unsupported', reason: 'Desktop updates are not available on Android' }
  return native ? { kind: 'checking' } : { kind: 'browser' }
}

function displayError(error: unknown): string {
  const message = error instanceof Error ? error.message : typeof error === 'string' ? error : String(error)
  return Array.from(message).slice(0, 2_048).join('')
}

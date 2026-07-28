import { useCallback, useEffect, useRef, useState } from 'react'
import { checkForUpdate, installUpdate, restartForUpdate } from './appUpdates'
import type { UpdateCheckResult } from './appUpdates'
import { getTauriInternals } from './tauri'

export type UpdateState =
  | { readonly kind: 'checking' }
  | { readonly kind: 'result'; readonly result: UpdateCheckResult }
  | { readonly kind: 'installing'; readonly version: string }
  | { readonly kind: 'installed'; readonly version: string }
  | { readonly kind: 'error'; readonly message: string }
  | { readonly kind: 'browser' }

export interface AppUpdateController {
  readonly state: UpdateState
  readonly check: () => Promise<void>
  readonly install: (version: string) => Promise<void>
  readonly restart: () => Promise<void>
}

export function useAppUpdates(): AppUpdateController {
  const [state, setState] = useState<UpdateState>(() =>
    getTauriInternals() === null ? { kind: 'browser' } : { kind: 'checking' },
  )
  const requestRevision = useRef(0)

  const loadUpdate = useCallback(async (revision: number): Promise<void> => {
    try {
      const result = await checkForUpdate()
      if (revision === requestRevision.current) setState({ kind: 'result', result })
    } catch (error) {
      if (revision === requestRevision.current) {
        setState({ kind: 'error', message: displayError(error) })
      }
    }
  }, [])

  const check = useCallback(async (): Promise<void> => {
    const revision = ++requestRevision.current
    setState({ kind: 'checking' })
    await loadUpdate(revision)
  }, [loadUpdate])

  useEffect(() => {
    if (getTauriInternals() === null) return
    const revision = ++requestRevision.current
    void loadUpdate(revision)
    return () => { requestRevision.current += 1 }
  }, [loadUpdate])

  const install = useCallback(async (version: string): Promise<void> => {
    const revision = ++requestRevision.current
    setState({ kind: 'installing', version })
    try {
      const installedVersion = await installUpdate()
      if (revision === requestRevision.current) setState({ kind: 'installed', version: installedVersion })
    } catch (error) {
      if (revision === requestRevision.current) {
        setState({ kind: 'error', message: displayError(error) })
      }
    }
  }, [])

  const restart = useCallback(async (): Promise<void> => {
    try {
      await restartForUpdate()
    } catch (error) {
      setState({ kind: 'error', message: displayError(error) })
    }
  }, [])

  return { state, check, install, restart }
}

function displayError(error: unknown): string {
  if (error instanceof Error) return error.message
  if (typeof error === 'string') return error
  return String(error)
}

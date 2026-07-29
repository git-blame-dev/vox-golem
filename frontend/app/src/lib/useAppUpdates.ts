import { useCallback, useEffect, useRef, useState } from 'react'
import { checkForUpdate, installUpdate, parseUpdateProgress, restartForUpdate } from './appUpdates'
import type { InstallBehavior, UpdateCheckResult, UpdateProgress } from './appUpdates'
import { getTauriInternals } from './tauri'

export type UpdateState =
  | { readonly kind: 'checking' }
  | { readonly kind: 'result'; readonly result: UpdateCheckResult }
  | { readonly kind: 'installing'; readonly version: string; readonly progress?: UpdateProgress }
  | { readonly kind: 'installed'; readonly version: string; readonly installBehavior: 'install_then_restart' | 'install_and_restart' }
  | { readonly kind: 'error'; readonly message: string }
  | { readonly kind: 'browser' }

export interface AppUpdateController {
  readonly state: UpdateState
  readonly check: () => Promise<void>
  readonly install: (version: string, installBehavior?: InstallBehavior) => Promise<void>
  readonly restart: () => Promise<void>
  readonly progress: UpdateProgress | undefined
}

export function useAppUpdates(): AppUpdateController {
  const [state, setState] = useState<UpdateState>(() =>
    getTauriInternals() === null ? { kind: 'browser' } : { kind: 'checking' },
  )
  const requestRevision = useRef(0)
  const activeVersion = useRef<string | undefined>(undefined)
  const [progress, setProgress] = useState<UpdateProgress | undefined>()

  const loadUpdate = useCallback(async (revision: number): Promise<void> => {
    try {
      const result = await checkForUpdate()
       if (revision === requestRevision.current) {
         activeVersion.current = stateVersion({ kind: 'result', result })
         setState({ kind: 'result', result })
       }
    } catch (error) {
      if (revision === requestRevision.current) {
        setState({ kind: 'error', message: displayError(error) })
      }
    }
  }, [])

  const check = useCallback(async (): Promise<void> => {
    const revision = ++requestRevision.current
    setState({ kind: 'checking' })
    setProgress(undefined)
    await loadUpdate(revision)
  }, [loadUpdate])

  useEffect(() => {
    if (getTauriInternals() === null) return
    const revision = ++requestRevision.current
    void loadUpdate(revision)
    return () => { requestRevision.current += 1 }
  }, [loadUpdate])

  const install = useCallback(async (version: string, installBehavior: InstallBehavior = 'install_then_restart'): Promise<void> => {
    const revision = ++requestRevision.current
    setState({ kind: 'installing', version })
    activeVersion.current = version
    setProgress(undefined)
    try {
      const installedVersion = await installUpdate()
      if (revision === requestRevision.current) {
        setProgress(undefined)
        setState({ kind: 'installed', version: installedVersion, installBehavior })
      }
    } catch (error) {
      if (revision === requestRevision.current) {
        setState({ kind: 'error', message: displayError(error) })
        setProgress(undefined)
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

  useEffect(() => {
    const tauri = getTauriInternals()
    if (!tauri?.listen) return
    let active = true
    let unlisten: (() => void) | undefined
    void tauri.listen('app-update-progress', (event) => {
      if (!active) return
      try {
        const next = parseUpdateProgress(event.payload)
        setProgress((current) => activeVersion.current === next.version ? next : current)
      } catch { /* Ignore malformed native events. */ }
    }).then((dispose) => { if (active) unlisten = dispose; else dispose() })
    return () => { active = false; unlisten?.() }
  }, [])

  const displayedState: UpdateState = state.kind === 'installing' && progress
    ? { ...state, progress }
    : state
  return { state: displayedState, check, install, restart, progress }
}

function stateVersion(state: UpdateState): string | undefined {
  if (state.kind === 'installing' || state.kind === 'installed') return state.version
  if (state.kind === 'result' && (state.result.status === 'available' || state.result.status === 'installing' || state.result.status === 'installed')) return state.result.version
  return undefined
}

function displayError(error: unknown): string {
  if (error instanceof Error) return error.message
  if (typeof error === 'string') return error
  return String(error)
}

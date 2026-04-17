export interface TauriInternals {
  readonly invoke: (command: string, args?: unknown) => Promise<unknown>
}

declare global {
  interface Window {
    __TAURI_INTERNALS__?: TauriInternals
  }
}

export function getTauriInternals(): TauriInternals | null {
  if (typeof window === 'undefined') {
    return null
  }

  const tauriInternals = window.__TAURI_INTERNALS__

  if (!tauriInternals || typeof tauriInternals.invoke !== 'function') {
    return null
  }

  return tauriInternals
}

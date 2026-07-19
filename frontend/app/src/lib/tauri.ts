import {
  convertFileSrc as tauriConvertFileSrc,
  invoke as tauriInvoke,
  isTauri,
} from '@tauri-apps/api/core'
import { listen as tauriListen } from '@tauri-apps/api/event'

export interface TauriEvent {
  readonly payload: unknown
}

export interface TauriInternals {
  readonly invoke: (command: string, args?: unknown) => Promise<unknown>
  readonly convertFileSrc?: (filePath: string, protocol?: string) => string
  readonly listen?: (
    event: string,
    handler: (event: TauriEvent) => void,
  ) => Promise<() => void>
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
  const nativeTauriInternals = tauriInternals as
    | (TauriInternals & { readonly transformCallback?: unknown })
    | undefined

  if (typeof nativeTauriInternals?.transformCallback === 'function') {
    return createTauriApi()
  }

  if (
    tauriInternals &&
    typeof tauriInternals.invoke === 'function'
  ) {
    return tauriInternals
  }

  if (!isTauri()) {
    return null
  }

  return createTauriApi()
}

function createTauriApi(): TauriInternals {
  return {
    invoke: (command, args) => tauriInvoke(command, args as Parameters<typeof tauriInvoke>[1]),
    convertFileSrc: tauriConvertFileSrc,
    listen: (event, handler) => tauriListen(event, handler),
  }
}

export function hasInjectedTauriInternals(): boolean {
  return typeof window !== 'undefined' && typeof window.__TAURI_INTERNALS__?.invoke === 'function'
}

export async function invokeTauriCommand(command: string, args?: unknown): Promise<unknown> {
  if (hasInjectedTauriInternals()) {
    return window.__TAURI_INTERNALS__!.invoke(command, args)
  }

  return tauriInvoke(command, args as Parameters<typeof tauriInvoke>[1])
}

export function convertTauriFileSrc(filePath: string): string | null {
  const tauriInternals = getTauriInternals()

  if (tauriInternals?.convertFileSrc) {
    return tauriInternals.convertFileSrc(filePath)
  }

  try {
    return tauriConvertFileSrc(filePath)
  } catch {
    return null
  }
}

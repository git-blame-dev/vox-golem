import type { StartupState } from '../types/chat'

interface TauriInternals {
  readonly invoke: (command: string, args?: unknown) => Promise<unknown>
}

declare global {
  interface Window {
    __TAURI_INTERNALS__?: TauriInternals
  }
}

export function parseStartupState(payload: unknown): StartupState {
  if (!isRecord(payload)) {
    throw new Error('Startup payload must be an object')
  }

  if (payload['kind'] === 'ready') {
    return { kind: 'ready' }
  }

  if (payload['kind'] === 'error') {
    const message = payload['message']

    if (typeof message === 'string' && message.length > 0) {
      return {
        kind: 'error',
        message,
      }
    }

    throw new Error('Startup error payload must include a message')
  }

  throw new Error('Startup payload contains an unsupported kind')
}

export async function loadStartupState(): Promise<StartupState> {
  if (typeof window === 'undefined') {
    return { kind: 'ready' }
  }

  const tauriInternals = window.__TAURI_INTERNALS__

  if (!tauriInternals || typeof tauriInternals.invoke !== 'function') {
    return { kind: 'ready' }
  }

  try {
    const payload = await tauriInternals.invoke('get_startup_state')
    return parseStartupState(payload)
  } catch (error) {
    const message = error instanceof Error ? error.message : 'Failed to load startup state'
    return {
      kind: 'error',
      message,
    }
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null
}

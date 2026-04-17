import type { CueAssetPaths, StartupState } from '../types/chat'

export const DEFAULT_CUE_ASSET_PATHS: CueAssetPaths = {
  startListening: 'assets/start-listening.mp3',
  stopListening: 'assets/stop-listening.mp3',
}

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
    return {
      kind: 'ready',
      cueAssetPaths: parseCueAssetPaths(payload['cue_asset_paths']),
    }
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
    return {
      kind: 'ready',
      cueAssetPaths: DEFAULT_CUE_ASSET_PATHS,
    }
  }

  const tauriInternals = window.__TAURI_INTERNALS__

  if (!tauriInternals || typeof tauriInternals.invoke !== 'function') {
    return {
      kind: 'ready',
      cueAssetPaths: DEFAULT_CUE_ASSET_PATHS,
    }
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

function parseCueAssetPaths(payload: unknown): CueAssetPaths {
  if (!isRecord(payload)) {
    throw new Error('Startup ready payload must include cue asset paths')
  }

  const startListening = payload['start_listening']
  const stopListening = payload['stop_listening']

  if (typeof startListening !== 'string' || startListening.length === 0) {
    throw new Error('Startup ready payload must include `start_listening` cue path')
  }

  if (typeof stopListening !== 'string' || stopListening.length === 0) {
    throw new Error('Startup ready payload must include `stop_listening` cue path')
  }

  return {
    startListening,
    stopListening,
  }
}

import { afterEach, describe, expect, it } from 'vitest'
import { DEFAULT_CUE_ASSET_PATHS, loadStartupState, parseStartupState } from './startupState'

afterEach(() => {
  Reflect.deleteProperty(window, '__TAURI_INTERNALS__')
})

describe('parseStartupState', () => {
  it('returns ready state with configured cue paths for ready payload', () => {
    expect(
      parseStartupState({
        kind: 'ready',
        cue_asset_paths: {
          start_listening: 'assets/start-listening.mp3',
          stop_listening: 'assets/stop-listening.mp3',
        },
      }),
    ).toEqual({
      kind: 'ready',
      cueAssetPaths: DEFAULT_CUE_ASSET_PATHS,
    })
  })

  it('returns error state for valid error payload', () => {
    expect(
      parseStartupState({
        kind: 'error',
        message: 'config file not found',
      }),
    ).toEqual({
      kind: 'error',
      message: 'config file not found',
    })
  })

  it('throws when ready payload omits cue paths', () => {
    expect(() => parseStartupState({ kind: 'ready' })).toThrow(
      'Startup ready payload must include cue asset paths',
    )
  })

  it('throws for unsupported payloads', () => {
    expect(() => parseStartupState({ kind: 'loading' })).toThrow()
  })
})

describe('loadStartupState', () => {
  it('falls back to default cue assets when tauri internals are unavailable', async () => {
    await expect(loadStartupState()).resolves.toEqual({
      kind: 'ready',
      cueAssetPaths: DEFAULT_CUE_ASSET_PATHS,
    })
  })

  it('loads configured cue paths from tauri startup payload', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async () => ({
        kind: 'ready',
        cue_asset_paths: {
          start_listening: 'configured/start.mp3',
          stop_listening: 'configured/stop.mp3',
        },
      }),
    }

    await expect(loadStartupState()).resolves.toEqual({
      kind: 'ready',
      cueAssetPaths: {
        startListening: 'configured/start.mp3',
        stopListening: 'configured/stop.mp3',
      },
    })
  })

  it('surfaces invoke failures as startup errors', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async () => {
        throw new Error('startup command failed')
      },
    }

    await expect(loadStartupState()).resolves.toEqual({
      kind: 'error',
      message: 'startup command failed',
    })
  })
})

import { afterEach, describe, expect, it } from 'vitest'
import { DEFAULT_CUE_ASSET_PATHS, isStartupStateSettled, loadStartupState, parseStartupState } from './startupState'

afterEach(() => {
  Reflect.deleteProperty(window, '__TAURI_INTERNALS__')
})

describe('parseStartupState', () => {
  it('returns ready state with configured cue paths for ready payload', () => {
    expect(
      parseStartupState({
        kind: 'ready',
        cue_asset_paths: {
          start_listening: 'resources/start-listening.wav',
          stop_listening: 'resources/stop-listening.wav',
        },
        runtime_phase: 'sleeping',
        voice_input_available: true,
        voice_input_error: null,
        silence_timeout_ms: 1500,
      }),
    ).toEqual({
      kind: 'ready',
      cueAssetPaths: {
        startListening: 'resources/start-listening.wav',
        stopListening: 'resources/stop-listening.wav',
      },
      runtimePhase: 'sleeping',
      voiceInputAvailable: true,
      voiceInputError: null,
      silenceTimeoutMs: 1500,
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

  it('returns warming state for valid warming payload', () => {
    expect(
      parseStartupState({
        kind: 'warming_model',
        cue_asset_paths: {
          start_listening: 'resources/start-listening.wav',
          stop_listening: 'resources/stop-listening.wav',
        },
        runtime_phase: 'initializing',
        voice_input_available: true,
        voice_input_error: null,
        silence_timeout_ms: 1500,
        message: 'Loading local Gemma model...',
      }),
    ).toEqual({
      kind: 'warming_model',
      cueAssetPaths: {
        startListening: 'resources/start-listening.wav',
        stopListening: 'resources/stop-listening.wav',
      },
      runtimePhase: 'initializing',
      voiceInputAvailable: true,
      voiceInputError: null,
      silenceTimeoutMs: 1500,
      message: 'Loading local Gemma model...',
    })
  })

  it('throws when ready payload omits cue paths', () => {
    expect(() => parseStartupState({ kind: 'ready', silence_timeout_ms: 1500 })).toThrow(
      'Startup ready payload must include voice_input_available',
    )
  })

  it('throws when startup payload omits silence timeout', () => {
    expect(() =>
      parseStartupState({
        kind: 'ready',
        cue_asset_paths: {
          start_listening: 'resources/start-listening.wav',
          stop_listening: 'resources/stop-listening.wav',
        },
        runtime_phase: 'sleeping',
        voice_input_available: true,
        voice_input_error: null,
      }),
    ).toThrow('Startup payload must include a positive integer `silence_timeout_ms`')
  })

  it('throws when startup payload includes non-safe silence timeout value', () => {
    expect(() =>
      parseStartupState({
        kind: 'ready',
        cue_asset_paths: {
          start_listening: 'resources/start-listening.wav',
          stop_listening: 'resources/stop-listening.wav',
        },
        runtime_phase: 'sleeping',
        voice_input_available: true,
        voice_input_error: null,
        silence_timeout_ms: 9_007_199_254_740_992,
      }),
    ).toThrow('Startup payload must include a positive integer `silence_timeout_ms`')
  })

  it('throws for unsupported payloads', () => {
    expect(() => parseStartupState({ kind: 'loading' })).toThrow()
  })
})

describe('isStartupStateSettled', () => {
  it('returns false while the model is warming', () => {
    expect(
      isStartupStateSettled({
        kind: 'warming_model',
        cueAssetPaths: DEFAULT_CUE_ASSET_PATHS,
        runtimePhase: 'initializing',
        voiceInputAvailable: true,
        voiceInputError: null,
        silenceTimeoutMs: 1500,
        message: 'Loading local Gemma model...',
      }),
    ).toBe(false)
  })

  it('returns true for ready and error states', () => {
    expect(
      isStartupStateSettled({
        kind: 'ready',
        cueAssetPaths: DEFAULT_CUE_ASSET_PATHS,
        runtimePhase: 'sleeping',
        voiceInputAvailable: true,
        voiceInputError: null,
        silenceTimeoutMs: 1500,
      }),
    ).toBe(true)
    expect(
      isStartupStateSettled({
        kind: 'error',
        message: 'startup failed',
      }),
    ).toBe(true)
  })
})

describe('loadStartupState', () => {
  it('falls back to default cue assets when tauri internals are unavailable', async () => {
    await expect(loadStartupState()).resolves.toEqual({
      kind: 'ready',
      cueAssetPaths: DEFAULT_CUE_ASSET_PATHS,
      runtimePhase: 'sleeping',
      voiceInputAvailable: true,
      voiceInputError: null,
      silenceTimeoutMs: 1500,
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
        runtime_phase: 'sleeping',
        voice_input_available: false,
        voice_input_error: 'Parakeet failed to initialize',
        silence_timeout_ms: 2300,
      }),
    }

    await expect(loadStartupState()).resolves.toEqual({
      kind: 'ready',
      cueAssetPaths: {
        startListening: 'configured/start.mp3',
        stopListening: 'configured/stop.mp3',
      },
      runtimePhase: 'sleeping',
      voiceInputAvailable: false,
      voiceInputError: 'Parakeet failed to initialize',
      silenceTimeoutMs: 2300,
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

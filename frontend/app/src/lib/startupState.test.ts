import { afterEach, describe, expect, it } from 'vitest'
import { DEFAULT_CUE_ASSET_PATHS, isStartupStateSettled, loadStartupState, parseStartupState } from './startupState'

const COMPLETE_CAPABILITIES = [
  'custom_provider', 'opencode', 'local_fast', 'local_quality', 'qwen_prediction',
  'wake_word', 'vad', 'parakeet', 'tts', 'deep', 'review',
].map((id) => ({ id, state: 'not_configured', reason: `${id} is not configured`, actual_provider: null }))

afterEach(() => {
  Reflect.deleteProperty(window, '__TAURI_INTERNALS__')
})

describe('parseStartupState', () => {
  it('strictly parses every zero-asset capability without making startup fatal', () => {
    const capabilities = [
      'custom_provider', 'opencode', 'local_fast', 'local_quality', 'qwen_prediction',
      'wake_word', 'vad', 'parakeet', 'tts', 'deep', 'review',
    ].map((id) => ({ id, state: 'not_configured', reason: `${id} is not configured`, actual_provider: null }))

    const state = parseStartupState({
      kind: 'ready',
      cue_asset_paths: { start_listening: 'start.wav', stop_listening: 'stop.wav' },
      runtime_phase: 'sleeping',
      voice_input_available: false,
      voice_input_error: 'voice assets are not configured',
      silence_timeout_ms: 1500,
      selected_response_profile: 'fast',
      supported_response_profiles: [],
      capabilities,
    })

    expect(state.kind).toBe('ready')
    if (state.kind === 'ready') {
      expect(state.capabilities.map(({ id }) => id)).toEqual(capabilities.map(({ id }) => id))
      expect(state.supportedResponseProfiles).toEqual([])
    }
  })

  it('rejects an incomplete native capability contract', () => {
    expect(() => parseStartupState({
      kind: 'ready',
      cue_asset_paths: { start_listening: 'start.wav', stop_listening: 'stop.wav' },
      runtime_phase: 'sleeping',
      voice_input_available: false,
      voice_input_error: null,
      silence_timeout_ms: 1500,
      selected_response_profile: 'fast',
      supported_response_profiles: [],
      capabilities: [{ id: 'opencode', state: 'failed', reason: 'failed', actual_provider: null }],
    })).toThrow('Startup payload is missing capabilities')
  })

  it('rejects a native payload that omits capabilities', () => {
    expect(() => parseStartupState({
      kind: 'ready',
      cue_asset_paths: { start_listening: 'start.wav', stop_listening: 'stop.wav' },
      runtime_phase: 'sleeping',
      voice_input_available: false,
      voice_input_error: null,
      silence_timeout_ms: 1500,
      selected_response_profile: 'fast',
      supported_response_profiles: [],
    })).toThrow('Startup payload must include capabilities')
  })

  it('returns ready state with configured cue paths for ready payload', () => {
    expect(parseStartupState({
        kind: 'ready',
        cue_asset_paths: {
          start_listening: 'resources/start-listening.wav',
          stop_listening: 'resources/stop-listening.wav',
        },
        runtime_phase: 'sleeping',
        voice_input_available: true,
        voice_input_error: null,
        silence_timeout_ms: 1500,
        selected_response_profile: 'fast',
         supported_response_profiles: ['fast', 'quality'],
         capabilities: COMPLETE_CAPABILITIES,
      }),
    ).toEqual(expect.objectContaining({
      kind: 'ready',
      cueAssetPaths: {
        startListening: 'resources/start-listening.wav',
        stopListening: 'resources/stop-listening.wav',
      },
      runtimePhase: 'sleeping',
      voiceInputAvailable: true,
      voiceInputError: null,
      silenceTimeoutMs: 1500,
      selectedResponseProfile: 'fast',
      supportedResponseProfiles: ['fast', 'quality'],
      promptCancellationAvailable: false,
      ttsEnabled: false,
      ttsOutputGainDb: 3,
    }))
  })

  it('returns error state for valid error payload', () => {
    expect(
      parseStartupState({
        kind: 'error',
        message: 'config file not found',
      }),
    ).toEqual(expect.objectContaining({
      kind: 'error',
      message: 'config file not found',
    }))
  })

  it('parses explicit tts_output_gain_db from ready payload', () => {
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
        selected_response_profile: 'fast',
         supported_response_profiles: ['fast', 'quality'],
         capabilities: COMPLETE_CAPABILITIES,
        tts_output_gain_db: 6,
      }),
    ).toEqual(expect.objectContaining({
      kind: 'ready',
      cueAssetPaths: {
        startListening: 'resources/start-listening.wav',
        stopListening: 'resources/stop-listening.wav',
      },
      runtimePhase: 'sleeping',
      voiceInputAvailable: true,
      voiceInputError: null,
      silenceTimeoutMs: 1500,
      selectedResponseProfile: 'fast',
      supportedResponseProfiles: ['fast', 'quality'],
      promptCancellationAvailable: false,
      ttsEnabled: false,
      ttsOutputGainDb: 6,
    }))
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
        selected_response_profile: 'quality',
         supported_response_profiles: ['fast', 'quality'],
         capabilities: COMPLETE_CAPABILITIES,
      }),
    ).toEqual(expect.objectContaining({
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
      selectedResponseProfile: 'quality',
      supportedResponseProfiles: ['fast', 'quality'],
      promptCancellationAvailable: false,
      ttsEnabled: false,
      ttsOutputGainDb: 3,
    }))
  })

  it('throws when startup payload omits selected response profile', () => {
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
        silence_timeout_ms: 1500,
        supported_response_profiles: ['fast', 'quality'],
      }),
    ).toThrow('Startup payload must include selected_response_profile')
  })

  it('throws when startup payload omits supported response profiles', () => {
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
        silence_timeout_ms: 1500,
        selected_response_profile: 'fast',
      }),
    ).toThrow('Startup payload must include supported_response_profiles')
  })

  it('throws when selected response profile is not listed as supported', () => {
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
        silence_timeout_ms: 1500,
        selected_response_profile: 'quality',
        supported_response_profiles: ['fast'],
      }),
    ).toThrow('Selected response profile must be present in supported_response_profiles')
  })

  it('allows empty response profiles when no provider is available', () => {
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
         selected_response_profile: 'fast',
         supported_response_profiles: [],
         capabilities: COMPLETE_CAPABILITIES,
       })).toEqual(expect.objectContaining({ supportedResponseProfiles: [] }))
  })

  it('throws when startup payload includes unsupported response profile tokens', () => {
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
        silence_timeout_ms: 1500,
        selected_response_profile: 'turbo',
        supported_response_profiles: ['fast', 'turbo'],
      }),
    ).toThrow('Startup payload must include a supported selected_response_profile')
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
        selectedResponseProfile: 'quality',
        supportedResponseProfiles: ['fast', 'quality'],
        promptCancellationAvailable: false,
        ttsEnabled: false,
        ttsOutputGainDb: 3,
        capabilities: [],
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
        selectedResponseProfile: 'quality',
        supportedResponseProfiles: ['fast', 'quality'],
        promptCancellationAvailable: false,
        ttsEnabled: false,
        ttsOutputGainDb: 3,
        capabilities: [],
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
    await expect(loadStartupState()).resolves.toEqual(expect.objectContaining({
      kind: 'ready',
      cueAssetPaths: DEFAULT_CUE_ASSET_PATHS,
      runtimePhase: 'sleeping',
      voiceInputAvailable: true,
      voiceInputError: null,
      silenceTimeoutMs: 1500,
      selectedResponseProfile: 'fast',
      supportedResponseProfiles: ['fast'],
      promptCancellationAvailable: false,
      ttsEnabled: false,
      ttsOutputGainDb: 3,
    }))
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
         selected_response_profile: 'fast',
         supported_response_profiles: ['fast', 'quality'],
         capabilities: COMPLETE_CAPABILITIES,
       }),
    }

    await expect(loadStartupState()).resolves.toEqual(expect.objectContaining({
      kind: 'ready',
      cueAssetPaths: {
        startListening: 'configured/start.mp3',
        stopListening: 'configured/stop.mp3',
      },
      runtimePhase: 'sleeping',
      voiceInputAvailable: false,
      voiceInputError: 'Parakeet failed to initialize',
      silenceTimeoutMs: 2300,
      selectedResponseProfile: 'fast',
      supportedResponseProfiles: ['fast', 'quality'],
      promptCancellationAvailable: false,
      ttsEnabled: false,
      ttsOutputGainDb: 3,
    }))
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

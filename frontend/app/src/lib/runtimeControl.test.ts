import { afterEach, describe, expect, it } from 'vitest'
import { ingestAudioFrame, invokeRuntimeControl } from './runtimeControl'

afterEach(() => {
  Reflect.deleteProperty(window, '__TAURI_INTERNALS__')
})

describe('invokeRuntimeControl', () => {
  it('returns null when tauri internals are unavailable', async () => {
    await expect(invokeRuntimeControl('begin_listening')).resolves.toBeNull()
  })

  it('parses runtime phase payloads from tauri commands', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        expect(command).toBe('begin_listening')

        return {
          runtime_phase: 'listening',
          transcription_ready_samples: null,
          last_activity_ms: 100,
          capturing_utterance: true,
          preroll_samples: 3,
          utterance_samples: 5,
        }
      },
    }

    await expect(invokeRuntimeControl('begin_listening')).resolves.toEqual({
      runtimePhase: 'listening',
      transcriptionReadySamples: null,
      lastActivityMs: 100,
      capturingUtterance: true,
      prerollSamples: 3,
      utteranceSamples: 5,
    })
  })

  it('parses speech activity payloads from tauri commands', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        expect(command).toBe('record_speech_activity')
        expect(args).toEqual({ nowMs: 101 })

        return {
          runtime_phase: 'listening',
          transcription_ready_samples: null,
          last_activity_ms: 101,
          capturing_utterance: true,
          preroll_samples: 4,
          utterance_samples: 4,
        }
      },
    }

    await expect(invokeRuntimeControl('record_speech_activity', { nowMs: 101 })).resolves.toEqual({
      runtimePhase: 'listening',
      transcriptionReadySamples: null,
      lastActivityMs: 101,
      capturingUtterance: true,
      prerollSamples: 4,
      utteranceSamples: 4,
    })
  })

  it('parses audio frame status payloads from tauri commands', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        expect(command).toBe('ingest_audio_frame')
        expect(args).toEqual({ frame: [0.1, 0.2, 0.3] })

        return {
          runtime_phase: 'sleeping',
          capturing_utterance: false,
          preroll_samples: 3,
          utterance_samples: 0,
        }
      },
    }

    await expect(ingestAudioFrame([0.1, 0.2, 0.3])).resolves.toEqual({
      runtimePhase: 'sleeping',
      capturingUtterance: false,
      prerollSamples: 3,
      utteranceSamples: 0,
    })
  })

  it('rejects runtime control payloads missing capture fields', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async () => ({
        runtime_phase: 'listening',
        transcription_ready_samples: null,
        last_activity_ms: 100,
      }),
    }

    await expect(invokeRuntimeControl('begin_listening')).rejects.toThrow(
      'Runtime control payload must include capturing_utterance',
    )
  })

  it('rejects runtime control payloads missing last activity', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async () => ({
        runtime_phase: 'listening',
        transcription_ready_samples: null,
        capturing_utterance: true,
        preroll_samples: 3,
        utterance_samples: 5,
      }),
    }

    await expect(invokeRuntimeControl('begin_listening')).rejects.toThrow(
      'Runtime control payload must include last_activity_ms',
    )
  })
})

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
          transcript_text: null,
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
      transcriptText: null,
      lastActivityMs: 100,
      capturingUtterance: true,
      prerollSamples: 3,
      utteranceSamples: 5,
      telemetry: null,
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
          transcript_text: null,
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
      transcriptText: null,
      lastActivityMs: 101,
      capturingUtterance: true,
      prerollSamples: 4,
      utteranceSamples: 4,
      telemetry: null,
    })
  })

  it('parses audio frame status payloads from tauri commands', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        expect(command).toBe('ingest_audio_frame')
        expect(args).toEqual({ frame: [0.1, 0.2, 0.3] })

        return {
          runtime_phase: 'sleeping',
          transcription_ready_samples: null,
          transcript_text: null,
          last_activity_ms: null,
          capturing_utterance: false,
          preroll_samples: 3,
          utterance_samples: 0,
        }
      },
    }

    await expect(ingestAudioFrame([0.1, 0.2, 0.3])).resolves.toEqual({
      runtimePhase: 'sleeping',
      transcriptionReadySamples: null,
      transcriptText: null,
      lastActivityMs: null,
      capturingUtterance: false,
      prerollSamples: 3,
      utteranceSamples: 0,
      telemetry: null,
    })
  })

  it('passes telemetry frame id when provided for ingest calls', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        expect(command).toBe('ingest_audio_frame')
        expect(args).toEqual({
          frame: [0.1, 0.2, 0.3],
          telemetryFrameId: 'frame-1000-1',
        })

        return {
          runtime_phase: 'sleeping',
          transcription_ready_samples: null,
          transcript_text: null,
          last_activity_ms: null,
          capturing_utterance: false,
          preroll_samples: 3,
          utterance_samples: 0,
          telemetry: {
            frame_id: 'frame-1000-1',
            backend_ingest_started_ms: 1002,
            backend_ingest_completed_ms: 1005,
            wake_detected_ms: 1004,
            wake_confidence: 0.63,
          },
        }
      },
    }

    await expect(
      ingestAudioFrame([0.1, 0.2, 0.3], { telemetryFrameId: 'frame-1000-1' }),
    ).resolves.toEqual({
      runtimePhase: 'sleeping',
      transcriptionReadySamples: null,
      transcriptText: null,
      lastActivityMs: null,
      capturingUtterance: false,
      prerollSamples: 3,
      utteranceSamples: 0,
      telemetry: {
        frameId: 'frame-1000-1',
        backendIngestStartedMs: 1002,
        backendIngestCompletedMs: 1005,
        wakeDetectedMs: 1004,
        wakeConfidence: 0.63,
        transcriptionStartedMs: null,
        transcriptionCompletedMs: null,
      },
    })
  })

  it('rejects runtime control payloads missing capture fields', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async () => ({
        runtime_phase: 'listening',
        transcription_ready_samples: null,
        transcript_text: null,
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
        transcript_text: null,
        capturing_utterance: true,
        preroll_samples: 3,
        utterance_samples: 5,
      }),
    }

    await expect(invokeRuntimeControl('begin_listening')).rejects.toThrow(
      'Runtime control payload must include last_activity_ms',
    )
  })

  it('rejects runtime control payloads missing transcript text', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async () => ({
        runtime_phase: 'processing',
        transcription_ready_samples: 3200,
        last_activity_ms: null,
        capturing_utterance: false,
        preroll_samples: 3,
        utterance_samples: 0,
      }),
    }

    await expect(invokeRuntimeControl('mark_silence')).rejects.toThrow(
      'Runtime control payload must include transcript_text',
    )
  })

  it('parses transcript text from runtime control payloads', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async () => ({
        runtime_phase: 'processing',
        transcription_ready_samples: 3200,
        transcript_text: 'Draft release notes',
        last_activity_ms: null,
        capturing_utterance: false,
        preroll_samples: 3,
        utterance_samples: 0,
      }),
    }

    await expect(invokeRuntimeControl('mark_silence')).resolves.toEqual({
      runtimePhase: 'processing',
      transcriptionReadySamples: 3200,
      transcriptText: 'Draft release notes',
      lastActivityMs: null,
      capturingUtterance: false,
      prerollSamples: 3,
      utteranceSamples: 0,
      telemetry: null,
    })
  })

  it('parses mark_silence transcription telemetry and forwards telemetry frame id args', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        expect(command).toBe('mark_silence')
        expect(args).toEqual({ telemetryFrameId: 'frame-4321-2' })

        return {
          runtime_phase: 'processing',
          transcription_ready_samples: 3200,
          transcript_text: 'Draft release notes',
          last_activity_ms: null,
          capturing_utterance: false,
          preroll_samples: 3,
          utterance_samples: 0,
          telemetry: {
            frame_id: 'frame-4321-2',
            transcription_started_ms: 5000,
            transcription_completed_ms: 5120,
          },
        }
      },
    }

    await expect(
      invokeRuntimeControl('mark_silence', { telemetryFrameId: 'frame-4321-2' }),
    ).resolves.toEqual({
      runtimePhase: 'processing',
      transcriptionReadySamples: 3200,
      transcriptText: 'Draft release notes',
      lastActivityMs: null,
      capturingUtterance: false,
      prerollSamples: 3,
      utteranceSamples: 0,
      telemetry: {
        frameId: 'frame-4321-2',
        backendIngestStartedMs: null,
        backendIngestCompletedMs: null,
        wakeDetectedMs: null,
        wakeConfidence: null,
        transcriptionStartedMs: 5000,
        transcriptionCompletedMs: 5120,
      },
    })
  })

  it('rejects malformed telemetry payload fields', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async () => ({
        runtime_phase: 'sleeping',
        transcription_ready_samples: null,
        transcript_text: null,
        last_activity_ms: null,
        capturing_utterance: false,
        preroll_samples: 0,
        utterance_samples: 0,
        telemetry: {
          frame_id: 7,
        },
      }),
    }

    await expect(ingestAudioFrame([0.1])).rejects.toThrow(
      'Runtime control telemetry field frame_id must be a string or null',
    )
  })

  it('rejects telemetry payloads that are not objects', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async () => ({
        runtime_phase: 'sleeping',
        transcription_ready_samples: null,
        transcript_text: null,
        last_activity_ms: null,
        capturing_utterance: false,
        preroll_samples: 0,
        utterance_samples: 0,
        telemetry: 'bad-telemetry',
      }),
    }

    await expect(ingestAudioFrame([0.1])).rejects.toThrow(
      'Runtime control payload telemetry must be an object when present',
    )
  })
})

import { afterEach, describe, expect, it } from 'vitest'
import { invokeRuntimeControl } from './runtimeControl'

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
        }
      },
    }

    await expect(invokeRuntimeControl('begin_listening')).resolves.toEqual({
      runtimePhase: 'listening',
      transcriptionReadySamples: null,
    })
  })
})

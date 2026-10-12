import { afterEach, describe, expect, it, vi } from 'vitest'
import { parseCaptureRecord, parseCaptureSnapshot } from './captures'

afterEach(() => {
  delete window.__TAURI_INTERNALS__
  vi.unstubAllEnvs()
  vi.resetModules()
})

describe('capture IPC adapter', () => {
  it('preserves voice-entry purpose and treats only legacy records as Notes', () => {
    expect(parseCaptureRecord(record())).toMatchObject({ purpose: 'NOTE' })
    for (const purpose of ['NOTE', 'ASK', 'TALK', 'DRAFT']) {
      expect(parseCaptureRecord({ ...record(), purpose })).toMatchObject({ purpose })
    }
    expect(() => parseCaptureRecord({ ...record(), purpose: 'invented' })).toThrow('Capture purpose is invalid')
    expect(() => parseCaptureRecord({ ...record(), purpose: null })).toThrow('Capture purpose is invalid')
  })
  it('preserves captured speech policy and live readiness without treating availability as accepted input', () => {
    expect(parseCaptureRecord({ ...record(), purpose: 'ASK', orgState: 'NONE', requestedLanguage: 'en-AU',
      speechNetworkFallback: false, liveTranscriptionState: 'UNAVAILABLE', liveStatusRevision: 3,
      canTranscribeLive: false, liveErrorCode: 'external-audio-provider-unavailable' })).toMatchObject({
      purpose: 'ASK', orgState: 'NONE', requestedLanguage: 'en-AU', speechNetworkFallback: false,
      liveTranscriptionState: 'UNAVAILABLE', liveStatusRevision: 3, canTranscribeLive: false,
      liveErrorCode: 'external-audio-provider-unavailable',
    })
    expect(parseCaptureRecord(record())).toMatchObject({ requestedLanguage: null, speechNetworkFallback: false,
      liveTranscriptionState: 'NOT_REQUESTED', liveStatusRevision: 0, canTranscribeLive: false, liveErrorCode: null })
    expect(() => parseCaptureRecord({ ...record(), liveStatusRevision: -1 })).toThrow('Capture liveStatusRevision is invalid')
    expect(() => parseCaptureRecord({ ...record(), liveTranscriptionState: 'invented' })).toThrow('Capture liveTranscriptionState is invalid')
    expect(() => parseCaptureRecord({ ...record(), speechNetworkFallback: 'true' })).toThrow('Capture speechNetworkFallback is invalid')
  })
  it('preserves explicit onboarding progress and errors without treating startup as failure', () => {
    const base = { available: true, setupRequired: true, notesFolderUri: null, captures: [], error: null }
    expect(parseCaptureSnapshot({ ...base, onboardingState: 'starting', onboardingError: null })).toMatchObject({
      onboardingState: 'starting', onboardingError: null,
    })
    expect(parseCaptureSnapshot({ ...base, onboardingState: 'permission-required', onboardingError: 'synthetic-denial' })).toMatchObject({
      onboardingState: 'permission-required', onboardingError: 'synthetic-denial',
    })
    expect(() => parseCaptureSnapshot({ ...base, onboardingState: 'invented', onboardingError: null })).toThrow('Capture onboarding state is invalid')
    expect(parseCaptureSnapshot({ ...base, setupRequired: true, onboardingState: 'enabled', onboardingError: null }).setupRequired).toBe(false)
    expect(parseCaptureSnapshot({ ...base, error: 'synthetic-legacy-error' }).onboardingError).toBe('synthetic-legacy-error')
  })
  it('validates the camelCase capture schema and rejects malformed native payloads', () => {
    expect(parseCaptureSnapshot({ available: true, notesFolderUri: null, captures: [record()], error: null })).toMatchObject({ setupRequired: false })
    expect(parseCaptureSnapshot({ available: true, notesFolderUri: null, captures: [record()], error: null }).captures[0]).toMatchObject({
      captureId: 'synthetic-capture-1', previewText: 'Last preview', finalText: 'Final words', transcriptVersion: 2,
    })
    expect(() => parseCaptureRecord({ ...record(), durationMs: -1 })).toThrow('Capture record is invalid')
    expect(() => parseCaptureRecord({ ...record(), recognitionState: 'invented' })).toThrow('Capture recognitionState is invalid')
    expect(() => parseCaptureSnapshot({ available: true, notesFolderUri: null, captures: [record(), {}], error: null })).toThrow('Capture record is invalid')
    expect(() => parseCaptureSnapshot({ available: true, notesFolderUri: null, captures: Array.from({ length: 101 }, record), error: null })).toThrow('Capture snapshot is invalid')
    expect(() => parseCaptureSnapshot({ available: true, setupRequired: 'yes', notesFolderUri: null, captures: [], error: null })).toThrow('Capture snapshot is invalid')
  })

  it('calls only the plugin capture IPC contract with bounded list pages', async () => {
    vi.stubEnv('VITE_VOXGOLEM_RUNTIME', 'android')
    vi.resetModules()
    const calls: Array<[string, unknown]> = []
    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        calls.push([command, args])
        if (command === 'plugin:capture|snapshot') return { available: true, notesFolderUri: null, captures: [record()], error: null }
        if (command === 'plugin:capture|list_captures') return [record()]
        if (command === 'plugin:capture|enable_capture') return { available: true, setupRequired: false, notesFolderUri: null, captures: [record()], error: null }
        return null
      },
    }
    const capture = await import('./captures')
    expect((await capture.loadCaptureSnapshot()).captures).toHaveLength(1)
    expect(await capture.listCaptures(100, 20)).toHaveLength(1)
    expect((await capture.enableCapture()).setupRequired).toBe(false)
    await capture.playCapture('synthetic-capture-1')
    expect(calls).toEqual([
      ['plugin:capture|snapshot', undefined],
      ['plugin:capture|list_captures', { offset: 100, limit: 20 }],
      ['plugin:capture|enable_capture', undefined],
      ['plugin:capture|play_capture', { captureId: 'synthetic-capture-1' }],
    ])
    await expect(capture.listCaptures(-1)).rejects.toThrow('Capture page is invalid')
  })

  it('does not invoke native capture IPC outside Android', async () => {
    const invoke = vi.fn()
    window.__TAURI_INTERNALS__ = { invoke }
    const capture = await import('./captures')
    await expect(capture.loadCaptureSnapshot()).rejects.toThrow('Voice notes are available only in the Android app')
    await expect(capture.playCapture('synthetic-capture-1')).rejects.toThrow('Voice notes are available only in the Android app')
    expect(invoke).not.toHaveBeenCalled()
  })
})

function record(): Record<string, unknown> {
  return {
    captureId: 'synthetic-capture-1', createdAtUtc: '2026-01-02T03:04:05Z', finishedAtUtc: '2026-01-02T03:04:35Z',
    durationMs: 30_000, captureState: 'SAVED', recognitionState: 'FINAL', transferState: 'PHONE_ARCHIVED',
    archiveState: 'VERIFIED', orgState: 'EXPORTED', previewText: 'Last preview', finalText: 'Final words',
    archivedAudioUri: 'content://synthetic/recording.wav', error: null, transcriptVersion: 2,
  }
}

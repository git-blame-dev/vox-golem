import { describe, expect, it } from 'vitest'
import { parseAndroidVoiceEvent, parseAndroidVoiceStatus } from './androidVoice'

describe('native Android voice boundary', () => {
  it('parses reported speech selection options with legacy omission compatibility and strict inventory bounds', () => {
    const wire = { kind: 'voice_status', available: true, reason: 'Synthetic speech service', mode: 'dictation', dictation: 'idle', loop: 'idle', ttsEnabled: true, ttsAvailable: true, ttsPhase: 'idle', error: null }
    const options = { ttsProviders: ['synthetic.tts.engine'], ttsVoices: [{ provider: '', name: 'synthetic-system-voice', language: 'en-AU', networkRequired: false }], recognitionInstalledLanguages: ['en-AU'], recognitionSupportedLanguages: null, localOnlyRecognitionAvailable: true, modelSelectionAvailable: false, modelIdentifiers: null }
    expect(parseAndroidVoiceStatus(wire)).toMatchObject({ speechLanguage: '', speechTtsProvider: '', speechTtsVoice: '', speechOptions: null })
    expect(parseAndroidVoiceStatus({ ...wire, speechLanguage: 'en-AU', speechTtsProvider: '', speechTtsVoice: '', speechOptions: options })).toMatchObject({ speechLanguage: 'en-AU', speechOptions: options })
    expect(parseAndroidVoiceStatus({ ...wire, speechOptions: { ...options, ttsProviders: Array(8).fill('synthetic.tts.engine'), ttsVoices: Array(1024).fill(options.ttsVoices[0]) } }).speechOptions?.ttsVoices).toHaveLength(1024)
    for (const invalid of [{ ...options, ttsProviders: Array(9).fill('synthetic.tts.engine') }, { ...options, ttsVoices: Array(1025).fill(options.ttsVoices[0]) }, { ...options, ttsVoices: [{ ...options.ttsVoices[0], networkRequired: 'false' }] }, { ...options, recognitionSupportedLanguages: [null] }, { ...options, modelSelectionAvailable: true }, { ...options, modelIdentifiers: ['synthetic-invented-model'] }]) expect(() => parseAndroidVoiceStatus({ ...wire, speechOptions: invalid })).toThrow()
    for (const invalid of [null, { ...options, recognitionInstalledLanguages: Array(129).fill('en-AU') }, { ...options, ttsProviders: ['é'.repeat(257)] }, { ...options, ttsVoices: Array(1) }]) expect(() => parseAndroidVoiceStatus({ ...wire, speechOptions: invalid })).toThrow()
    for (const key of ['speechLanguage', 'speechTtsProvider', 'speechTtsVoice']) for (const value of [null, 1, 'é'.repeat(257)]) expect(() => parseAndroidVoiceStatus({ ...wire, [key]: value })).toThrow()
  })
  it('passes supported recognition languages separately from installed languages and rejects malformed evidence lists', () => {
    const metadata = { selectedProvider: 'synthetic.recognition/Service', requestedLanguage: 'en-AU', selectedLanguage: 'en-AU', reportedLanguage: null, installedLanguages: ['en-AU'], supportedLanguages: ['en-AU', 'en-GB'], actualProcessing: 'not_reported', model: null, offlineTrialVerified: false, fallbackReason: null }
    const wire = { kind: 'voice_status', available: true, reason: 'Synthetic speech service', mode: 'dictation', dictation: 'idle', loop: 'idle', ttsEnabled: false, ttsAvailable: true, ttsPhase: 'idle', error: null, recognitionMetadata: metadata }
    expect(parseAndroidVoiceStatus(wire).recognitionMetadata).toMatchObject({ installedLanguages: ['en-AU'], supportedLanguages: ['en-AU', 'en-GB'] })
    expect(() => parseAndroidVoiceStatus({ ...wire, recognitionMetadata: { ...metadata, supportedLanguages: [false] } })).toThrow()
  })
  it('retains structured speech evidence and raw diagnostics without inferring actual network use from a selected local voice', () => {
    const metadata = { configuredProvider: 'synthetic.tts.preference', reportedProvider: null, requestedLanguage: 'en-AU', selectedLanguage: 'en-GB', reportedLanguage: null, selectedVoice: 'synthetic-en-GB-profile', installedLanguages: ['en-GB'], networkRequired: false, actualProcessing: 'not_reported', fallbackReason: 'Synthetic requested locale unavailable', model: null, offlineTrialVerified: false }
    const wire = { kind: 'voice_status', available: true, reason: 'Synthetic speech service', mode: 'dictation', dictation: 'idle', loop: 'idle', ttsEnabled: true, ttsAvailable: true, ttsPhase: 'idle', error: null, diagnosticError: 'recognition_error_7', ttsMetadata: metadata }
    expect(parseAndroidVoiceEvent({ ...wire, sessionId: 'synthetic-session' })).toMatchObject({ status: { ttsMetadata: metadata, diagnosticError: 'recognition_error_7' } })
    for (const malformed of [{ ...metadata, networkRequired: 'false' }, { ...metadata, actualProcessing: 'offline_verified' }, { ...metadata, offlineTrialVerified: 1 }, { ...metadata, installedLanguages: [null] }, { ...metadata, model: 'x'.repeat(513) }]) {
      expect(() => parseAndroidVoiceStatus({ ...wire, ttsMetadata: malformed })).toThrow()
    }
  })
  it('keeps native dictation separate from desktop wake/VAD contracts', () => {
    expect(parseAndroidVoiceStatus({ kind: 'voice_status', available: true, reason: 'Synthetic local recognizer', mode: 'dictation', dictation: 'listening', loop: 'idle', ttsEnabled: false, ttsAvailable: true, ttsPhase: 'idle', error: null })).toMatchObject({ available: true, dictation: 'listening' })
    expect(() => parseAndroidVoiceStatus({ available: true, parakeet: true })).toThrow()
  })
  it('dictation final fills a draft while only a loop submit event requests execution', () => {
    expect(parseAndroidVoiceEvent({ sessionId: 'synthetic-session', kind: 'dictation', phase: 'final', draft: 'Synthetic final words', error: null, utterance: '7' })).toMatchObject({ kind: 'dictation', draft: 'Synthetic final words' })
    expect(parseAndroidVoiceEvent({ sessionId: 'synthetic-session', kind: 'submit', requestId: 'synthetic-request', text: 'Synthetic final words' })).toMatchObject({ kind: 'submit', requestId: 'synthetic-request' })
  })
  it('binds optional native voice submissions to both conversation and selected head with legacy omission compatibility', () => {
    const payload = { sessionId: 'synthetic-session', kind: 'submit', requestId: 'synthetic-request', text: 'Synthetic final words' }
    const context = { sessionId: '11111111-1111-4111-8111-111111111111:1', selectedHead: '11111111-1111-4111-8111-111111111111:2' }
    expect(parseAndroidVoiceEvent(payload)).not.toHaveProperty('context')
    expect(parseAndroidVoiceEvent({ ...payload, context })).toMatchObject({ context })
    expect(parseAndroidVoiceEvent({ ...payload, context: { ...context, selectedHead: null } })).toMatchObject({ context: { selectedHead: null } })
    for (const invalid of [null, { sessionId: context.sessionId }, { ...context, selectedHead: 1 }, { ...context, selectedHead: '' }, { ...context, sessionId: 'x'.repeat(129) }]) expect(() => parseAndroidVoiceEvent({ ...payload, context: invalid })).toThrow()
  })
  it('rejects oversized, empty and uncorrelated execution events', () => {
    for (const payload of [{ kind: 'submit', text: 'Synthetic text' }, { sessionId: 'synthetic-session', kind: 'submit', requestId: 'synthetic-request', text: '' }, { sessionId: 'synthetic-session', kind: 'submit', requestId: 'synthetic-request', text: 'é'.repeat(16_385) }]) expect(() => parseAndroidVoiceEvent(payload)).toThrow()
  })
  it('preserves optional native readiness without inferring it from a listening phase', () => {
    const status = { kind: 'voice_status', available: true, reason: 'Synthetic speech service', mode: 'dictation',
      dictation: 'listening', loop: 'idle', ttsEnabled: false, ttsAvailable: false, ttsPhase: 'idle', error: null }
    expect(parseAndroidVoiceStatus(status).readyForSpeech).toBeUndefined()
    for (const readyForSpeech of [false, true]) {
      expect(parseAndroidVoiceStatus({ ...status, readyForSpeech }).readyForSpeech).toBe(readyForSpeech)
      expect(parseAndroidVoiceEvent({ ...status, sessionId: 'synthetic-session', readyForSpeech })).toMatchObject({
        kind: 'voice_status', status: { readyForSpeech },
      })
    }
    for (const readyForSpeech of [null, 'true', 1, {}]) expect(() => parseAndroidVoiceStatus({ ...status, readyForSpeech })).toThrow()
  })
  it('preserves optional route disclosure while accepting older native statuses', () => {
    const status = { kind: 'voice_status', available: true, reason: 'Synthetic speech service', mode: 'dictation',
      dictation: 'listening', loop: 'idle', ttsEnabled: true, ttsAvailable: true, ttsPhase: 'idle', error: null }
    expect(parseAndroidVoiceStatus(status)).toMatchObject({ recognitionRoute: null, ttsRoute: null })
    expect(parseAndroidVoiceStatus(status).localOnlyRecognitionAvailable).toBe(false)
    expect(parseAndroidVoiceStatus({ ...status, localOnlyRecognitionAvailable: true }).localOnlyRecognitionAvailable).toBe(true)
    expect(() => parseAndroidVoiceStatus({ ...status, localOnlyRecognitionAvailable: 'true' })).toThrow(/capability/)
    expect(parseAndroidVoiceStatus({ ...status, recognitionRoute: 'Google · en-AU requested · provider-controlled · may use network · fallback',
      ttsRoute: 'Samsung preference · en-GB · installed local voice' })).toMatchObject({
      recognitionRoute: 'Google · en-AU requested · provider-controlled · may use network · fallback',
      ttsRoute: 'Samsung preference · en-GB · installed local voice',
    })
    expect(() => parseAndroidVoiceStatus({ ...status, recognitionRoute: '' })).toThrow()
    expect(() => parseAndroidVoiceStatus({ ...status, ttsRoute: 'x'.repeat(513) })).toThrow()
  })
  it('passes all neutral locale evidence and provider-network disclosure through status and Channel events', () => {
    const status = { kind: 'voice_status', available: true, reason: 'Synthetic speech service', mode: 'dictation',
      dictation: 'listening', loop: 'idle', ttsEnabled: true, ttsAvailable: true, ttsPhase: 'idle', error: null,
      recognitionRoute: 'Google · requested en-AU · selected en-US · reported en-GB · local preferred · provider-controlled · may use network · fallback',
      ttsRoute: 'Samsung preference · requested en-AU · selected en-GB · installed local voice · fallback' }
    expect(parseAndroidVoiceStatus(status)).toMatchObject({ recognitionRoute: status.recognitionRoute, ttsRoute: status.ttsRoute })
    expect(parseAndroidVoiceEvent({ ...status, sessionId: 'synthetic-session' })).toMatchObject({ kind: 'voice_status',
      status: { recognitionRoute: status.recognitionRoute, ttsRoute: status.ttsRoute } })
  })
})

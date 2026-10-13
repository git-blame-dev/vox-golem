import { act } from 'react'
import { createRoot } from 'react-dom/client'
import { describe, expect, it } from 'vitest'
import { parseStartupState, CAPABILITY_IDS } from '../lib/startupState'
import { parseAndroidVoiceStatus } from '../lib/androidVoice'
import { SpeechSettings } from './SpeechSettings'

function startup(speechServices?: unknown, ttsProvider: string | null = 'cuda') {
  return parseStartupState({
    kind: 'ready',
    cue_asset_paths: { start_listening: 'start.wav', stop_listening: 'stop.wav' },
    runtime_phase: 'sleeping',
    voice_input_available: true,
    voice_input_error: null,
    silence_timeout_ms: 750,
    selected_response_profile: 'fast',
    supported_response_profiles: ['fast'],
    tts_enabled: true,
    capabilities: CAPABILITY_IDS.map((id) => ({ id, state: 'available', reason: 'ready', actual_provider: id === 'tts' ? ttsProvider : 'cpu' })),
    ...(speechServices === undefined ? {} : { speech_services: speechServices }),
  })
}

const identity = {
  stt: { configured_engine: 'Parakeet', runtime_engine: 'Parakeet', configured_model: null, configured_voice: null, configured_locale: null },
  tts: { configured_engine: 'Piper ONNX', runtime_engine: 'Piper ONNX', configured_model: null, configured_voice: null, configured_locale: null },
}

const speechOptions = {
  ttsProviders: ['synthetic.tts.a', 'synthetic.tts.b'],
  ttsVoices: [
    { provider: '', name: 'synthetic-system-local', language: 'en-AU', networkRequired: false },
    { provider: 'synthetic.tts.a', name: 'synthetic-a-local', language: 'en-AU', networkRequired: false },
    { provider: 'synthetic.tts.a', name: 'synthetic-a-network', language: 'en-GB', networkRequired: true },
    { provider: 'synthetic.tts.b', name: 'synthetic-b-local', language: 'en-GB', networkRequired: false },
  ],
  recognitionInstalledLanguages: ['en-AU'], recognitionSupportedLanguages: ['en-AU', 'en-GB'], localOnlyRecognitionAvailable: true, modelSelectionAvailable: false, modelIdentifiers: null,
}

function voiceStatus(extra: Record<string, unknown> = {}) {
  return parseAndroidVoiceStatus({ kind: 'voice_status', available: true, mode: 'dictation', dictation: 'idle', loop: 'idle', error: null, reason: 'Synthetic recognizer', ttsPhase: 'idle', ttsAvailable: true, ttsEnabled: true, ...extra })
}

function renderSpeech(props: Parameters<typeof SpeechSettings>[0]) {
  const container = document.createElement('div')
  document.body.append(container)
  const root = createRoot(container)
  act(() => root.render(<SpeechSettings {...props} />))
  return { container, dispose: () => { act(() => root.unmount()); container.remove() } }
}

describe('SpeechSettings', () => {
  it('offers only Automatic before SDK support is reported and leaves hidden-model selection explicitly unavailable', () => {
    const { container, dispose } = renderSpeech({ android: true, androidStatus: voiceStatus(), statusError: null, startupState: startup(), onSpeechSelectionChange: () => undefined })
    try {
      for (const label of ['Speech language', 'TTS engine', 'TTS voice']) {
        const select = container.querySelector<HTMLSelectElement>(`[aria-label="${label}"]`)!
        expect(select).not.toBeNull()
        expect(Array.from(select.options).map((option) => option.value)).toEqual([''])
        expect(select.selectedOptions[0]?.textContent).toContain('Automatic')
      }
      const model = container.querySelector<HTMLSelectElement>('[aria-label="Speech model"]')!
      expect(model.disabled).toBe(true)
      expect(model.textContent).toContain('Unavailable')
      expect(model.textContent).toContain('not reported')
      expect(container.textContent).not.toContain('en-AU')
    } finally { dispose() }
  })
  it('scopes voice choices to the requested provider, disables network voices with fallback off, and explicitly clears voice on engine change', () => {
    const changes: unknown[] = []
    const selection = { speechLanguage: 'en-AU', speechTtsProvider: 'synthetic.tts.a', speechTtsVoice: 'synthetic-a-network' }
    const { container, dispose } = renderSpeech({ android: true, androidStatus: voiceStatus({ speechOptions }), statusError: null, startupState: startup(), speechNetworkFallback: false, speechSelection: selection, onSpeechSelectionChange: (next) => changes.push(next) })
    try {
      const voice = container.querySelector<HTMLSelectElement>('[aria-label="TTS voice"]')!
      expect(Array.from(voice.options).map((option) => option.value)).toEqual(['', 'synthetic-a-local', 'synthetic-a-network'])
      expect(voice.value).toBe('synthetic-a-network')
      expect(voice.selectedOptions[0]?.disabled).toBe(true)
      expect(voice.textContent).not.toContain('synthetic-b-local')
      expect(voice.textContent).toContain('network fallback is off')
      act(() => { voice.value = 'synthetic-a-network'; voice.dispatchEvent(new Event('change', { bubbles: true })) })
      expect(changes).toEqual([])
      const language = container.querySelector<HTMLSelectElement>('[aria-label="Speech language"]')!
      expect(Array.from(language.options).map((option) => option.value)).toEqual(['', 'en-AU'])
      const engine = container.querySelector<HTMLSelectElement>('[aria-label="TTS engine"]')!
      expect(container.textContent).toContain('Changing the TTS engine resets the requested voice to Automatic')
      act(() => { engine.value = 'synthetic.tts.b'; engine.dispatchEvent(new Event('change', { bubbles: true })) })
      expect(changes).toEqual([{ ...selection, speechTtsProvider: 'synthetic.tts.b', speechTtsVoice: '' }])
    } finally { dispose() }
  })
  it('keeps removed requested selections visible with an explicit all-Automatic recovery path, separate from native-reported configuration', () => {
    const changes: unknown[] = []
    const selection = { speechLanguage: 'synthetic-removed-locale', speechTtsProvider: 'synthetic.removed.engine', speechTtsVoice: 'synthetic-removed-voice' }
    const { container, dispose } = renderSpeech({ android: true, androidStatus: voiceStatus({ speechOptions, speechLanguage: 'en-GB', speechTtsProvider: '', speechTtsVoice: 'synthetic-system-local' }), statusError: null, startupState: startup(), speechSelection: selection, onSpeechSelectionChange: (next) => changes.push(next) })
    try {
      for (const [label, value] of [['Speech language', selection.speechLanguage], ['TTS engine', selection.speechTtsProvider], ['TTS voice', selection.speechTtsVoice]]) {
        const select = container.querySelector<HTMLSelectElement>(`[aria-label="${label}"]`)!
        expect(select.value).toBe(value)
        expect(select.selectedOptions[0]?.disabled).toBe(true)
        expect(select.selectedOptions[0]?.textContent).toContain('not currently reported')
      }
      expect(container.querySelector('[aria-label="Last native-reported requested configuration"]')?.textContent).toContain('en-GB')
      act(() => container.querySelector<HTMLButtonElement>('[aria-label="Use Automatic speech settings"]')!.click())
      expect(changes).toEqual([{ speechLanguage: '', speechTtsProvider: '', speechTtsVoice: '' }])
    } finally { dispose() }
  })
  it.each([false, true])('does not accept forged unknown choices or bypass disabled controls (disabled: %s)', (disabled) => {
    const changes: unknown[] = []
    const { container, dispose } = renderSpeech({ android: true, androidStatus: voiceStatus({ speechOptions }), statusError: null, startupState: startup(), disabled, speechSelection: { speechLanguage: 'en-AU', speechTtsProvider: '', speechTtsVoice: '' }, onSpeechSelectionChange: (next) => changes.push(next) })
    try {
      for (const label of ['Speech language', 'TTS engine', 'TTS voice']) {
        const select = container.querySelector<HTMLSelectElement>(`[aria-label="${label}"]`)!
        expect(select.disabled).toBe(disabled)
        const forged = document.createElement('option'); forged.value = 'synthetic-forged-choice'; select.append(forged)
        act(() => { select.value = forged.value; select.dispatchEvent(new Event('change', { bubbles: true })) })
      }
      if (disabled) act(() => container.querySelector<HTMLButtonElement>('[aria-label="Use Automatic speech settings"]')!.click())
      expect(changes).toEqual([])
    } finally { dispose() }
  })
  it('keeps fallback recoverable after local-only capability disappears without offering unsupported disabling', () => {
    let changed: boolean | null = null
    const androidStatus = parseAndroidVoiceStatus({ kind: 'voice_status', available: false, mode: 'dictation', dictation: 'idle', loop: 'idle', error: null, reason: 'Synthetic local-only unavailable', ttsPhase: 'idle', ttsAvailable: false, ttsEnabled: false, localOnlyRecognitionAvailable: false })
    const { container, dispose } = renderSpeech({ android: true, androidStatus, statusError: null, startupState: startup(), speechNetworkFallback: false, onSpeechNetworkFallbackChange: (enabled) => { changed = enabled } })
    try {
      const checkbox = container.querySelector<HTMLInputElement>('[aria-label="Allow speech network fallback"]')!
      expect(checkbox).not.toBeNull()
      expect(checkbox.disabled).toBe(false)
      act(() => checkbox.click())
      expect(changed).toBe(true)
      expect(container.textContent).toContain('dedicated on-device API is unavailable or not reported')
      expect(container.textContent).toContain('Ordinary recognition providers may still work offline')
    } finally { dispose() }
    const enabled = renderSpeech({ android: true, androidStatus, statusError: null, startupState: startup(), speechNetworkFallback: true, onSpeechNetworkFallbackChange: () => undefined })
    try { expect(enabled.container.querySelector('[aria-label="Allow speech network fallback"]')).toBeNull() } finally { enabled.dispose() }
  })
  it('reports pending capability as checking rather than declaring offline recognition impossible', () => {
    const { container, dispose } = renderSpeech({ android: true, androidStatus: null, statusError: null, startupState: startup(),
      onSpeechNetworkFallbackChange: () => undefined })
    try {
      expect(container.textContent).toContain('Checking dedicated on-device recognition capability')
      expect(container.textContent).not.toContain('Local-only recognition is unavailable')
      expect(container.querySelector('[aria-label="Allow speech network fallback"]')).toBeNull()
    } finally { dispose() }
  })
  it('keeps failed status observation distinct from loading or proof of unavailable offline recognition', () => {
    const { container, dispose } = renderSpeech({ android: true, androidStatus: null,
      statusError: 'Synthetic status observation failed', startupState: startup(), onSpeechNetworkFallbackChange: () => undefined })
    try {
      expect(container.textContent).toContain('Dedicated on-device recognition capability is not reported')
      expect(container.textContent).toContain('Keep Chat foreground to refresh native speech status')
      expect(container.textContent).toContain('Synthetic status observation failed')
      expect(container.textContent).not.toContain('Checking dedicated on-device recognition capability')
      expect(container.querySelector('[aria-label="Allow speech network fallback"]')).toBeNull()
    } finally { dispose() }
  })
  it('separates requested, selected, installed and reported speech facts from unknown current processing and model identity', () => {
    const recognitionMetadata = { selectedProvider: 'synthetic.recognition/Service', requestedLanguage: 'en-AU', selectedLanguage: 'en-US', reportedLanguage: 'en-GB', installedLanguages: ['en-US'], actualProcessing: 'not_reported', localPreferred: true, fallbackReason: 'Synthetic locale fallback', model: null, offlineTrialVerified: false }
    const ttsMetadata = { configuredProvider: 'synthetic.tts.preference', reportedProvider: null, requestedLanguage: 'en-AU', selectedLanguage: 'en-GB', reportedLanguage: null, selectedVoice: 'synthetic-en-GB-profile', installedLanguages: ['en-GB'], networkRequired: false, actualProcessing: 'not_reported', fallbackReason: null, model: null, offlineTrialVerified: false }
    const androidStatus = parseAndroidVoiceStatus({ kind: 'voice_status', available: true, mode: 'dictation', dictation: 'idle', loop: 'idle', error: null, diagnosticError: 'recognition_error_7', reason: 'Synthetic native recognizer', ttsPhase: 'idle', ttsAvailable: true, ttsEnabled: true, recognitionMetadata, ttsMetadata })
    const { container, dispose } = renderSpeech({ android: true, androidStatus, statusError: null, startupState: startup() })
    try {
      for (const fact of ['Selected provider: synthetic.recognition/Service', 'Requested locale: en-AU', 'Selected locale: en-US', 'Provider-reported locale: en-GB', 'Installed locales: en-US', 'Configured provider preference: synthetic.tts.preference', 'Reported provider: Not reported', 'Selected voice profile: synthetic-en-GB-profile', 'SDK voice network requirement: Not required', 'Actual local/network processing: Not reported', 'Underlying model identity: Not reported', 'Offline trial verification reported by runtime: Not reported']) expect(container.textContent).toContain(fact)
      expect(container.textContent).not.toContain('recognition_error_7')
      expect(container.textContent).not.toContain('Local processing')
    } finally { dispose() }
  })
  it.each([false, true])('shows the fallback preference only for explicit native support (supported: %s)', (supported) => {
    let changed: boolean | null = null
    const androidStatus = parseAndroidVoiceStatus({ kind: 'voice_status', available: true, mode: 'dictation', dictation: 'idle', loop: 'idle', error: 'recognition_error_7', reason: 'Synthetic recognizer ready', ttsPhase: 'idle', ttsAvailable: true, ttsEnabled: true, ...(supported ? { localOnlyRecognitionAvailable: true } : {}) })
    const { container, dispose } = renderSpeech({ android: true, androidStatus, statusError: null, startupState: startup(), speechNetworkFallback: true, onSpeechNetworkFallbackChange: (value) => { changed = value } })
    try {
      const toggle = container.querySelector<HTMLInputElement>('[aria-label="Allow speech network fallback"]')
      expect(toggle !== null).toBe(supported)
      expect(container.textContent).not.toContain('recognition_error_7')
      if (toggle) {
        expect(toggle.checked).toBe(true)
        expect(container.textContent).toContain('not proof of offline processing')
        act(() => toggle.click())
        expect(changed).toBe(false)
      }
    } finally { dispose() }
  })
  it('retains actionable retirement failures and never invents desktop fallback controls', () => {
    const androidStatus = parseAndroidVoiceStatus({ kind: 'voice_status', available: false, mode: 'dictation', dictation: 'error', loop: 'idle', error: 'recognition_retirement_failed', reason: 'Synthetic unavailable recognizer', ttsPhase: 'idle', ttsAvailable: false, ttsEnabled: false, localOnlyRecognitionAvailable: true })
    const phone = renderSpeech({ android: true, androidStatus, statusError: null, startupState: startup() })
    const desktop = renderSpeech({ android: false, androidStatus, statusError: null, startupState: startup(), speechSelection: { speechLanguage: 'en-AU', speechTtsProvider: 'synthetic.tts.a', speechTtsVoice: 'synthetic-a-local' }, onSpeechSelectionChange: () => undefined, onSpeechNetworkFallbackChange: () => undefined })
    try {
      expect(phone.container.textContent).toContain('recognition_retirement_failed')
      expect(desktop.container.querySelector('input')).toBeNull()
      expect(desktop.container.querySelector('select')).toBeNull()
    } finally { phone.dispose(); desktop.dispose() }
  })
  it.each(['cpu', 'cuda'])('shows native engine identity independently of %s execution', (provider) => {
    const { container, dispose } = renderSpeech({ android: false, androidStatus: null, statusError: null, startupState: startup(identity, provider) })
    try {
      expect(container.textContent).toContain('Configured engine: Parakeet')
      expect(container.textContent).toContain('Runtime engine: Parakeet')
      expect(container.textContent).toContain('Configured engine: Piper ONNX')
      expect(container.textContent).toContain('Runtime engine: Piper ONNX')
      expect(container.textContent).toContain(`Reported execution provider: ${provider.toUpperCase()}`)
      expect(container.textContent).toContain('Model identity not reported')
      expect(container.textContent).not.toMatch(/Kokoro|PocketTTS|Local synthesis/)
    } finally { dispose() }
  })

  it('labels configured identity without claiming an unloaded engine is selected', () => {
    const configured = { ...identity, tts: { ...identity.tts, runtime_engine: null, configured_model: 'Synthetic model', configured_voice: 'Synthetic voice', configured_locale: 'en-US' } }
    const { container, dispose } = renderSpeech({ android: false, androidStatus: null, statusError: null, startupState: startup(configured, null) })
    try {
      expect(container.textContent).toContain('Configured engine: Piper ONNX')
      expect(container.textContent).toContain('Runtime engine: Not loaded')
      expect(container.textContent).toContain('Configured model: Synthetic model')
      expect(container.textContent).toContain('Configured voice: Synthetic voice')
      expect(container.textContent).toContain('Configured locale: en-US')
    } finally { dispose() }
  })

  it('does not infer engine identity from a legacy CPU/CUDA capability', () => {
    const { container, dispose } = renderSpeech({ android: false, androidStatus: null, statusError: null, startupState: startup() })
    try {
      expect(container.textContent).toContain('Engine identity not reported')
      expect(container.textContent).not.toMatch(/Configured engine:|Runtime engine:|Kokoro|PocketTTS|Piper|Parakeet/)
    } finally { dispose() }
  })

  it('preserves loaded identity while warming without treating CUDA as the engine', () => {
    const state = startup(identity)
    if (state.kind !== 'ready') throw new Error('Synthetic startup must be ready')
    const { container, dispose } = renderSpeech({ android: false, androidStatus: null, statusError: null, startupState: {
      ...state,
      kind: 'warming_model',
      message: 'Synthetic response model warming',
      capabilities: state.capabilities.map((capability) => capability.id === 'tts' ? { ...capability, state: 'warming', reason: 'Synthetic TTS warming' } : capability),
    } })
    try {
      expect(container.textContent).toContain('warming · Synthetic TTS warming')
      expect(container.textContent).toContain('Runtime engine: Piper ONNX')
      expect(container.textContent).toContain('Reported execution provider: CUDA')
      expect(container.textContent).not.toContain('Runtime engine: CUDA')
    } finally { dispose() }
  })

  it('does not fabricate a default engine, locale or voice when native metadata is unconfigured', () => {
    const unconfigured = { configured_engine: null, runtime_engine: null, configured_model: null, configured_voice: null, configured_locale: null }
    const { container, dispose } = renderSpeech({ android: false, androidStatus: null, statusError: null, startupState: startup({ stt: unconfigured, tts: unconfigured }, null) })
    try {
      expect(container.textContent).toContain('Configured engine: Not configured')
      expect(container.textContent).toContain('Runtime engine: Not loaded')
      expect(container.textContent).toContain('Locale not reported')
      expect(container.textContent).toContain('Voice identity not reported')
      expect(container.textContent).not.toMatch(/Piper|Parakeet|en-US|Kokoro|PocketTTS/)
    } finally { dispose() }
  })

  it('preserves the native Android route rather than applying desktop engines', () => {
    const androidStatus = parseAndroidVoiceStatus({ kind: 'voice_status', available: true, mode: 'dictation', dictation: 'idle', loop: 'off', error: null, reason: 'Synthetic recognizer ready', ttsPhase: 'idle', ttsAvailable: true, ttsEnabled: true, recognitionRoute: 'Synthetic recognizer · en-US · route not reported', ttsRoute: 'Synthetic Android engine · Synthetic voice · en-US' })
    const { container, dispose } = renderSpeech({ android: true, androidStatus, statusError: null, startupState: startup(identity) })
    try {
      expect(container.textContent).toContain(androidStatus.recognitionRoute)
      expect(container.textContent).toContain(androidStatus.ttsRoute)
      expect(container.textContent).not.toMatch(/Piper|Parakeet|Configured engine/)
    } finally { dispose() }
  })

  it('renders missing speech services notice with Play Store action when speech services are missing', () => {
    const androidStatus = voiceStatus({ available: false, missingSpeechServices: true, speechRequirementMessage: 'Google speech services required for testing' })
    const { container, dispose } = renderSpeech({ android: true, androidStatus, statusError: null, startupState: startup(identity) })
    try {
      const notice = container.querySelector('.settings-panel__missing-speech')
      expect(notice).not.toBeNull()
      expect(notice?.textContent).toContain('Google speech services required for testing')
      const button = notice?.querySelector('button')
      expect(button?.textContent).toBe('Open Play Store')
    } finally { dispose() }
  })
})

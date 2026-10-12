import { isRecord, utf8ByteLength } from './wire'
import { parseSpeechSelectionString } from './assistantSettings'
import type { SpeechSelection } from './assistantSettings'

export interface AndroidVoiceStatus extends SpeechSelection {
  readonly available: boolean
  readonly reason: string
  readonly mode: 'dictation' | 'loop' | 'shortcut' | 'ask'
  readonly dictation: string
  readonly readyForSpeech?: boolean
  readonly loop: string
  readonly ttsEnabled: boolean
  readonly ttsAvailable: boolean
  readonly ttsPhase: string
  readonly error: string | null
  readonly shortcutPhase: string | null
  readonly recognitionRoute?: string | null
  readonly ttsRoute?: string | null
  readonly localOnlyRecognitionAvailable?: boolean
  readonly recognitionMetadata?: AndroidSpeechMetadata | null
  readonly ttsMetadata?: AndroidSpeechMetadata | null
  readonly diagnosticError?: string | null
  readonly speechOptions?: AndroidSpeechOptions | null
}
export interface AndroidSpeechVoiceOption {
  readonly provider: string
  readonly name: string
  readonly language: string
  readonly networkRequired: boolean
}
export interface AndroidSpeechOptions {
  readonly ttsProviders: readonly string[]
  readonly ttsVoices: readonly AndroidSpeechVoiceOption[]
  readonly recognitionInstalledLanguages: readonly string[] | null
  readonly recognitionSupportedLanguages: readonly string[] | null
  readonly localOnlyRecognitionAvailable: boolean
  readonly modelSelectionAvailable: false
  readonly modelIdentifiers: null
}
export interface AndroidSpeechMetadata {
  readonly requestedLanguage: string
  readonly selectedLanguage: string | null
  readonly reportedLanguage: string | null
  readonly installedLanguages: readonly string[] | null
  readonly supportedLanguages?: readonly string[] | null
  readonly actualProcessing: 'on_device_api' | 'not_reported'
  readonly model: string | null
  readonly offlineTrialVerified: boolean
  readonly fallbackReason: string | null
  readonly configuredProvider?: string | null
  readonly selectedProvider?: string | null
  readonly reportedProvider?: string | null
  readonly selectedVoice?: string | null
  readonly networkRequired?: boolean
  readonly localPreferred?: boolean
}
export interface AndroidVoiceContext { readonly sessionId: string; readonly selectedHead: string | null }
export type AndroidVoiceEvent =
  | { readonly sessionId: string; readonly kind: 'dictation'; readonly phase: string; readonly draft: string }
  | { readonly sessionId: string; readonly kind: 'submit'; readonly requestId: string; readonly text: string; readonly context?: AndroidVoiceContext }
  | { readonly sessionId: string; readonly kind: 'loop'; readonly phase: string; readonly error: string | null }
  | { readonly sessionId: string; readonly kind: 'voice_status'; readonly status: AndroidVoiceStatus }
  | { readonly sessionId: string; readonly kind: 'shortcut_ready' }
  | { readonly sessionId: string; readonly kind: 'shortcut_review'; readonly draft: string; readonly reason: string }

function text(value: unknown, bytes: number, empty = true): string {
  if (typeof value !== 'string' || utf8ByteLength(value) > bytes || (!empty && value.trim() === '')) throw new Error('Native voice value is invalid')
  return value
}
export function parseAndroidVoiceStatus(value: unknown): AndroidVoiceStatus {
  if (!isRecord(value) || value['kind'] !== 'voice_status' || typeof value['available'] !== 'boolean' || typeof value['ttsEnabled'] !== 'boolean' || typeof value['ttsAvailable'] !== 'boolean' || !['dictation', 'loop', 'shortcut', 'ask'].includes(value['mode'] as string)) throw new Error('Native voice status is invalid')
  if (value['readyForSpeech'] !== undefined && typeof value['readyForSpeech'] !== 'boolean') throw new Error('Native voice status is invalid')
  if (value['localOnlyRecognitionAvailable'] !== undefined && typeof value['localOnlyRecognitionAvailable'] !== 'boolean') throw new Error('Native speech fallback capability is invalid')
  return { available: value['available'], reason: text(value['reason'], 512), mode: value['mode'] as AndroidVoiceStatus['mode'],
    speechLanguage: parseSpeechSelectionString(value, 'speechLanguage'), speechTtsProvider: parseSpeechSelectionString(value, 'speechTtsProvider'), speechTtsVoice: parseSpeechSelectionString(value, 'speechTtsVoice'),
    speechOptions: parseSpeechOptions(value['speechOptions']),
    localOnlyRecognitionAvailable: value['localOnlyRecognitionAvailable'] === true,
    recognitionMetadata: parseSpeechMetadata(value['recognitionMetadata']), ttsMetadata: parseSpeechMetadata(value['ttsMetadata']),
    diagnosticError: value['diagnosticError'] === undefined || value['diagnosticError'] === null ? null : text(value['diagnosticError'], 512),
    dictation: text(value['dictation'], 32), ...(value['readyForSpeech'] === undefined ? {} : { readyForSpeech: value['readyForSpeech'] }), loop: text(value['loop'], 32), ttsEnabled: value['ttsEnabled'], ttsAvailable: value['ttsAvailable'], ttsPhase: text(value['ttsPhase'], 32),
    error: value['error'] === null ? null : text(value['error'], 512), shortcutPhase: value['shortcutPhase'] === undefined || value['shortcutPhase'] === null ? null : text(value['shortcutPhase'], 32),
    recognitionRoute: value['recognitionRoute'] === undefined || value['recognitionRoute'] === null ? null : text(value['recognitionRoute'], 512, false),
    ttsRoute: value['ttsRoute'] === undefined || value['ttsRoute'] === null ? null : text(value['ttsRoute'], 512, false) }
}

function languages(value: unknown): readonly string[] | null {
  if (value === null) return null
  if (!Array.isArray(value) || value.length > 128) throw new Error('Native speech language inventory is invalid')
  return Array.from(value, (language) => text(language, 512))
}

function parseSpeechOptions(value: unknown): AndroidSpeechOptions | null {
  if (value === undefined) return null
  if (!isRecord(value) || !Array.isArray(value['ttsProviders']) || value['ttsProviders'].length > 8 || !Array.isArray(value['ttsVoices']) || value['ttsVoices'].length > 1024 || typeof value['localOnlyRecognitionAvailable'] !== 'boolean' || value['modelSelectionAvailable'] !== false || value['modelIdentifiers'] !== null) throw new Error('Native speech options are invalid or unsupported')
  return { ttsProviders: Array.from(value['ttsProviders'], (provider) => text(provider, 512)),
    ttsVoices: Array.from(value['ttsVoices'], (voice): AndroidSpeechVoiceOption => {
      if (!isRecord(voice) || typeof voice['networkRequired'] !== 'boolean') throw new Error('Native speech voice inventory is invalid')
      return { provider: text(voice['provider'], 512), name: text(voice['name'], 512), language: text(voice['language'], 512), networkRequired: voice['networkRequired'] }
    }), recognitionInstalledLanguages: languages(value['recognitionInstalledLanguages']), recognitionSupportedLanguages: languages(value['recognitionSupportedLanguages']),
    localOnlyRecognitionAvailable: value['localOnlyRecognitionAvailable'], modelSelectionAvailable: false, modelIdentifiers: null }
}

function parseSpeechMetadata(value: unknown): AndroidSpeechMetadata | null {
  if (value === undefined || value === null) return null
  if (!isRecord(value)) throw new Error('Native speech metadata is invalid')
  const nullableText = (key: string): string | null => value[key] === null ? null : text(value[key], 512, false)
  const installed = value['installedLanguages']
  if (installed !== null && (!Array.isArray(installed) || installed.length > 128)) throw new Error('Native installed speech languages are invalid')
  const actualProcessing = value['actualProcessing']
  if (actualProcessing !== 'on_device_api' && actualProcessing !== 'not_reported') throw new Error('Native speech processing evidence is invalid')
  if (typeof value['offlineTrialVerified'] !== 'boolean') throw new Error('Native offline speech evidence is invalid')
  const identities: { configuredProvider?: string | null; selectedProvider?: string | null; reportedProvider?: string | null; selectedVoice?: string | null; networkRequired?: boolean; localPreferred?: boolean } = {}
  for (const key of ['configuredProvider', 'selectedProvider', 'reportedProvider', 'selectedVoice'] as const) {
    if (Object.prototype.hasOwnProperty.call(value, key)) identities[key] = nullableText(key)
  }
  for (const key of ['networkRequired', 'localPreferred'] as const) {
    if (Object.prototype.hasOwnProperty.call(value, key)) {
      if (typeof value[key] !== 'boolean') throw new Error('Native speech routing preference is invalid')
      identities[key] = value[key]
    }
  }
  return { ...identities, requestedLanguage: text(value['requestedLanguage'], 128, false),
    selectedLanguage: nullableText('selectedLanguage'), reportedLanguage: nullableText('reportedLanguage'),
    installedLanguages: installed === null ? null : Array.from(installed as unknown[], (language) => text(language, 128, false)),
    supportedLanguages: value['supportedLanguages'] === undefined ? null : languages(value['supportedLanguages']),
    actualProcessing, model: nullableText('model'), offlineTrialVerified: value['offlineTrialVerified'], fallbackReason: nullableText('fallbackReason') }
}

export function visibleSpeechError(error: string | null | undefined): string | null {
  return error === 'recognition_error_7' || error === 'no_speech' || error === 'speech-recognizer-7' || error === 'no-speech-detected' ? null : error ?? null
}
export function parseAndroidVoiceEvent(value: unknown): AndroidVoiceEvent {
  if (!isRecord(value)) throw new Error('Native voice event is invalid')
  const sessionId = text(value['sessionId'], 128, false)
  if (value['kind'] === 'voice_status') return { sessionId, kind: 'voice_status', status: parseAndroidVoiceStatus(value) }
  if (value['kind'] === 'dictation') return { sessionId, kind: 'dictation', phase: text(value['phase'], 32), draft: text(value['draft'], 32 * 1024) }
  if (value['kind'] === 'submit') {
    const context = value['context']
    if (context !== undefined && (!isRecord(context) || context['selectedHead'] !== null && typeof context['selectedHead'] !== 'string')) throw new Error('Native voice context is invalid')
    return { sessionId, kind: 'submit', requestId: text(value['requestId'], 128, false), text: text(value['text'], 32 * 1024, false),
      ...(isRecord(context) ? { context: { sessionId: text(context['sessionId'], 128, false), selectedHead: context['selectedHead'] === null ? null : text(context['selectedHead'], 128, false) } } : {}) }
  }
  if (value['kind'] === 'loop') return { sessionId, kind: 'loop', phase: text(value['phase'], 32), error: value['error'] === null ? null : text(value['error'], 512) }
  if (value['kind'] === 'shortcut_ready') return { sessionId, kind: 'shortcut_ready' }
  if (value['kind'] === 'shortcut_review') return { sessionId, kind: 'shortcut_review', draft: text(value['draft'], 32 * 1024), reason: text(value['reason'], 512) }
  throw new Error('Native voice event kind is unsupported')
}

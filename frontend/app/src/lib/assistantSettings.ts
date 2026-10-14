import { DEFAULT_MODEL_CHOICES, modelLabels } from './modelCatalog'
import type { AgentChoice, InstantChoice } from './modelCatalog'
export type { InstantChoice } from './modelCatalog'
export type DeepChoice = AgentChoice
export type ReviewChoice = DeepChoice

export interface SpeechSelection {
  readonly speechLanguage: string
  readonly speechTtsProvider: string
  readonly speechTtsVoice: string
}

export const DEFAULT_SPEECH_SELECTION: SpeechSelection = { speechLanguage: '', speechTtsProvider: '', speechTtsVoice: '' }

export interface AssistantSettings extends SpeechSelection {
  instant: InstantChoice
  deep: DeepChoice
  review: ReviewChoice
  deepEnabled: boolean
  reviewEnabled: boolean
  prefetch: boolean
  completion: boolean
  webEnabled: boolean
  speechNetworkFallback: boolean
}

export interface AssistantCapabilities {
  localFast: boolean
  localQuality: boolean
  custom: boolean
  openCode: boolean
  qwenPrediction: boolean
  deep: boolean
  review: boolean
}

export interface AssistantOption<T extends string> {
  value: T
  label: string
  available: boolean
  reason?: string
  description?: string
}

export const DEFAULT_ASSISTANT_SETTINGS: Readonly<AssistantSettings> = {
  ...DEFAULT_SPEECH_SELECTION,
  ...DEFAULT_MODEL_CHOICES,
  deepEnabled: false, reviewEnabled: false, prefetch: false, completion: true, webEnabled: true, speechNetworkFallback: true,
}

const instantLabels = modelLabels('instant')
const deepLabels = modelLabels('deep')
const reviewLabels = modelLabels('review')

function options<T extends string>(labels: Record<T, string>, capabilities: AssistantCapabilities): AssistantOption<T>[] {
  const isAndroid = import.meta.env['VITE_VOXGOLEM_RUNTIME'] === 'android'
  return (Object.keys(labels) as T[])
    .filter((value) => isAndroid || value !== 'gemini-nano')
    .map((value) => {
    const provider = value.startsWith('local') || value === 'none' || value === 'gemini-nano' ? 'local' : value.startsWith('custom') ? 'custom' : value.startsWith('agy') ? 'agy' : 'openCode'
    const available = value === 'local-fast' ? capabilities.localFast : value === 'local-quality' ? capabilities.localQuality : value === 'gemini-nano' ? isAndroid : provider === 'custom' ? capabilities.custom : provider === 'agy' ? true : capabilities.openCode
    const providerName = provider === 'agy' ? 'Agy' : provider === 'openCode' ? 'OpenCode' : provider === 'custom' ? 'Custom' : 'Local'
    const description = value === 'custom-sol-none' || value === 'custom-luna-none' ? 'Direct Custom connection; reasoning disabled' : value === 'gemini-nano' ? 'Pixel 8 on-device model' : value.startsWith('agy') ? 'Google Antigravity search' : value.startsWith('opencode') ? 'OpenCode search' : value.startsWith('custom') ? 'Custom reasoning-only' : undefined
    const featureUnavailable = labels === deepLabels ? !capabilities.deep : labels === reviewLabels ? !capabilities.review : false
    return { value, label: labels[value], available: available && !featureUnavailable, ...(description ? { description } : {}), ...(available && !featureUnavailable ? {} : { reason: featureUnavailable ? `${providerName} does not support this option` : `${providerName} is unavailable` }) }
  })
}

export const instantOptions = (capabilities: AssistantCapabilities): AssistantOption<InstantChoice>[] => options(instantLabels, capabilities)
export const deepOptions = (capabilities: AssistantCapabilities): AssistantOption<DeepChoice>[] => options(deepLabels, capabilities)
export const reviewOptions = (capabilities: AssistantCapabilities): AssistantOption<ReviewChoice>[] => options(reviewLabels, capabilities)

const instantValues = new Set(Object.keys(instantLabels))
const deepValues = new Set(Object.keys(deepLabels))
const reviewValues = new Set(Object.keys(reviewLabels))
function stringField(payload: Record<string, unknown>, key: string): string {
  if (typeof payload[key] !== 'string') throw new Error(`Invalid assistant settings: ${key}`)
  return payload[key] as string
}

export function parseAssistantSettings(payload: unknown): AssistantSettings {
  if (!isRecord(payload)) throw new Error('Invalid assistant settings payload')
  const p = payload
  const instant = stringField(p, 'instant'); const deep = stringField(p, 'deep'); const review = stringField(p, 'review')
  if (!instantValues.has(instant) || !deepValues.has(deep) || !reviewValues.has(review) || typeof p['deep_enabled'] !== 'boolean' || typeof p['review_enabled'] !== 'boolean' || typeof p['prefetch'] !== 'boolean' || typeof p['completion'] !== 'boolean') throw new Error('Invalid assistant settings payload')
  return { instant: instant as InstantChoice, deep: deep as DeepChoice, review: review as ReviewChoice, deepEnabled: p['deep_enabled'] as boolean, reviewEnabled: p['review_enabled'] as boolean, prefetch: p['prefetch'] as boolean, completion: p['completion'] as boolean, webEnabled: parsePolicyBoolean(p, 'web_enabled'), speechNetworkFallback: parsePolicyBoolean(p, 'speech_network_fallback'), speechLanguage: parseSpeechSelectionString(p, 'speech_language'), speechTtsProvider: parseSpeechSelectionString(p, 'speech_tts_provider'), speechTtsVoice: parseSpeechSelectionString(p, 'speech_tts_voice') }
}

export function serializeAssistantSettings(settings: AssistantSettings): Record<string, unknown> {
  return { instant: settings.instant, deep: settings.deep, review: settings.review, deep_enabled: settings.deepEnabled, review_enabled: settings.reviewEnabled, prefetch: settings.prefetch, completion: settings.completion, web_enabled: settings.webEnabled, speech_network_fallback: settings.speechNetworkFallback, speech_language: settings.speechLanguage, speech_tts_provider: settings.speechTtsProvider, speech_tts_voice: settings.speechTtsVoice }
}
export function parsePolicyBoolean(payload: Record<string, unknown>, key: 'web_enabled' | 'speech_network_fallback'): boolean {
  if (!Object.prototype.hasOwnProperty.call(payload, key)) return true
  if (typeof payload[key] !== 'boolean') throw new Error(`Preference ${key} must be boolean`)
  return payload[key]
}
export function parseSpeechSelectionString(payload: Record<string, unknown>, key: string): string {
  if (!Object.prototype.hasOwnProperty.call(payload, key)) return ''
  const value = payload[key]
  if (typeof value !== 'string' || utf8ByteLength(value) > 512) throw new Error(`Speech preference ${key} must be a string within 512 UTF-8 bytes`)
  return value
}
import { isRecord, utf8ByteLength } from './wire'

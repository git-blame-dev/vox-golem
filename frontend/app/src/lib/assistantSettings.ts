/** The deliberately small, stable vocabulary shared by the settings UI and Tauri. */
export type InstantChoice = 'local-fast' | 'local-quality' | 'custom-sol-high' | 'custom-luna-low' | 'opencode-sol-high' | 'opencode-luna-low'
export type DeepChoice = 'custom-sol-high' | 'custom-luna-low' | 'opencode-sol-high' | 'opencode-luna-low'
export type ReviewChoice = DeepChoice

export interface AssistantSettings {
  instant: InstantChoice
  deep: DeepChoice
  review: ReviewChoice
  deepEnabled: boolean
  reviewEnabled: boolean
  prefetch: boolean
  completion: boolean
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
  instant: 'local-fast', deep: 'opencode-sol-high', review: 'opencode-sol-high',
  deepEnabled: false, reviewEnabled: false, prefetch: false, completion: true,
}

const instantLabels: Record<InstantChoice, string> = {
  'local-fast': 'Local: Fast', 'local-quality': 'Local: Quality',
  'custom-sol-high': 'Custom: GPT-5.6 Sol High', 'custom-luna-low': 'Custom: GPT-5.6 Luna Low',
  'opencode-sol-high': 'OpenCode: GPT-5.6 Sol High', 'opencode-luna-low': 'OpenCode: GPT-5.6 Luna Low',
}
const deepLabels: Record<DeepChoice, string> = {
  'custom-sol-high': 'Deep: Custom: GPT-5.6 Sol High', 'custom-luna-low': 'Deep: Custom: GPT-5.6 Luna Low',
  'opencode-sol-high': 'Deep: OpenCode: GPT-5.6 Sol High', 'opencode-luna-low': 'Deep: OpenCode: GPT-5.6 Luna Low',
}
const reviewLabels: Record<ReviewChoice, string> = {
  'custom-sol-high': 'Review: Custom: GPT-5.6 Sol High', 'custom-luna-low': 'Review: Custom: GPT-5.6 Luna Low',
  'opencode-sol-high': 'Review: OpenCode: GPT-5.6 Sol High', 'opencode-luna-low': 'Review: OpenCode: GPT-5.6 Luna Low',
}

function options<T extends string>(labels: Record<T, string>, capabilities: AssistantCapabilities): AssistantOption<T>[] {
  return (Object.keys(labels) as T[]).map((value) => {
    const provider = value.startsWith('local') || value === 'none' ? 'local' : value.startsWith('custom') ? 'custom' : 'openCode'
    const available = value === 'local-fast' ? capabilities.localFast : value === 'local-quality' ? capabilities.localQuality : provider === 'custom' ? capabilities.custom : capabilities.openCode
    const providerName = provider === 'openCode' ? 'OpenCode' : provider === 'custom' ? 'Custom' : 'Local'
    const description = value.startsWith('opencode') ? 'OpenCode search' : value.startsWith('custom') ? 'Custom reasoning-only' : undefined
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
  if (!payload || typeof payload !== 'object' || Array.isArray(payload)) throw new Error('Invalid assistant settings payload')
  const p = payload as Record<string, unknown>
  const instant = stringField(p, 'instant'); const deep = stringField(p, 'deep'); const review = stringField(p, 'review')
  if (!instantValues.has(instant) || !deepValues.has(deep) || !reviewValues.has(review) || typeof p['deep_enabled'] !== 'boolean' || typeof p['review_enabled'] !== 'boolean' || typeof p['prefetch'] !== 'boolean' || typeof p['completion'] !== 'boolean') throw new Error('Invalid assistant settings payload')
  return { instant: instant as InstantChoice, deep: deep as DeepChoice, review: review as ReviewChoice, deepEnabled: p['deep_enabled'] as boolean, reviewEnabled: p['review_enabled'] as boolean, prefetch: p['prefetch'] as boolean, completion: p['completion'] as boolean }
}

export function serializeAssistantSettings(settings: AssistantSettings): Record<string, unknown> {
  return { instant: settings.instant, deep: settings.deep, review: settings.review, deep_enabled: settings.deepEnabled, review_enabled: settings.reviewEnabled, prefetch: settings.prefetch, completion: settings.completion }
}

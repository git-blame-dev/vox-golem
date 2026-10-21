import { MODEL_CATALOG, DEFAULT_MODEL_CHOICES } from './modelCatalog.generated'
export { MODEL_CATALOG, DEFAULT_MODEL_CHOICES }

/** Stable fallback metadata; never overrides native labels or availability. */
export interface ModelCatalogEntry {
  readonly id: string
  readonly label: string
  readonly agent: boolean
  readonly preview?: boolean
  readonly familyId: string
  readonly familyLabel: string
  readonly shortFamilyLabel: string
  readonly effort: 'none' | 'low' | 'medium' | 'high' | 'xhigh' | 'max'
}

export type InstantChoice = typeof MODEL_CATALOG[number]['id']
export type AgentChoice = Extract<typeof MODEL_CATALOG[number], { readonly agent: true }>['id']
export const INSTANT_CHOICES: readonly InstantChoice[] = MODEL_CATALOG.map((model) => model.id)
export const AGENT_CHOICES: readonly AgentChoice[] = MODEL_CATALOG.filter(
  (model) => model.agent,
).map((model) => model.id)

export function modelLabels(stage: 'instant'): Record<InstantChoice, string>
export function modelLabels(stage: 'deep' | 'review'): Record<AgentChoice, string>
export function modelLabels(stage: 'instant' | 'deep' | 'review'): Record<string, string> {
  const prefix = stage === 'instant' ? '' : stage === 'deep' ? 'Deep: ' : 'Review: '
  return Object.fromEntries(MODEL_CATALOG.filter((model) => stage === 'instant' || model.agent)
    .map((model) => [model.id, `${prefix}${model.label}`]))
}

export function browserModelOptions() {
  const isAndroid = import.meta.env['VITE_VOXGOLEM_RUNTIME'] === 'android'
  return MODEL_CATALOG.filter((model) => isAndroid || model.id !== 'gemini-nano').map((model) => ({
    id: model.id,
    label: model.preview ? `${model.label} (browser preview)` : model.label,
    available: model.preview,
    unavailable_reason: model.preview ? null : 'Unavailable in browser preview',
  }))
}

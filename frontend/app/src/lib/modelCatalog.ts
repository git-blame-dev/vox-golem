/** Stable routing IDs; these defaults never override native labels or availability. */
export const MODEL_CATALOG = [
  { id: 'local-fast', label: 'Local: Fast', agent: false, preview: true },
  { id: 'local-quality', label: 'Local: Quality', agent: false, preview: false },
  { id: 'custom-sol-high', label: 'Custom: GPT-6.1 Sol High', agent: true, preview: false },
  { id: 'custom-sol-none', label: 'Custom: GPT-6.1 Sol No Reasoning', agent: false, preview: false },
  { id: 'custom-luna-none', label: 'Custom: GPT-6 Luna No Reasoning', agent: false, preview: false },
  { id: 'custom-luna-low', label: 'Custom: GPT-5.6 Luna Low', agent: true, preview: false },
  { id: 'opencode-sol-high', label: 'OpenCode: GPT-6.1 Sol High', agent: true, preview: false },
  { id: 'opencode-luna-low', label: 'OpenCode: GPT-5.6 Luna Low', agent: true, preview: false },
] as const

export type InstantChoice = typeof MODEL_CATALOG[number]['id']
export type AgentChoice = Extract<typeof MODEL_CATALOG[number], { readonly agent: true }>['id']
export const INSTANT_CHOICES: readonly InstantChoice[] = MODEL_CATALOG.map((model) => model.id)
export const AGENT_CHOICES: readonly AgentChoice[] = MODEL_CATALOG.filter(
  (model): model is Extract<typeof MODEL_CATALOG[number], { readonly agent: true }> => model.agent,
).map((model) => model.id)
export const DEFAULT_MODEL_CHOICES = { instant: 'local-fast', deep: 'opencode-sol-high', review: 'opencode-sol-high' } as const

export function modelLabels(stage: 'instant'): Record<InstantChoice, string>
export function modelLabels(stage: 'deep' | 'review'): Record<AgentChoice, string>
export function modelLabels(stage: 'instant' | 'deep' | 'review'): Record<string, string> {
  const prefix = stage === 'instant' ? '' : stage === 'deep' ? 'Deep: ' : 'Review: '
  return Object.fromEntries(MODEL_CATALOG.filter((model) => stage === 'instant' || model.agent)
    .map((model) => [model.id, `${prefix}${model.label}`]))
}

export function browserModelOptions() {
  return MODEL_CATALOG.map((model) => ({
    id: model.id,
    label: model.preview ? `${model.label} (browser preview)` : model.label,
    available: model.preview,
    unavailable_reason: model.preview ? null : 'Unavailable in browser preview',
  }))
}

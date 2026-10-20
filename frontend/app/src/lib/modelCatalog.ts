/** Stable routing IDs; these defaults never override native labels or availability. */
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

export const MODEL_CATALOG: readonly ModelCatalogEntry[] = [
  // Local / On-device
  { id: 'local-fast', label: 'Local: Llama 3.2 3B', agent: false, preview: true, familyId: 'local-fast', familyLabel: 'Local: Llama 3.2 3B', shortFamilyLabel: 'Llama 3.2 3B', effort: 'none' },
  { id: 'local-quality', label: 'Local: Quality', agent: false, preview: false, familyId: 'local-quality', familyLabel: 'Local: Quality', shortFamilyLabel: 'Quality', effort: 'none' },
  { id: 'gemini-nano', label: 'Local: Gemini Nano', agent: false, preview: false, familyId: 'gemini-nano', familyLabel: 'Local: Gemini Nano', shortFamilyLabel: 'Gemini Nano', effort: 'none' },

  // Custom: GPT-6.1 Sol
  { id: 'custom-sol-low', label: 'Custom: GPT-6.1 Sol Low', agent: true, preview: false, familyId: 'custom-sol', familyLabel: 'Custom: GPT-6.1 Sol', shortFamilyLabel: 'GPT-6.1 Sol', effort: 'low' },
  { id: 'custom-sol-medium', label: 'Custom: GPT-6.1 Sol Medium', agent: true, preview: false, familyId: 'custom-sol', familyLabel: 'Custom: GPT-6.1 Sol', shortFamilyLabel: 'GPT-6.1 Sol', effort: 'medium' },
  { id: 'custom-sol-high', label: 'Custom: GPT-6.1 Sol High', agent: true, preview: false, familyId: 'custom-sol', familyLabel: 'Custom: GPT-6.1 Sol', shortFamilyLabel: 'GPT-6.1 Sol', effort: 'high' },
  { id: 'custom-sol-xhigh', label: 'Custom: GPT-6.1 Sol X-High', agent: true, preview: false, familyId: 'custom-sol', familyLabel: 'Custom: GPT-6.1 Sol', shortFamilyLabel: 'GPT-6.1 Sol', effort: 'xhigh' },
  { id: 'custom-sol-max', label: 'Custom: GPT-6.1 Sol Max', agent: true, preview: false, familyId: 'custom-sol', familyLabel: 'Custom: GPT-6.1 Sol', shortFamilyLabel: 'GPT-6.1 Sol', effort: 'max' },

  // Custom: GPT-6 Luna
  { id: 'custom-luna-none', label: 'Custom: GPT-6 Luna No Reasoning', agent: false, preview: false, familyId: 'custom-luna6', familyLabel: 'Custom: GPT-6 Luna', shortFamilyLabel: 'GPT-6 Luna', effort: 'none' },
  { id: 'custom-luna6-low', label: 'Custom: GPT-6 Luna Low', agent: true, preview: false, familyId: 'custom-luna6', familyLabel: 'Custom: GPT-6 Luna', shortFamilyLabel: 'GPT-6 Luna', effort: 'low' },
  { id: 'custom-luna6-medium', label: 'Custom: GPT-6 Luna Medium', agent: true, preview: false, familyId: 'custom-luna6', familyLabel: 'Custom: GPT-6 Luna', shortFamilyLabel: 'GPT-6 Luna', effort: 'medium' },
  { id: 'custom-luna6-high', label: 'Custom: GPT-6 Luna High', agent: true, preview: false, familyId: 'custom-luna6', familyLabel: 'Custom: GPT-6 Luna', shortFamilyLabel: 'GPT-6 Luna', effort: 'high' },
  { id: 'custom-luna6-xhigh', label: 'Custom: GPT-6 Luna X-High', agent: true, preview: false, familyId: 'custom-luna6', familyLabel: 'Custom: GPT-6 Luna', shortFamilyLabel: 'GPT-6 Luna', effort: 'xhigh' },
  { id: 'custom-luna6-max', label: 'Custom: GPT-6 Luna Max', agent: true, preview: false, familyId: 'custom-luna6', familyLabel: 'Custom: GPT-6 Luna', shortFamilyLabel: 'GPT-6 Luna', effort: 'max' },

  // Custom: GPT-5.6 Luna
  { id: 'custom-luna56-none', label: 'Custom: GPT-5.6 Luna No Reasoning', agent: false, preview: false, familyId: 'custom-luna56', familyLabel: 'Custom: GPT-5.6 Luna', shortFamilyLabel: 'GPT-5.6 Luna', effort: 'none' },
  { id: 'custom-luna-low', label: 'Custom: GPT-5.6 Luna Low', agent: true, preview: false, familyId: 'custom-luna56', familyLabel: 'Custom: GPT-5.6 Luna', shortFamilyLabel: 'GPT-5.6 Luna', effort: 'low' },
  { id: 'custom-luna56-medium', label: 'Custom: GPT-5.6 Luna Medium', agent: true, preview: false, familyId: 'custom-luna56', familyLabel: 'Custom: GPT-5.6 Luna', shortFamilyLabel: 'GPT-5.6 Luna', effort: 'medium' },
  { id: 'custom-luna56-high', label: 'Custom: GPT-5.6 Luna High', agent: true, preview: false, familyId: 'custom-luna56', familyLabel: 'Custom: GPT-5.6 Luna', shortFamilyLabel: 'GPT-5.6 Luna', effort: 'high' },
  { id: 'custom-luna56-xhigh', label: 'Custom: GPT-5.6 Luna X-High', agent: true, preview: false, familyId: 'custom-luna56', familyLabel: 'Custom: GPT-5.6 Luna', shortFamilyLabel: 'GPT-5.6 Luna', effort: 'xhigh' },
  { id: 'custom-luna56-max', label: 'Custom: GPT-5.6 Luna Max', agent: true, preview: false, familyId: 'custom-luna56', familyLabel: 'Custom: GPT-5.6 Luna', shortFamilyLabel: 'GPT-5.6 Luna', effort: 'max' },

  // OpenCode: GPT-6.1 Sol
  { id: 'opencode-sol-low', label: 'OpenCode: GPT-6.1 Sol Low', agent: true, preview: false, familyId: 'opencode-sol', familyLabel: 'OpenCode: GPT-6.1 Sol', shortFamilyLabel: 'GPT-6.1 Sol', effort: 'low' },
  { id: 'opencode-sol-medium', label: 'OpenCode: GPT-6.1 Sol Medium', agent: true, preview: false, familyId: 'opencode-sol', familyLabel: 'OpenCode: GPT-6.1 Sol', shortFamilyLabel: 'GPT-6.1 Sol', effort: 'medium' },
  { id: 'opencode-sol-high', label: 'OpenCode: GPT-6.1 Sol High', agent: true, preview: false, familyId: 'opencode-sol', familyLabel: 'OpenCode: GPT-6.1 Sol', shortFamilyLabel: 'GPT-6.1 Sol', effort: 'high' },
  { id: 'opencode-sol-xhigh', label: 'OpenCode: GPT-6.1 Sol X-High', agent: true, preview: false, familyId: 'opencode-sol', familyLabel: 'OpenCode: GPT-6.1 Sol', shortFamilyLabel: 'GPT-6.1 Sol', effort: 'xhigh' },
  { id: 'opencode-sol-max', label: 'OpenCode: GPT-6.1 Sol Max', agent: true, preview: false, familyId: 'opencode-sol', familyLabel: 'OpenCode: GPT-6.1 Sol', shortFamilyLabel: 'GPT-6.1 Sol', effort: 'max' },

  // OpenCode: GPT-5.6 Luna
  { id: 'opencode-luna-none', label: 'OpenCode: GPT-5.6 Luna No Reasoning', agent: false, preview: false, familyId: 'opencode-luna', familyLabel: 'OpenCode: GPT-5.6 Luna', shortFamilyLabel: 'GPT-5.6 Luna', effort: 'none' },
  { id: 'opencode-luna-low', label: 'OpenCode: GPT-5.6 Luna Low', agent: true, preview: false, familyId: 'opencode-luna', familyLabel: 'OpenCode: GPT-5.6 Luna', shortFamilyLabel: 'GPT-5.6 Luna', effort: 'low' },
  { id: 'opencode-luna-medium', label: 'OpenCode: GPT-5.6 Luna Medium', agent: true, preview: false, familyId: 'opencode-luna', familyLabel: 'OpenCode: GPT-5.6 Luna', shortFamilyLabel: 'GPT-5.6 Luna', effort: 'medium' },
  { id: 'opencode-luna-high', label: 'OpenCode: GPT-5.6 Luna High', agent: true, preview: false, familyId: 'opencode-luna', familyLabel: 'OpenCode: GPT-5.6 Luna', shortFamilyLabel: 'GPT-5.6 Luna', effort: 'high' },
  { id: 'opencode-luna-xhigh', label: 'OpenCode: GPT-5.6 Luna X-High', agent: true, preview: false, familyId: 'opencode-luna', familyLabel: 'OpenCode: GPT-5.6 Luna', shortFamilyLabel: 'GPT-5.6 Luna', effort: 'xhigh' },
  { id: 'opencode-luna-max', label: 'OpenCode: GPT-5.6 Luna Max', agent: true, preview: false, familyId: 'opencode-luna', familyLabel: 'OpenCode: GPT-5.6 Luna', shortFamilyLabel: 'GPT-5.6 Luna', effort: 'max' },

  // Agy: Gemini 4 Argon
  { id: 'agy-argon-low', label: 'Agy: Gemini 4 Argon Low', agent: true, preview: false, familyId: 'agy-argon', familyLabel: 'Agy: Gemini 4 Argon', shortFamilyLabel: 'Gemini 4 Argon', effort: 'low' },
  { id: 'agy-argon-medium', label: 'Agy: Gemini 4 Argon Medium', agent: true, preview: false, familyId: 'agy-argon', familyLabel: 'Agy: Gemini 4 Argon', shortFamilyLabel: 'Gemini 4 Argon', effort: 'medium' },
  { id: 'agy-argon-high', label: 'Agy: Gemini 4 Argon High', agent: true, preview: false, familyId: 'agy-argon', familyLabel: 'Agy: Gemini 4 Argon', shortFamilyLabel: 'Gemini 4 Argon', effort: 'high' },

  // Agy: Gemini 3.8 Flash
  { id: 'agy-flash-none', label: 'Agy: Gemini 3.8 Flash None', agent: false, preview: false, familyId: 'agy-flash', familyLabel: 'Agy: Gemini 3.8 Flash', shortFamilyLabel: 'Gemini 3.8 Flash', effort: 'none' },
  { id: 'agy-flash-low', label: 'Agy: Gemini 3.8 Flash Low', agent: true, preview: false, familyId: 'agy-flash', familyLabel: 'Agy: Gemini 3.8 Flash', shortFamilyLabel: 'Gemini 3.8 Flash', effort: 'low' },
  { id: 'agy-flash-medium', label: 'Agy: Gemini 3.8 Flash Medium', agent: true, preview: false, familyId: 'agy-flash', familyLabel: 'Agy: Gemini 3.8 Flash', shortFamilyLabel: 'Gemini 3.8 Flash', effort: 'medium' },
  { id: 'agy-flash-high', label: 'Agy: Gemini 3.8 Flash High', agent: true, preview: false, familyId: 'agy-flash', familyLabel: 'Agy: Gemini 3.8 Flash', shortFamilyLabel: 'Gemini 3.8 Flash', effort: 'high' },

  // Agy: Claude Opus 5.5
  { id: 'agy-opus-low', label: 'Agy: Claude Opus 5.5 Low', agent: true, preview: false, familyId: 'agy-opus', familyLabel: 'Agy: Claude Opus 5.5', shortFamilyLabel: 'Claude Opus 5.5', effort: 'low' },
  { id: 'agy-opus-medium', label: 'Agy: Claude Opus 5.5 Medium', agent: true, preview: false, familyId: 'agy-opus', familyLabel: 'Agy: Claude Opus 5.5', shortFamilyLabel: 'Claude Opus 5.5', effort: 'medium' },
  { id: 'agy-opus-high', label: 'Agy: Claude Opus 5.5 High', agent: true, preview: false, familyId: 'agy-opus', familyLabel: 'Agy: Claude Opus 5.5', shortFamilyLabel: 'Claude Opus 5.5', effort: 'high' },

  // Agy: Claude Sonnet 5.5
  { id: 'agy-sonnet-low', label: 'Agy: Claude Sonnet 5.5 Low', agent: true, preview: false, familyId: 'agy-sonnet', familyLabel: 'Agy: Claude Sonnet 5.5', shortFamilyLabel: 'Claude Sonnet 5.5', effort: 'low' },
  { id: 'agy-sonnet-medium', label: 'Agy: Claude Sonnet 5.5 Medium', agent: true, preview: false, familyId: 'agy-sonnet', familyLabel: 'Agy: Claude Sonnet 5.5', shortFamilyLabel: 'Claude Sonnet 5.5', effort: 'medium' },
  { id: 'agy-sonnet-high', label: 'Agy: Claude Sonnet 5.5 High', agent: true, preview: false, familyId: 'agy-sonnet', familyLabel: 'Agy: Claude Sonnet 5.5', shortFamilyLabel: 'Claude Sonnet 5.5', effort: 'high' },
] as const

export type InstantChoice = typeof MODEL_CATALOG[number]['id']
export type AgentChoice = Extract<typeof MODEL_CATALOG[number], { readonly agent: true }>['id']
export const INSTANT_CHOICES: readonly InstantChoice[] = MODEL_CATALOG.map((model) => model.id)
export const AGENT_CHOICES: readonly AgentChoice[] = (MODEL_CATALOG as readonly ModelCatalogEntry[]).filter(
  (model) => model.agent,
).map((model) => model.id) as unknown as readonly AgentChoice[]
export const DEFAULT_MODEL_CHOICES = { instant: 'local-fast' as InstantChoice, deep: 'opencode-sol-high' as AgentChoice, review: 'opencode-sol-high' as AgentChoice } as const

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

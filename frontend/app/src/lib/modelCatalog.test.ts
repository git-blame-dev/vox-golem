import { describe, expect, it } from 'vitest'
import { AGENT_CHOICES, DEFAULT_MODEL_CHOICES, MODEL_CATALOG, modelLabels } from './modelCatalog'

describe('generated model metadata', () => {
  it('preserves historical family IDs, stage restrictions and defaults', () => {
    expect(MODEL_CATALOG).toHaveLength(44)
    expect(new Set(MODEL_CATALOG.map((model) => model.id)).size).toBe(44)
    expect(MODEL_CATALOG.find((model) => model.id === 'custom-luna-none')).toMatchObject({
      familyId: 'custom-luna6', effort: 'none', agent: false,
      label: 'Custom: GPT-6 Luna No Reasoning',
    })
    expect(MODEL_CATALOG.find((model) => model.id === 'custom-luna-low')).toMatchObject({
      familyId: 'custom-luna56', effort: 'low', agent: true,
    })
    expect(AGENT_CHOICES).not.toContain('custom-luna-none')
    expect(AGENT_CHOICES).not.toContain('custom-luna56-none')
    expect(AGENT_CHOICES).toContain(DEFAULT_MODEL_CHOICES.deep)
    expect(AGENT_CHOICES).toContain(DEFAULT_MODEL_CHOICES.review)
    expect(DEFAULT_MODEL_CHOICES.instant).toBe('local-fast')
    expect(modelLabels('deep')['custom-sol-high']).toBe('Deep: Custom: GPT-6.1 Sol High')
    expect(modelLabels('deep')).not.toHaveProperty('custom-luna-none')
    expect(MODEL_CATALOG.filter((model) => model.preview).map((model) => model.id)).toEqual(['local-fast'])
  })
})

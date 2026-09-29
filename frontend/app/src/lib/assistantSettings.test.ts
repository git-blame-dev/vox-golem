import { describe, expect, it } from 'vitest'
import { DEFAULT_ASSISTANT_SETTINGS, deepOptions, instantOptions, parseAssistantSettings, reviewOptions, serializeAssistantSettings } from './assistantSettings'

const all = { localFast: true, localQuality: true, custom: true, openCode: true, qwenPrediction: true, deep: true, review: true }
describe('assistant settings contract', () => {
  it('has safe defaults', () => expect(DEFAULT_ASSISTANT_SETTINGS).toEqual({ instant: 'local-fast', deep: 'opencode-sol-high', review: 'opencode-sol-high', deepEnabled: false, reviewEnabled: false, prefetch: false, completion: true }))
  it('labels every choice with its provider', () => {
    expect(instantOptions(all)).toHaveLength(8); expect(deepOptions(all)).toHaveLength(4); expect(reviewOptions(all)).toHaveLength(4)
    expect(instantOptions(all).map((x) => x.label)).toEqual(['Local: Fast', 'Local: Quality', 'Custom: GPT-6 Sol High', 'Custom: GPT-6 Sol No Reasoning', 'Custom: GPT-6 Luna No Reasoning', 'Custom: GPT-5.6 Luna Low', 'OpenCode: GPT-6 Sol High', 'OpenCode: GPT-5.6 Luna Low'])
    expect(deepOptions(all).map((x) => x.value)).toEqual(['custom-sol-high', 'custom-luna-low', 'opencode-sol-high', 'opencode-luna-low'])
    expect(reviewOptions(all).map((x) => x.value)).toEqual(['custom-sol-high', 'custom-luna-low', 'opencode-sol-high', 'opencode-luna-low'])
    expect(deepOptions(all).map((x) => x.description)).toEqual(['Custom reasoning-only', 'Custom reasoning-only', 'OpenCode search', 'OpenCode search'])
  })
  it('reports unavailable providers', () => expect(deepOptions({ ...all, custom: false, openCode: false }).filter((x) => !x.available).every((x) => x.reason)).toBe(true))
  it('strictly parses and serializes backend payloads', () => {
    const value = parseAssistantSettings({ instant: 'local-fast', deep: 'opencode-sol-high', review: 'custom-luna-low', deep_enabled: true, review_enabled: false, prefetch: true, completion: false })
    expect(serializeAssistantSettings(value)).toMatchObject({ deep_enabled: true, review_enabled: false })
    expect(() => parseAssistantSettings({})).toThrow()
    expect(() => parseAssistantSettings({ instant: 'local-balanced', deep: 'opencode-sol-high', review: 'opencode-sol-high', deep_enabled: false, review_enabled: false, prefetch: false, completion: true })).toThrow()
    expect(() => parseAssistantSettings({ instant: 'local-fast', deep: 'opencode-sol-high', review: 'none', deep_enabled: false, review_enabled: false, prefetch: false, completion: true })).toThrow()
    expect(() => parseAssistantSettings({ instant: 'local-fast', deep: 'opencode-sol-high', review: 'opencode-sol-high', deep_enabled: 1, review_enabled: false, prefetch: false, completion: true })).toThrow()
  })

  it('round-trips a direct non-reasoning Instant choice without adding it to Deep or Review', () => {
    const payload = {
      instant: 'custom-sol-none', deep: 'custom-sol-high', review: 'opencode-sol-high',
      deep_enabled: false, review_enabled: false, prefetch: false, completion: true,
    }
    expect(serializeAssistantSettings(parseAssistantSettings(payload))).toEqual(payload)
    expect(instantOptions(all).find((option) => option.value === 'custom-sol-none')).toMatchObject({
      available: true, description: 'Direct Custom connection; reasoning disabled',
    })
    expect(deepOptions(all).map((option) => option.value)).not.toContain('custom-sol-none')
  })
  it('round-trips direct GPT-6 Luna None and keeps it Instant-only', () => {
    const payload = { instant: 'custom-luna-none', deep: 'custom-sol-high', review: 'custom-luna-low', deep_enabled: false, review_enabled: false, prefetch: true, completion: true }
    expect(serializeAssistantSettings(parseAssistantSettings(payload))).toEqual(payload)
    expect(instantOptions(all).find((option) => option.value === payload.instant)).toMatchObject({ available: true, description: 'Direct Custom connection; reasoning disabled' })
    expect(instantOptions({ ...all, custom: false }).find((option) => option.value === payload.instant)?.available).toBe(false)
    expect(deepOptions(all).map((option) => option.value)).not.toContain(payload.instant)
    expect(reviewOptions(all).map((option) => option.value)).not.toContain(payload.instant)
  })
})

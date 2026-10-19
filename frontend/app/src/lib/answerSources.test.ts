import { describe, expect, it } from 'vitest'
import { MAX_ANSWER_SOURCES, mergeAnswerSources } from './answerSources'

describe('observed answer sources', () => {
  it('keeps exact URLs, first titles and stable encounter order without mutating inputs', () => {
    const current = [{ url: 'https://example.test/a', title: 'Synthetic first title' }]
    const incoming = [
      { url: 'https://example.test/a', title: 'Synthetic later title' },
      { url: 'https://example.test/a/', title: 'Synthetic distinct URL' },
      { url: 'https://example.test/b', title: 'Synthetic new source' },
      { url: 'https://example.test/b', title: 'Synthetic duplicate' },
    ]
    Object.freeze(current)
    Object.freeze(incoming)
    expect(mergeAnswerSources(current, incoming)).toEqual([current[0], incoming[1], incoming[2]])
    expect(current).toHaveLength(1)
    expect(incoming).toHaveLength(4)
  })

  it('bounds cumulative sources using the same limit as individual wire events', () => {
    const sources = Array.from({ length: MAX_ANSWER_SOURCES + 8 }, (_, index) => ({
      url: `https://example.test/source-${index}`, title: `Synthetic source ${index}`,
    }))
    const merged = mergeAnswerSources(sources.slice(0, 24), sources.slice(16))
    expect(merged).toEqual(sources.slice(0, MAX_ANSWER_SOURCES))
    expect(mergeAnswerSources(merged, sources)).toEqual(merged)
  })

  it('does not clear observed sources when an operation adds none', () => {
    const sources = [{ url: 'https://example.test/a', title: 'Synthetic observed source' }]
    expect(mergeAnswerSources(sources, [])).toEqual(sources)
    expect(mergeAnswerSources([], [])).toEqual([])
  })
})

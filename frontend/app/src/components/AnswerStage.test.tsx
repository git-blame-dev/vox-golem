import { act } from 'react'
import { createRoot } from 'react-dom/client'
import type { Root } from 'react-dom/client'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { AnswerStage } from './AnswerStage'

const roots: Root[] = []
const containers: HTMLDivElement[] = []

afterEach(() => {
  for (const root of roots) act(() => root.unmount())
  for (const container of containers) container.remove()
  roots.length = 0
  containers.length = 0
})

function render(props: Parameters<typeof AnswerStage>[0]) {
  const container = document.createElement('div')
  document.body.append(container)
  const root = createRoot(container)
  roots.push(root)
  containers.push(container)
  act(() => root.render(<AnswerStage {...props} />))
  return container
}

describe('AnswerStage', () => {
  it('compacts phone answers without losing refusal, busy/terminal status or evidence', () => {
    const container = render({ compact: true, answer: 'Synthetic answer', stages: [{ stage: 'instant', status: 'completed' }], sources: [{ url: 'https://example.test/source', title: 'Synthetic source' }], priorVersions: [{ id: 'synthetic-old', text: 'Synthetic prior answer' }] })
    expect(container.querySelector('h2')).toBeNull()
    expect(container.textContent).not.toContain('Current answer')
    expect(container.querySelector('[data-status="completed"]')).toBeNull()
    expect(container.querySelector('details')?.textContent).toContain('Synthetic source')
    expect(container.querySelector('details')?.textContent).toContain('Synthetic prior answer')
    const refusal = render({ compact: true, contentKind: 'refusal', answer: 'Synthetic refusal', stages: [{ stage: 'instant', status: 'failed' }] })
    expect(refusal.querySelector('h2')?.textContent).toBe('Assistant refusal')
    expect(refusal.querySelector('[data-status="failed"]')).toBeTruthy()
    const busy = render({ compact: true, answer: 'Synthetic stream', stages: [{ stage: 'instant', status: 'running' }] })
    expect(busy.querySelector('[data-status="running"]')).toBeTruthy()
  })
  it('exposes the authoritative answer and accessible status names', () => {
    const container = render({
      answer: '<em>safe</em> **plain text**',
      stages: [
        { stage: 'instant', status: 'completed' },
        { stage: 'deep', status: 'running' },
        { stage: 'review', status: 'queued' },
      ],
    })

    expect(container.querySelector('h2')?.textContent).toBe('Current answer')
    expect(container.querySelector('.answer-stage__answer')?.textContent).toContain('<em>safe</em> **plain text**')
    expect(container.querySelector('.answer-stage__answer em')).toBeNull()
    expect(container.querySelector('[aria-label="Instant status: completed"]')).toBeTruthy()
    expect(container.querySelector('[aria-label="Deep status: running"]')).toBeTruthy()
    expect(container.querySelector('[aria-label="Review status: queued"]')).toBeTruthy()
    expect(container.querySelector('details')).toBeNull()
  })

  it('shows recorded prior versions even when one matches the current answer', () => {
    const container = render({
      answer: 'final',
      stages: [{ stage: 'review', status: 'kept' }],
      priorVersions: [
        { id: 'same', text: 'final' },
        { id: 'old', text: 'old <b>answer</b>', label: 'Before review' },
      ],
    })

    const details = container.querySelector('details')
    expect(details?.querySelector('summary')?.textContent).toBe('Details')
    expect(details?.querySelectorAll('h3')[1]?.textContent).toBe('Before review')
    expect(details?.textContent).toContain('old <b>answer</b>')
    expect(details?.textContent).toContain('final')
    expect(details?.querySelector('b')).toBeNull()
    expect(container.querySelector('[data-status="kept"]')).toBeTruthy()
    expect(container.querySelector('summary')?.getAttribute('role')).toBeNull()
  })

  it('omits history when no prior versions were recorded', () => {
    const container = render({ answer: 'answer', stages: [], priorVersions: [] })
    expect(container.querySelector('details')).toBeNull()
  })

  it.each([
    ['sources only', { sources: [{ url: 'https://example.com', title: 'Example' }] }],
    ['correction only', { stages: [{ stage: 'review' as const, status: 'corrected' as const, detail: 'The corrected answer' }] }],
    ['prior only', { priorVersions: [{ id: 'old', text: 'Previous answer', label: 'Earlier' }] }],
  ])('uses one Details disclosure for %s metadata', (_name, metadata) => {
    const container = render({ answer: 'answer', stages: [], ...metadata })
    expect(container.querySelectorAll('details')).toHaveLength(1)
    expect(container.querySelector('summary')?.textContent).toBe('Details')
  })

  it('consolidates sources, correction detail, and prior versions without nesting disclosures', () => {
    const container = render({
      answer: 'answer',
      stages: [{ stage: 'review', status: 'corrected', detail: 'Correction text' }],
      sources: [{ url: 'https://example.com', title: 'Example' }],
      priorVersions: [{ id: 'old', text: 'Previous answer', label: 'Earlier' }],
    })
    const details = container.querySelector('details')
    expect(container.querySelectorAll('details')).toHaveLength(1)
    expect(details?.textContent).toContain('Example')
    expect(details?.textContent).toContain('Correction text')
    expect(details?.textContent).toContain('Previous answer')
    expect(details?.querySelector('a')?.getAttribute('rel')).toBe('noreferrer')
    expect(container.querySelector('[data-status="corrected"]')?.textContent).not.toContain('Correction text')
  })

  it('keeps stage semantic statuses outside the disclosure', () => {
    const container = render({
      answer: 'answer',
      stages: [
        { stage: 'instant', status: 'completed' },
        { stage: 'deep', status: 'running' },
        { stage: 'review', status: 'queued' },
      ],
    })
    expect(container.querySelector('[aria-label="Instant status: completed"]')).toBeTruthy()
    expect(container.querySelector('[aria-label="Deep status: running"]')).toBeTruthy()
    expect(container.querySelector('[aria-label="Review status: queued"]')).toBeTruthy()
    expect(container.querySelector('.answer-stage__statuses')?.closest('details')).toBeNull()
  })

  it('collects every non-empty stage detail with stage and status labels', () => {
    const container = render({
      answer: 'answer',
      stages: [
        { stage: 'instant', status: 'completed', detail: 'Completed detail' },
        { stage: 'deep', status: 'failed', detail: 'Failure detail' },
        { stage: 'review', status: 'stale', detail: 'Stale detail' },
        { stage: 'review', status: 'corrected', detail: 'Corrected detail' },
        { stage: 'instant', status: 'running', detail: '   ' },
      ],
    })
    const details = container.querySelector('details')
    expect(details?.textContent).toContain('Instant (completed): Completed detail')
    expect(details?.textContent).toContain('Deep (failed): Failure detail')
    expect(details?.textContent).toContain('Review (stale): Stale detail')
    expect(details?.textContent).toContain('Review (corrected): Corrected detail')
    expect(details?.textContent).not.toContain('running')
  })

  it('renders duplicate source URLs without duplicate React keys', () => {
    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => undefined)
    const container = render({
      answer: 'answer',
      stages: [],
      sources: [
        { url: 'https://example.com', title: 'First citation' },
        { url: 'https://example.com', title: 'Second citation' },
      ],
    })

    expect(container.querySelectorAll('.answer-stage__sources a')).toHaveLength(2)
    expect(container.textContent).toContain('First citation')
    expect(container.textContent).toContain('Second citation')
    expect(errorSpy.mock.calls.flat().join(' ')).not.toContain('unique "key" prop')
  })
})

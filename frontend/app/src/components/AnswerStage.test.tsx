import { act } from 'react'
import { createRoot } from 'react-dom/client'
import type { Root } from 'react-dom/client'
import { afterEach, describe, expect, it } from 'vitest'
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
    expect(details?.querySelector('summary')?.textContent).toContain('2')
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
})

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { installHistoryHarness, type TestHistoryHarness } from './historyHarness'

let harness: TestHistoryHarness
const initialUrl = window.location.href

beforeEach(() => { harness = installHistoryHarness(null) })
afterEach(() => {
  harness.restore()
  window.history.replaceState(null, '', initialUrl)
  vi.restoreAllMocks()
})

describe('App history harness', () => {
  it('replaces the current entry and truncates forward history on push', async () => {
    window.history.pushState({ page: 'one' }, '', '#one')
    window.history.pushState({ page: 'two' }, '', '#two')
    window.history.back()
    await Promise.resolve()
    window.history.replaceState({ page: 'replaced' }, '', '#replaced')
    expect(harness.entries).toEqual([null, { page: 'replaced' }, { page: 'two' }])
    window.history.pushState({ page: 'new' }, '', '#new')
    window.history.forward()
    expect(harness.entries).toEqual([null, { page: 'replaced' }, { page: 'new' }])
    expect(harness.index).toBe(2)
    expect(window.history.state).toEqual({ page: 'new' })
    expect(window.location.hash).toBe('#new')
    expect(harness.forwardCalls).toBe(1)
  })

  it('captures each traversed state before queuing popstate', async () => {
    const states: unknown[] = []
    const listener = (event: PopStateEvent): void => { states.push(event.state) }
    window.addEventListener('popstate', listener)
    try {
      window.history.pushState('one', '')
      window.history.pushState('two', '')
      window.history.back()
      window.history.back()
      window.history.replaceState('changed after traversal', '')
      expect(states).toEqual([])
      await Promise.resolve()
      expect(states).toEqual(['one', null])
      expect(harness.backCalls).toBe(2)
    } finally { window.removeEventListener('popstate', listener) }
  })

  it('restores entry URLs and state through go and forward without emitting for no-op traversal', async () => {
    const popstate = vi.fn()
    window.addEventListener('popstate', popstate)
    try {
      window.history.pushState('one', '', '#one')
      window.history.pushState('two', '', '#two')
      window.history.go(-2)
      expect(window.location.href).toBe(initialUrl)
      expect(window.history.state).toBeNull()
      window.history.forward()
      expect(window.location.hash).toBe('#one')
      expect(window.history.state).toBe('one')
      window.history.go(1)
      expect(window.location.hash).toBe('#two')
      window.history.go()
      window.history.go(0)
      window.history.go(100)
      window.history.go(-100)
      await Promise.resolve()
      expect(popstate).toHaveBeenCalledTimes(3)
    } finally { window.removeEventListener('popstate', popstate) }
  })

  it('keeps deferred Back pending until settlement and settles it only once', async () => {
    const popstate = vi.fn()
    window.addEventListener('popstate', popstate)
    try {
      window.history.pushState('one', '')
      harness.deferBack = true
      window.history.back()
      await Promise.resolve()
      expect(window.history.state).toBe('one')
      expect(harness.index).toBe(1)
      expect(popstate).not.toHaveBeenCalled()
      harness.settleBack()
      harness.settleBack()
      expect(window.history.state).toBeNull()
      expect(harness.index).toBe(0)
      await Promise.resolve()
      expect(popstate).toHaveBeenCalledTimes(1)
      expect(harness.backCalls).toBe(1)
    } finally { window.removeEventListener('popstate', popstate) }
  })
})

import { vi } from 'vitest'

export interface TestHistoryHarness {
  entries: unknown[]
  index: number
  backCalls: number
  forwardCalls: number
  deferBack: boolean
  settleBack: () => void
  pushState: History['pushState']
  replaceState: History['replaceState']
  back: History['back']
  go: History['go']
  forward: History['forward']
  setCurrentState: (state: unknown) => void
  restore: () => void
}

/** In-memory navigation for App tests; go(0) reloads are outside this harness. */
export function installHistoryHarness(initialState: unknown = window.history.state): TestHistoryHarness {
  const replace = window.history.replaceState.bind(window.history)
  const entries: unknown[] = [initialState]
  const urls = [window.location.href]
  let index = 0
  let pendingBack: number | null = null
  const expose = (): void => replace(entries[index] ?? null, '', urls[index])
  const traverse = (nextIndex: number): void => {
    if (!Number.isFinite(nextIndex) || nextIndex === index || nextIndex < 0 || nextIndex >= entries.length) return
    index = nextIndex
    harness.index = index
    expose()
    const traversedState = entries[index]
    queueMicrotask(() => {
      const event = new PopStateEvent('popstate', { state: traversedState })
      // happy-dom aliases PopStateEvent to Event, which ignores the state option.
      Object.defineProperty(event, 'state', { value: traversedState })
      window.dispatchEvent(event)
    })
  }
  const harness: TestHistoryHarness = {
    entries,
    index,
    backCalls: 0,
    forwardCalls: 0,
    deferBack: false,
    settleBack: () => {
      if (pendingBack === null) return
      const target = pendingBack
      pendingBack = null
      traverse(target)
    },
    pushState: (state: unknown, _unused: string, url?: string | URL | null) => {
      replace(state, '', url)
      entries.splice(index + 1)
      urls.splice(index + 1)
      entries.push(state)
      urls.push(window.location.href)
      index += 1
      harness.index = index
    },
    replaceState: (state: unknown, _unused: string, url?: string | URL | null) => {
      replace(state, '', url)
      entries[index] = state
      urls[index] = window.location.href
    },
    back: () => {
      harness.backCalls += 1
      if (harness.deferBack) pendingBack = index - 1
      else traverse(index - 1)
    },
    forward: () => {
      harness.forwardCalls += 1
      traverse(index + 1)
    },
    go: (delta = 0) => { traverse(index + Math.trunc(delta)) },
    setCurrentState: (state) => { harness.replaceState(state, '') },
    restore: () => { for (const mock of mocks) mock.mockRestore() },
  }
  const mocks = [
    vi.spyOn(window.history, 'pushState').mockImplementation(harness.pushState),
    vi.spyOn(window.history, 'replaceState').mockImplementation(harness.replaceState),
    vi.spyOn(window.history, 'back').mockImplementation(harness.back),
    vi.spyOn(window.history, 'forward').mockImplementation(harness.forward),
    vi.spyOn(window.history, 'go').mockImplementation(harness.go),
  ]
  expose()
  return harness
}

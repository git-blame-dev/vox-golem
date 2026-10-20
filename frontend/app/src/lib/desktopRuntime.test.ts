import { describe, expect, it, vi } from 'vitest'
import { DesktopRuntimeObserver, parseDesktopRuntimeRead } from './desktopRuntime'
import type { DesktopRuntimeSnapshot } from './desktopRuntime'

const voice = { auto_stop_on_silence: true, auto_start_available: false, capture_id: 7, state: 'active', runtime_phase: 'listening', wake_confidence: 0.75,
  partial_transcript: { session_id: 41, revision: 9, text: 'Synthetic partial' }, error: null }
const speech = { playback_id: null, pending: false, playing: false }
const prompt = { request_id: 'synthetic-native', execution_id: { session_id: '1', generation: '2' }, prompt: 'Synthetic prompt', updates: [{ kind: 'text', text: 'Synthetic answer' }], result: null }
function root(revision: number) { return { revision, snapshot: { revision, voice, speech, prompt }, events: [] } }
function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (error: Error) => void
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no })
  return { promise, resolve, reject }
}

describe('desktop runtime wire boundary', () => {
  it('accepts valid escaped accumulated text plus canonical result within the expanded native projection budget', () => {
    const answer = '\u0001'.repeat(200 * 1024)
    const turn = { request_id: prompt.request_id, execution_id: prompt.execution_id,
      user: { id: '3', role: 'user', content: { kind: 'text', text: prompt.prompt } },
      answer: { id: '4', role: 'assistant', content: { kind: 'text', text: answer } }, sources: [], final_stage: 'instant' }
    const payload = { revision: 1, snapshot: { revision: 1, voice, speech, prompt: { ...prompt, updates: [{ kind: 'text', text: answer }], result: { kind: 'completed', turn } } }, events: [] }
    expect(JSON.stringify(payload).length).toBeGreaterThan(2 * 1024 * 1024)
    expect(parseDesktopRuntimeRead(payload).snapshot?.prompt?.result?.canonicalTurn?.answer.content.text).toBe(answer)
  })
  it('enforces the aggregate five-MiB wire budget including JSON escaping, even for otherwise ignored fields', () => {
    expect(() => parseDesktopRuntimeRead({ ...root(1), padding: Array(4).fill('\u0001'.repeat(256 * 1024)) })).toThrow(/wire budget/)
  })
  it('requires native-effective auto-stop and process-lifetime autostart consent instead of guessing defaults', () => {
    for (const field of ['auto_stop_on_silence', 'auto_start_available']) {
      const missing = { ...voice } as Record<string, unknown>
      delete missing[field]
      expect(() => parseDesktopRuntimeRead({ revision: 1, snapshot: { revision: 1, voice: missing, speech, prompt: null }, events: [] })).toThrow()
    }
    expect(parseDesktopRuntimeRead({ revision: 1, snapshot: { revision: 1, voice: { ...voice, auto_stop_on_silence: false, auto_start_available: true }, speech, prompt: null }, events: [] }).snapshot?.voice).toMatchObject({ autoStopOnSilence: false, autoStartAvailable: true })
  })
  it('preserves native partial and canonical execution identity without a renderer sequence', () => {
    const parsed = parseDesktopRuntimeRead(root(12))
    expect(parsed.snapshot?.voice.partialTranscript).toEqual({ sessionId: 41, revision: 9, text: 'Synthetic partial' })
    expect(parsed.snapshot?.prompt).toMatchObject({ requestId: 'synthetic-native', executionId: { sessionId: '1', generation: '2' }, updates: [{ requestId: 'synthetic-native', kind: 'text', text: 'Synthetic answer' }] })
  })
  it.each([NaN, Infinity, -1, 0.5, Number.MAX_SAFE_INTEGER + 1, '3'])('rejects unsafe revision %s', (revision) => {
    expect(() => parseDesktopRuntimeRead({ ...root(1), revision })).toThrow()
  })
  it('rejects oversized text, event batches, unsafe native IDs and unknown states', () => {
    expect(() => parseDesktopRuntimeRead({ ...root(1), events: Array(257).fill({ revision: 1, kind: 'speech', speech }) })).toThrow()
    expect(() => parseDesktopRuntimeRead({ revision: 1, snapshot: { revision: 1, voice: { ...voice, capture_id: Number.MAX_SAFE_INTEGER + 1 }, speech, prompt }, events: [] })).toThrow()
    expect(() => parseDesktopRuntimeRead({ revision: 1, snapshot: { revision: 1, voice: { ...voice, state: 'unknown' }, speech, prompt }, events: [] })).toThrow()
    expect(() => parseDesktopRuntimeRead({ revision: 1, snapshot: { revision: 1, voice, speech, prompt: { ...prompt, updates: [{ kind: 'text', text: 'x'.repeat(256 * 1024 + 1) }] } }, events: [] })).toThrow()
  })
  it('uses the existing canonical parser for refusal/source integrity and request correlation', () => {
    const turn = { request_id: 'synthetic-native', execution_id: prompt.execution_id,
      user: { id: '3', role: 'user', content: { kind: 'text', text: prompt.prompt } },
      answer: { id: '4', role: 'assistant', content: { kind: 'refusal', text: 'Synthetic refusal' } }, sources: [], final_stage: 'instant' }
    const read = { revision: 1, snapshot: null, events: [{ revision: 1, kind: 'prompt_finished', request_id: prompt.request_id, result: { kind: 'refused', turn } }] }
    expect(parseDesktopRuntimeRead(read).events[0]).toMatchObject({ result: { canonicalOutcome: 'refused', canonicalTurn: { answer: { content: { kind: 'refusal' } } } } })
    expect(() => parseDesktopRuntimeRead({ ...read, events: [{ ...read.events[0], request_id: 'synthetic-other' }] })).toThrow()
    expect(() => parseDesktopRuntimeRead({ ...read, events: [{ ...read.events[0], result: { kind: 'refused', turn: { ...turn, sources: [{ url: 'https://example.test', title: 'Synthetic' }] } } }] })).toThrow()
  })
})

describe('desktop observation', () => {
  function harness(read: (cursor: number | null) => Promise<unknown>) {
    let receive!: (payload: unknown) => void
    const unlisten = vi.fn()
    const snapshot = vi.fn<(snapshot: DesktopRuntimeSnapshot) => void>()
    const event = vi.fn()
    const error = vi.fn()
    const observer = new DesktopRuntimeObserver({ read, listen: async (callback) => { receive = callback; return unlisten }, snapshot, event, error })
    return { observer, snapshot, event, error, unlisten, receive: (payload: unknown) => receive(payload) }
  }
  it('listens before initial read and coalesces an invalidation racing its response', async () => {
    const initial = deferred<unknown>()
    const read = vi.fn<(cursor: number | null) => Promise<unknown>>().mockReturnValueOnce(initial.promise).mockResolvedValueOnce({ revision: 3, snapshot: null, events: [{ revision: 2, kind: 'speech', speech }, { revision: 3, kind: 'voice', voice }] })
    const h = harness(read)
    const started = h.observer.start()
    await vi.waitFor(() => expect(read).toHaveBeenCalledTimes(1))
    expect(read).toHaveBeenCalledWith(null)
    h.receive({ revision: 2 }); h.receive({ revision: 3 }); h.receive({ revision: 2 })
    expect(read).toHaveBeenCalledTimes(1)
    initial.resolve(root(1))
    await started
    expect(read.mock.calls).toEqual([[null], [1]])
    expect(h.snapshot).toHaveBeenCalledTimes(1)
    expect(h.event.mock.calls.map(([event]) => event.revision)).toEqual([2, 3])
    h.receive({ revision: 3 }); h.receive({ revision: 1 })
    expect(read).toHaveBeenCalledTimes(2)
    h.observer.stop()
    expect(h.unlisten).toHaveBeenCalledTimes(1)
  })
  it('fences a pre-reset response and obtains a new root without replaying it', async () => {
    const initial = deferred<unknown>()
    const read = vi.fn<(cursor: number | null) => Promise<unknown>>().mockReturnValueOnce(initial.promise).mockResolvedValueOnce(root(9))
    const h = harness(read)
    const started = h.observer.start(); await vi.waitFor(() => expect(read).toHaveBeenCalledTimes(1))
    h.observer.invalidate()
    initial.resolve(root(1)); await started
    expect(h.snapshot.mock.calls.map(([snapshot]) => snapshot.revision)).toEqual([9])
    expect(read.mock.calls).toEqual([[null], [null]])
    h.observer.stop()
  })
  it('fails closed on a lost revision, then reattaches via a root snapshot without cancellation', async () => {
    const read = vi.fn<(cursor: number | null) => Promise<unknown>>().mockResolvedValueOnce(root(1)).mockResolvedValueOnce({ revision: 3, snapshot: null, events: [{ revision: 3, kind: 'speech', speech }] }).mockResolvedValueOnce(root(4))
    const h = harness(read)
    await h.observer.start()
    h.receive({ revision: 3 })
    await vi.waitFor(() => expect(h.error).toHaveBeenCalledTimes(1))
    expect(h.event).not.toHaveBeenCalled()
    await h.observer.refresh()
    expect(read.mock.calls).toEqual([[null], [1], [null]])
    expect(h.snapshot.mock.calls.map(([snapshot]) => snapshot.revision)).toEqual([1, 4])
    h.observer.stop()
  })
  it('detaching does not publish late responses or perform a native Stop', async () => {
    const initial = deferred<unknown>()
    const read = vi.fn().mockReturnValue(initial.promise)
    const h = harness(read)
    const started = h.observer.start(); await vi.waitFor(() => expect(read).toHaveBeenCalledTimes(1))
    h.observer.stop(); initial.resolve(root(1)); await started
    expect(h.snapshot).not.toHaveBeenCalled()
    expect(h.event).not.toHaveBeenCalled()
    expect(h.error).not.toHaveBeenCalled()
  })
})

import { parseExecutionId } from './appSnapshot'
import type { ExecutionId } from './appSnapshot'
import { parseCanonicalPromptOutcome, parsePromptExecutionEvent, parsePromptExecutionResult } from './promptExecution'
import { isRecord, parseRuntimePhase, utf8ByteLength } from './wire'
import type { BackendRuntimePhase, PromptExecutionEvent, PromptExecutionResult } from '../types/chat'

export interface VoiceProjection {
  readonly autoStopOnSilence: boolean
  readonly autoStartAvailable: boolean
  readonly captureId: number | null
  readonly state: 'stopped' | 'starting' | 'active' | 'error'
  readonly runtimePhase: BackendRuntimePhase
  readonly wakeConfidence: number | null
  readonly partialTranscript: { readonly sessionId: number; readonly revision: number; readonly text: string } | null
  readonly error: string | null
}
export interface SpeechProjection { readonly playbackId: number | null; readonly playing: boolean; readonly pending: boolean }
export interface NativePromptProjection {
  readonly requestId: string
  readonly executionId: ExecutionId
  readonly prompt: string
  readonly updates: readonly PromptExecutionEvent[]
  readonly result: PromptExecutionResult | null
}
export interface DesktopRuntimeSnapshot {
  readonly revision: number
  readonly voice: VoiceProjection
  readonly prompt: NativePromptProjection | null
  readonly speech: SpeechProjection
}
export type DesktopRuntimeEvent = { readonly revision: number } & (
  | { readonly kind: 'voice'; readonly voice: VoiceProjection }
  | { readonly kind: 'speech'; readonly speech: SpeechProjection }
  | { readonly kind: 'prompt_started'; readonly prompt: NativePromptProjection }
  | { readonly kind: 'prompt_update'; readonly requestId: string; readonly update: PromptExecutionEvent }
  | { readonly kind: 'prompt_finished'; readonly requestId: string; readonly result: PromptExecutionResult }
)
export interface DesktopRuntimeRead { readonly revision: number; readonly snapshot: DesktopRuntimeSnapshot | null; readonly events: readonly DesktopRuntimeEvent[] }

function record(value: unknown): Record<string, unknown> {
  if (!isRecord(value)) throw new Error('Invalid desktop runtime projection')
  return value
}
function sequence(value: unknown): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0) throw new Error('Invalid desktop runtime revision')
  return value
}
function nullableId(value: unknown): number | null { return value === null ? null : sequence(value) }
function text(value: unknown, max: number, nonempty = false): string {
  if (typeof value !== 'string' || value.length > max || utf8ByteLength(value) > max || (nonempty && value.trim().length === 0)) throw new Error('Invalid desktop runtime text')
  return value
}
function parseVoice(value: unknown): VoiceProjection {
  const v = record(value)
  const state = v['state']
  if (typeof v['auto_stop_on_silence'] !== 'boolean' || typeof v['auto_start_available'] !== 'boolean') throw new Error('Invalid desktop voice control projection')
  if (state !== 'stopped' && state !== 'starting' && state !== 'active' && state !== 'error') throw new Error('Invalid desktop capture state')
  const confidence = v['wake_confidence']
  if (confidence !== null && (typeof confidence !== 'number' || !Number.isFinite(confidence) || confidence < 0 || confidence > 1)) throw new Error('Invalid wake confidence')
  const partial = v['partial_transcript'] === null ? null : record(v['partial_transcript'])
  return { autoStopOnSilence: v['auto_stop_on_silence'], autoStartAvailable: v['auto_start_available'], captureId: nullableId(v['capture_id']), state, runtimePhase: parseRuntimePhase(v['runtime_phase'], 'voice.runtime_phase'), wakeConfidence: confidence,
    partialTranscript: partial === null ? null : { sessionId: sequence(partial['session_id']), revision: sequence(partial['revision']), text: text(partial['text'], 256 * 1024) },
    error: v['error'] === null ? null : text(v['error'], 4096) }
}
function parseSpeech(value: unknown): SpeechProjection {
  const v = record(value)
  if (typeof v['playing'] !== 'boolean' || typeof v['pending'] !== 'boolean') throw new Error('Invalid desktop speech state')
  return { playbackId: nullableId(v['playback_id']), playing: v['playing'], pending: v['pending'] }
}
function parseUpdate(value: unknown, requestId: string): PromptExecutionEvent {
  const v = record(value)
  if ('request_id' in v) throw new Error('Desktop update must use enclosing request identity')
  return parsePromptExecutionEvent({ ...v, request_id: requestId })
}
function parseResult(value: unknown, requestId: string): PromptExecutionResult {
  const v = record(value)
  const result = typeof v['kind'] === 'string' ? parseCanonicalPromptOutcome(v, requestId) : parsePromptExecutionResult(v)
  if (result.requestId !== requestId || (result.errorMessage !== null && result.errorMessage !== undefined && utf8ByteLength(result.errorMessage) > 4096)) throw new Error('Invalid desktop prompt result')
  return result
}
function parsePrompt(value: unknown): NativePromptProjection {
  const v = record(value)
  const requestId = text(v['request_id'], 128, true)
  const execution = record(v['execution_id'])
  text(execution['session_id'], 64, true)
  text(execution['generation'], 64, true)
  const updates = v['updates']
  if (!Array.isArray(updates) || updates.length > 32) throw new Error('Invalid desktop prompt projection budget')
  const parsed = updates.map((update) => parseUpdate(update, requestId))
  if (parsed.reduce((sum, update) => sum + ('text' in update ? utf8ByteLength(update.text) : 0), 0) > 512 * 1024) throw new Error('Desktop prompt projection exceeds text budget')
  return { requestId, executionId: parseExecutionId(execution, true), prompt: text(v['prompt'], 32 * 1024, true), updates: parsed, result: v['result'] === null ? null : parseResult(v['result'], requestId) }
}
function parseSnapshot(value: unknown): DesktopRuntimeSnapshot {
  const v = record(value)
  return { revision: sequence(v['revision']), voice: parseVoice(v['voice']), prompt: v['prompt'] === null ? null : parsePrompt(v['prompt']), speech: parseSpeech(v['speech']) }
}
function parseEvent(value: unknown): DesktopRuntimeEvent {
  const v = record(value)
  const revision = sequence(v['revision'])
  switch (v['kind']) {
    case 'voice': return { revision, kind: 'voice', voice: parseVoice(v['voice']) }
    case 'speech': return { revision, kind: 'speech', speech: parseSpeech(v['speech']) }
    case 'prompt_started': return { revision, kind: 'prompt_started', prompt: parsePrompt(v['prompt']) }
    case 'prompt_update': {
      const requestId = text(v['request_id'], 128, true)
      return { revision, kind: 'prompt_update', requestId, update: parseUpdate(v['update'], requestId) }
    }
    case 'prompt_finished': {
      const requestId = text(v['request_id'], 128, true)
      return { revision, kind: 'prompt_finished', requestId, result: parseResult(v['result'], requestId) }
    }
    default: throw new Error('Invalid desktop runtime event kind')
  }
}
export function parseDesktopRuntimeRead(value: unknown): DesktopRuntimeRead {
  validateWireBudget(value)
  const v = record(value)
  const revision = sequence(v['revision'])
  const snapshot = v['snapshot'] === null ? null : parseSnapshot(v['snapshot'])
  if (!Array.isArray(v['events']) || v['events'].length > 256) throw new Error('Invalid desktop event budget')
  const events = v['events'].map(parseEvent)
  if (snapshot !== null && snapshot.revision !== revision) throw new Error('Desktop snapshot revision mismatch')
  if (events.some((event, index) => event.revision > revision || (index > 0 && event.revision <= events[index - 1]!.revision))) throw new Error('Desktop events are not ordered')
  return { revision, snapshot, events }
}

function validateWireBudget(value: unknown): void {
  let remainingBytes = 5 * 1024 * 1024
  let remainingNodes = 16_384
  const consume = (bytes: number): void => {
    remainingBytes -= bytes
    if (remainingBytes < 0) throw new Error('Desktop projection exceeds wire budget')
  }
  const visit = (item: unknown, depth: number): void => {
    if (--remainingNodes < 0 || depth > 16) throw new Error('Desktop projection exceeds structural budget')
    if (typeof item === 'string') {
      // Count JSON escaping as well as raw text; bound the temporary encoding
      // by the largest supported text field before allocating it.
      if (item.length > 256 * 1024 || item.length > remainingBytes) throw new Error('Desktop projection exceeds wire budget')
      consume(utf8ByteLength(JSON.stringify(item)))
    } else if (Array.isArray(item)) {
      if (item.length > 256) throw new Error('Desktop projection exceeds array budget')
      consume(2 + Math.max(0, item.length - 1))
      for (const child of item) visit(child, depth + 1)
    } else if (isRecord(item)) {
      const entries = Object.entries(item)
      if (entries.length > 128) throw new Error('Desktop projection exceeds field budget')
      consume(2 + entries.length + Math.max(0, entries.length - 1))
      for (const [key, child] of entries) { visit(key, depth + 1); visit(child, depth + 1) }
    } else if (item !== null && typeof item !== 'boolean' && (typeof item !== 'number' || !Number.isFinite(item))) {
      throw new Error('Desktop projection contains a non-JSON value')
    } else consume(item === null ? 4 : String(item).length)
  }
  visit(value, 0)
}

/** One read and one coalesced invalidation, never a renderer replay queue or timer. */
export class DesktopRuntimeObserver {
  private readonly options: DesktopRuntimeObserverOptions
  private revision: number | null = null
  private desired = 0
  private forceSnapshot = true
  private flight: Promise<DesktopRuntimeSnapshot | null> | null = null
  private stopped = false
  private unlisten: (() => void) | null = null
  private subscription: Promise<void> | null = null
  private epoch = 0
  constructor(options: DesktopRuntimeObserverOptions) { this.options = options }

  start(): Promise<DesktopRuntimeSnapshot | null> { return this.refresh() }
  private subscribe(): Promise<void> {
    if (this.unlisten !== null || this.stopped) return Promise.resolve()
    if (this.subscription !== null) return this.subscription
    this.subscription = this.options.listen((payload) => {
      try {
        const revision = sequence(record(payload)['revision'])
        if (this.stopped || (this.revision !== null && revision <= this.revision)) return
        this.desired = Math.max(this.desired, revision)
        if (this.unlisten !== null) void this.drain().catch(() => undefined)
      } catch { this.invalidate(); this.options.error() }
    }).then((unlisten) => {
      if (this.stopped) unlisten()
      else this.unlisten = unlisten
    }).catch((error: unknown) => {
      if (!this.stopped) this.options.error()
      throw error
    }).finally(() => { this.subscription = null })
    return this.subscription
  }
  async refresh(): Promise<DesktopRuntimeSnapshot | null> {
    await this.subscribe()
    if (this.stopped) return null
    this.forceSnapshot = true
    return this.drain()
  }
  /** Fence an in-flight pre-reset read; only a new authoritative root may publish. */
  invalidate(): void { this.epoch += 1; this.forceSnapshot = true; this.revision = null }
  hasNewerInvalidation(revision: number): boolean { return this.desired > revision }
  stop(): void { this.stopped = true; this.epoch += 1; this.unlisten?.(); this.unlisten = null }
  private drain(): Promise<DesktopRuntimeSnapshot | null> {
    if (this.flight !== null) return this.flight
    this.flight = this.readLoop().then((snapshot) => {
      this.flight = null
      // A callback can invalidate between readLoop's last turn and settlement.
      if (!this.stopped && (this.forceSnapshot || (this.revision !== null && this.desired > this.revision))) {
        return this.drain().then((newer) => newer ?? snapshot)
      }
      return snapshot
    }, (error: unknown) => { this.flight = null; throw error })
    return this.flight
  }
  private async readLoop(): Promise<DesktopRuntimeSnapshot | null> {
    let latest: DesktopRuntimeSnapshot | null = null
    try {
      do {
        const epoch = this.epoch
        const cursor = this.forceSnapshot ? null : this.revision
        this.forceSnapshot = false
        const response = parseDesktopRuntimeRead(await this.options.read(cursor))
        if (this.stopped) return null
        if (epoch !== this.epoch) continue
        if (this.revision !== null && response.revision < this.revision) throw new Error('Desktop runtime revision regressed')
        if (response.snapshot !== null) {
          latest = response.snapshot
          this.options.snapshot(response.snapshot)
        } else {
          if (cursor === null || (response.events.length === 0 ? response.revision !== cursor : response.events[0]!.revision !== cursor + 1 || response.events.at(-1)!.revision !== response.revision) || response.events.some((event, i) => i > 0 && event.revision !== response.events[i - 1]!.revision + 1)) throw new Error('Desktop runtime requires a root snapshot')
          for (const event of response.events) this.options.event(event)
        }
        this.revision = response.revision
        if (cursor === response.revision && this.desired > response.revision) throw new Error('Desktop runtime failed to advance')
      } while (!this.stopped && (this.forceSnapshot || this.revision === null || this.desired > this.revision))
      return latest
    } catch (error) {
      this.revision = null
      this.forceSnapshot = true
      if (!this.stopped) this.options.error()
      throw error
    }
  }
}

interface DesktopRuntimeObserverOptions {
    readonly listen: (callback: (payload: unknown) => void) => Promise<() => void>
    readonly read: (sinceRevision: number | null) => Promise<unknown>
    readonly snapshot: (snapshot: DesktopRuntimeSnapshot) => void
    readonly event: (event: DesktopRuntimeEvent) => void
    readonly error: () => void
}

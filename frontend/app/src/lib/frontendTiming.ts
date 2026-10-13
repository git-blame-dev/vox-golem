import type { CustomDirectTimings } from '../types/chat'
import { invokeTauriCommand, isAndroidRuntime } from './tauri'

type StartupStage = 'SHELL_DOM_COMMITTED' | 'INITIAL_SNAPSHOT_REQUESTED' | 'INITIAL_SNAPSHOT_RECEIVED' | 'INITIAL_SNAPSHOT_APPLIED' | 'INITIAL_SEARCH_MODE_REQUESTED' | 'INITIAL_SEARCH_MODE_ACK' | 'STATE_READY'
type RequestStage = 'ADMITTED' | 'IPC_DISPATCH' | 'FIRST_NONBLANK_RECEIVED' | 'FIRST_TEXT_DOM_COMMITTED' | 'SETTLED'

export function frontendTimingEnabled(): boolean {
  return import.meta.env['VITE_VOXGOLEM_TIMING'] === '1'
}

function reportNativeStage(stage: 'frontend_ready' | 'first_response_dom'): void {
  if (!isAndroidRuntime) return
  // Native reports bridge arrival, not WebView commit time or painted pixels.
  void invokeTauriCommand('report_startup_stage', { stage }).catch(() => undefined)
}

function emit(metadata: Record<string, string | number>): void {
  // A string survives Android's console-to-logcat bridge without remote object inspection.
  try { console.info('[frontend-timing]', JSON.stringify(metadata)) } catch { /* Diagnostics cannot reject chat work. */ }
}

/** One WebView clock: navigation origin is 0; request durations start at admission. */
export class FrontendTiming {
  private readonly startupStages = new Set<StartupStage>()
  private ordinal = 0
  private lastRequest: FrontendRequestTiming | null = null
  private firstResponseReported = false
  private readonly enabled: boolean

  constructor(enabled = frontendTimingEnabled()) { this.enabled = enabled }

  startup(stage: StartupStage): void {
    if (!this.enabled || this.startupStages.has(stage)) return
    this.startupStages.add(stage)
    emit({ scope: 'webview_navigation', stage, atMs: Math.round(performance.now()) })
    if (stage === 'STATE_READY') reportNativeStage('frontend_ready')
  }

  admit(assistantId: string, admittedAt: number): FrontendRequestTiming {
    const request = new FrontendRequestTiming(assistantId, ++this.ordinal, admittedAt, this.enabled)
    if (this.enabled) this.lastRequest = request
    return request
  }

  retireRequest(): void { this.lastRequest = null }

  domCommitted(assistantId: string): void {
    if (!this.enabled || !this.lastRequest?.domCommitted(assistantId)) return
    if (!this.firstResponseReported) {
      this.firstResponseReported = true
      reportNativeStage('first_response_dom')
    }
  }
}

/** Keeps only IDs for private correlation, never response text or public identifiers. */
export class FrontendRequestTiming {
  private canonicalAnswerId: string | null = null
  private firstNonblankAt: number | null = null
  private settledAt: number | null = null
  private readonly stages = new Set<RequestStage>()
  private providerReported = false
  private readonly assistantId: string
  private readonly ordinal: number
  private readonly admittedAt: number
  private readonly enabled: boolean

  constructor(assistantId: string, ordinal: number, admittedAt: number, enabled: boolean) {
    this.assistantId = assistantId
    this.ordinal = ordinal
    this.admittedAt = admittedAt
    this.enabled = enabled
    this.record('ADMITTED', admittedAt)
  }

  private record(stage: RequestStage, at?: number): void {
    if (!this.enabled || this.stages.has(stage)) return
    this.stages.add(stage)
    const timestamp = at ?? performance.now()
    emit({ scope: 'frontend_request', request: this.ordinal, stage,
      atMs: Math.round(timestamp), elapsedMs: Math.max(0, Math.round(timestamp - this.admittedAt)) })
  }

  dispatch(): void { this.record('IPC_DISPATCH') }

  received(text: string): CustomDirectTimings | undefined {
    if (this.firstNonblankAt === null && text.trim().length > 0) {
      this.firstNonblankAt = performance.now()
      this.record('FIRST_NONBLANK_RECEIVED', this.firstNonblankAt)
      return this.badgeTimings()
    }
    return undefined
  }

  canonicalAnswer(id: string): void { this.canonicalAnswerId = id }

  domCommitted(id: string): boolean {
    if (this.firstNonblankAt === null || this.stages.has('FIRST_TEXT_DOM_COMMITTED') ||
      (id !== this.assistantId && id !== this.canonicalAnswerId)) return false
    this.record('FIRST_TEXT_DOM_COMMITTED')
    return true
  }

  settle(): CustomDirectTimings | undefined {
    this.settledAt ??= performance.now()
    this.record('SETTLED', this.settledAt)
    return this.badgeTimings()
  }

  private badgeTimings(): CustomDirectTimings | undefined {
    if (this.firstNonblankAt === null) return undefined
    return {
      firstTextMs: Math.max(0, Math.round(this.firstNonblankAt - this.admittedAt)),
      completedMs: Math.max(0, Math.round((this.settledAt ?? this.firstNonblankAt) - this.admittedAt)),
    }
  }

  provider(timings: CustomDirectTimings): void {
    if (!this.enabled || this.providerReported) return
    this.providerReported = true
    emit({ scope: 'provider_request', request: this.ordinal,
      stage: 'PROVIDER_DURATIONS', firstTextMs: timings.firstTextMs, completedMs: timings.completedMs })
  }
}

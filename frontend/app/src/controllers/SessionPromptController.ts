import type { PromptProjection } from '../SessionAuthority'
import type { AppSnapshot, ExecutionId } from '../lib/appSnapshot'
import { sameExecution } from '../lib/desktopRuntime'
import type { NativePromptProjection } from '../lib/desktopRuntime'
import type { PromptExecutionEvent, PromptExecutionResult } from '../types/chat'

/** Request-scoped presentation resources; SessionAuthority alone publishes state. */
export class SessionPromptController {
  readonly active = { current: null as PromptProjection | null }
  readonly revision = { current: 0 }
  private retiredNative: { readonly requestId: string; readonly executionId: ExecutionId } | null = null
  private native: { readonly requestId: string; readonly executionId: ExecutionId; readonly update: (event: PromptExecutionEvent) => void; readonly finish: (result: PromptExecutionResult) => void; readonly replace: () => void } | null = null

  observeNative(prompt: NativePromptProjection, update: (event: PromptExecutionEvent) => void, replace: () => void): Promise<PromptExecutionResult> {
    this.detachNative()
    return new Promise((finish) => {
      this.native = { requestId: prompt.requestId, executionId: prompt.executionId, update, finish, replace }
      this.replaceNative(prompt)
    })
  }
  replaceNative(prompt: NativePromptProjection): void {
    const native = this.native
    if (native?.requestId !== prompt.requestId || !sameExecution(native.executionId, prompt.executionId)) return
    native.replace()
    for (const update of prompt.updates) native.update(update)
    if (prompt.result !== null) this.finishNative(prompt.requestId, prompt.executionId, prompt.result)
  }
  isNativeRetired(prompt: NativePromptProjection): boolean {
    return this.retiredNative?.requestId === prompt.requestId && sameExecution(this.retiredNative.executionId, prompt.executionId)
  }
  updateNative(requestId: string, executionId: ExecutionId, update: PromptExecutionEvent): void {
    if (this.native?.requestId === requestId && sameExecution(this.native.executionId, executionId)) this.native.update(update)
  }
  finishNative(requestId: string, executionId: ExecutionId, result: PromptExecutionResult): void {
    if (this.native?.requestId !== requestId || !sameExecution(this.native.executionId, executionId)) return
    const native = this.native
    this.native = null
    this.retiredNative = { requestId, executionId }
    native.finish(result)
  }
  detachNative(): void {
    const native = this.native
    this.native = null
    native?.finish({ requestId: native.requestId, outcome: 'cancelled', runtimePhase: null, errorMessage: null })
  }

  schedule(projection: PromptProjection, lifetime: () => boolean, render: () => void): void {
    if (projection.renderFrameId !== null) return
    const revision = ++projection.renderFrameRevision
    const frameId = window.requestAnimationFrame(() => {
      if (projection.renderFrameRevision !== revision || projection.renderFrameId !== frameId) return
      projection.renderFrameId = null
      if (!lifetime() || this.active.current !== projection || projection.terminal) return
      render()
    })
    projection.renderFrameId = frameId
  }

  /** Acknowledgement is not retirement: only an inactive canonical snapshot settles recovery. */
  async reconcile(options: {
    readonly isCurrent: () => boolean
    readonly runtimeRevision: () => number
    readonly load: () => Promise<AppSnapshot>
    readonly requested: () => void
    readonly acceptRetiredRequest?: (snapshot: AppSnapshot) => boolean
  }): Promise<{ readonly snapshot: AppSnapshot; readonly runtimeRevision: number } | null> {
    for (let attempt = 0; attempt < 3; attempt += 1) {
      if (!options.isCurrent()) return null
      try {
        const runtimeRevision = options.runtimeRevision()
        options.requested()
        const snapshot = await options.load()
        if (!options.isCurrent()) return null
        if (snapshot.activeRequest === null || options.acceptRetiredRequest?.(snapshot) === true) return { snapshot, runtimeRevision }
      } catch {
        // Transport failure and still-active native work share the bounded recovery budget.
      }
      if (attempt < 2 && options.isCurrent()) await new Promise((resolve) => window.setTimeout(resolve, 250 * (attempt + 1)))
    }
    return null
  }
}

export function cancelPromptProjectionFrame(projection: PromptProjection): void {
  if (projection.renderFrameId === null) return
  window.cancelAnimationFrame(projection.renderFrameId)
  projection.renderFrameId = null
  projection.renderFrameRevision += 1
}

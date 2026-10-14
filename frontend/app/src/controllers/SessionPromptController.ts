import type { PromptProjection } from '../SessionAuthority'
import type { AppSnapshot } from '../lib/appSnapshot'

/** Request-scoped presentation resources; SessionAuthority alone publishes state. */
export class SessionPromptController {
  readonly active = { current: null as PromptProjection | null }
  readonly revision = { current: 0 }

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
  }): Promise<{ readonly snapshot: AppSnapshot; readonly runtimeRevision: number } | null> {
    for (let attempt = 0; attempt < 3; attempt += 1) {
      if (!options.isCurrent()) return null
      try {
        const runtimeRevision = options.runtimeRevision()
        options.requested()
        const snapshot = await options.load()
        if (!options.isCurrent()) return null
        if (snapshot.activeRequest === null) return { snapshot, runtimeRevision }
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

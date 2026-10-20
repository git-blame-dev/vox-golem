import type { Turn } from '../lib/appSnapshot'

export interface SelectionRecovery { readonly transcriptRevision: number; readonly composerRevision: number }
export interface ShortcutDraft { oldDraft: string; oldSession: string | null; revision: number; needsNew: boolean; prepare: Promise<void> | null }

/** Composer edits and selected context are separate fences, even when both change in one action. */
export class SessionSelectionController {
  readonly composerRevision = { current: 0 }
  readonly contextRevision = { current: 0 }
  readonly canonicalSession = { current: null as string | null }
  readonly transcript = { current: [] as readonly Turn[] }
  readonly shortcutDraft = { current: null as ShortcutDraft | null }
  readonly recovery = { current: null as SelectionRecovery | null }
  readonly draftWrites = { current: Promise.resolve() as Promise<void> }
  readonly pendingDraft = { current: null as string | null }
  readonly draftFailure = { current: null as unknown }

  persistDraft(text: string, options: {
    readonly blocked: () => boolean
    readonly queuedBlocked: () => boolean
    readonly save: (text: string) => Promise<void>
    readonly saved: () => void
    readonly fail: (reason: unknown) => void
  }): void {
    this.pendingDraft.current = text
    if (options.blocked()) return
    const composerRevision = this.composerRevision.current
    const sessionId = this.canonicalSession.current
    const current = (): boolean => composerRevision === this.composerRevision.current && sessionId === this.canonicalSession.current
    this.draftWrites.current = this.draftWrites.current.then(async () => {
      if (options.queuedBlocked() || !current()) return
      await options.save(text)
      if (current()) { this.pendingDraft.current = null; this.draftFailure.current = null }
      options.saved()
    }).catch((reason: unknown) => {
      this.draftFailure.current = reason
      options.fail(reason)
    })
  }
}


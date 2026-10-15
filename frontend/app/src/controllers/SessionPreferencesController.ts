import { DEFAULT_ASSISTANT_SETTINGS } from '../lib/assistantSettings'
import type { UiTextSize, UiTheme } from '../types/chat'

/** These preferences agree on FIFO writes, latest optimistic revision and persisted rollback. */
class PreferenceJournal<T> {
  readonly revision = { current: 0 }
  readonly persistedRevision = { current: 0 }
  readonly persisted: { current: T }
  readonly initialized = { current: false }
  readonly pending = { current: null as number | null }
  readonly writes = { current: Promise.resolve() as Promise<void> }

  constructor(initial: T) { this.persisted = { current: initial } }

  enqueue<R>(save: () => Promise<R>): Promise<R> {
    const write = this.writes.current.then(save)
    this.writes.current = write.then(() => undefined, () => undefined)
    return write
  }
}

export class SessionPreferencesController {
  readonly assistant = new PreferenceJournal(DEFAULT_ASSISTANT_SETTINGS)
  readonly instantRevision = { current: 0 }
  readonly textSize = new PreferenceJournal<UiTextSize>('medium')
  readonly theme = new PreferenceJournal<UiTheme>('dark')
  readonly tts = new PreferenceJournal(false)
}

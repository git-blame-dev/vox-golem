import { useCallback, useSyncExternalStore } from 'react'
import { sessionAuthority } from './SessionAuthority'
import type { SessionAuthority } from './SessionAuthority'
import type { FrontendSessionIntents } from './FrontendSessionRuntime'

/** Read-only app boundary. Runtime mutation and resource access stay private to its owner. */
export class FrontendSession {
  readonly #authority: SessionAuthority

  constructor(options?: ConstructorParameters<typeof SessionAuthority>[0]) {
    this.#authority = sessionAuthority(this, options)
  }

  subscribe = (listener: () => void) => this.#authority.subscribe(listener)
  getControlsSnapshot = () => this.#authority.getControlsSnapshot()
  getMessagesSnapshot = () => this.#authority.getMessagesSnapshot()
  getAnnouncementSnapshot = () => this.#authority.getAnnouncementSnapshot()
  getToolbarSnapshot = () => this.#authority.getToolbarSnapshot()
  getComposerSnapshot = () => this.#authority.getComposerSnapshot()
  getSettingsSnapshot = () => this.#authority.getSettingsSnapshot()
  getModelsSnapshot = () => this.#authority.getModelsSnapshot()
  get intents(): FrontendSessionIntents { return this.#authority.getIntents<FrontendSessionIntents>() }
  getViewSnapshot = <N extends Parameters<SessionAuthority['getViewSnapshot']>[0]>(name: N) => this.#authority.getViewSnapshot(name)
}

export function useSessionView<N extends Parameters<SessionAuthority['getViewSnapshot']>[0]>(session: FrontendSession, name: N) {
  const snapshot = useCallback(() => session.getViewSnapshot(name), [name, session])
  return useSyncExternalStore(session.subscribe, snapshot, snapshot)
}

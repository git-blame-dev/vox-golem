import { describe, expect, it, vi } from 'vitest'
import { FrontendSession } from './FrontendSession'
import { SessionAuthority } from './SessionAuthority'
import type { StartupState } from './types/chat'

describe('FrontendSession transition authority', () => {
  it('constructs without native calls and atomically publishes both domains with live deferred-callback accessors', async () => {
    const native = vi.fn()
    const prior = window.__TAURI_INTERNALS__
    window.__TAURI_INTERNALS__ = { invoke: native }
    try {
      const session = new SessionAuthority()
      expect(native).not.toHaveBeenCalled()
      const initial = session.getControlsSnapshot()
      const composer = session.fieldRef('composerValue')
      const observations: unknown[] = []
      const unsubscribe = session.subscribe(() => observations.push(session.getControlsSnapshot()))
      session.transaction(() => {
        session.fieldState('runtimeStatus')[1]('sleeping')
        session.fieldState('composerValue')[1]('Synthetic owned draft')
        expect(composer.current).toBe('Synthetic owned draft')
        expect(observations).toHaveLength(0)
      })
      expect(observations).toHaveLength(1)
      expect(observations[0]).toMatchObject({ runtimeStatus: 'sleeping', composerValue: 'Synthetic owned draft' })
      expect(initial).toMatchObject({ runtimeStatus: 'initializing', composerValue: '' })
      await Promise.resolve()
      expect(observations).toHaveLength(1)
      unsubscribe()
    } finally {
      if (prior === undefined) delete window.__TAURI_INTERNALS__
      else window.__TAURI_INTERNALS__ = prior
    }
  })

  it('keeps independent invalidation counters and retires the active lifetime once before replay activation', () => {
    const session = new SessionAuthority()
    const context = session.operationRef('contextRevision', () => 0)
    const composer = session.operationRef('composerRevision', () => 0)
    composer.current += 1
    expect(context.current).toBe(0)
    expect(session.operationRef('composerRevision', () => 99)).toBe(composer)
    const retire = vi.fn(() => expect(session.activeRef.current).toBe(false))
    session.activate(retire)
    session.dispose()
    session.dispose()
    expect(retire).toHaveBeenCalledTimes(1)
    session.activate(retire)
    expect(session.activeRef.current).toBe(true)
    session.dispose()
    expect(retire).toHaveBeenCalledTimes(2)
  })

  it('does not revive callbacks captured before StrictMode disposal and replay activation', () => {
    const session = new SessionAuthority()
    session.activate()
    const oldCallbackIsCurrent = session.captureLifetime()
    expect(oldCallbackIsCurrent()).toBe(true)
    session.dispose()
    session.activate()
    expect(oldCallbackIsCurrent()).toBe(false)
    expect(session.captureLifetime()()).toBe(true)
    session.dispose()
  })

  it('exposes no state writers, resource refs or pending seeds through the app facade', () => {
    const session = new FrontendSession()
    expect('fieldState' in session).toBe(false)
    expect('fieldRef' in session).toBe(false)
    expect('operationRef' in session).toBe(false)
    expect(session.getControlsSnapshot()).toBe(session.getControlsSnapshot())
  })

  it('publishes transcript and controls together while admission reads the fresh uncommitted context', async () => {
    const session = new SessionAuthority({ snapshotAvailable: false })
    session.activate()
    const startup: StartupState = {
      kind: 'ready', cueAssetPaths: { startListening: 'synthetic-start.wav', stopListening: 'synthetic-stop.wav' },
      runtimePhase: 'sleeping', voiceInputAvailable: false, voiceInputError: null, initialSilenceTimeoutMs: 2500,
      silenceTimeoutMs: 750, selectedResponseProfile: 'fast', supportedResponseProfiles: ['fast'],
      promptCancellationAvailable: true, ttsEnabled: false, ttsOutputGainDb: 3,
      capabilities: [{ id: 'local_fast', state: 'available', reason: 'Synthetic available provider', actualProvider: null }],
    }
    session.transaction(() => {
      session.fieldState('startupState')[1](startup)
      session.fieldState('runtimeStatus')[1]('sleeping')
      session.fieldState('assistantSettings')[1]((current) => ({ ...current, instant: 'local-fast' }))
    })
    const controls = session.getControlsSnapshot()
    const transcript = session.getMessagesSnapshot()
    const models = session.getModelsSnapshot()
    session.fieldState('snapshotControlsFresh')[1](false)
    session.fieldState('messages')[1]([{ id: 'synthetic-turn', role: 'user', content: 'Synthetic atomic turn' }])
    expect(session.getControlsSnapshot()).toBe(controls)
    expect(session.getMessagesSnapshot()).toBe(transcript)
    expect(session.admitPrompt('Synthetic prompt', 'typed', undefined, false)).toBeNull()
    await Promise.resolve()
    expect(session.getControlsSnapshot().snapshotControlsFresh).toBe(false)
    expect(session.getMessagesSnapshot()[0]?.content).toBe('Synthetic atomic turn')
    expect(session.getModelsSnapshot()).toBe(models)
    session.fieldState('snapshotControlsFresh')[1](true)
    const admission = session.admitPrompt('Synthetic prompt', 'typed', undefined, false)
    expect(admission).not.toBeNull()
    expect(session.admitPrompt('Synthetic second same-tick prompt', 'typed', undefined, false)).toBeNull()
    session.dispose()
    session.activate()
    expect(admission?.isCurrent()).toBe(false)
    session.dispose()
  })
})

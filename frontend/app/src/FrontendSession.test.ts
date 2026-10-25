import { describe, expect, it, vi } from 'vitest'
import { FrontendSession } from './FrontendSession'
import { SessionAuthority, useSessionView } from './SessionAuthority'
import type { StartupState } from './types/chat'
import type { ModelOption } from './lib/appSnapshot'
import { act, createElement, useSyncExternalStore } from 'react'
import { createRoot } from 'react-dom/client'

describe('FrontendSession transition authority', () => {
  it.each([false, true])('keeps named React views stable without coalescing accepted controls (android=%s)', async (android) => {
    const session = new SessionAuthority({ android })
    const names = ['runtime', 'toolbar', 'composer', 'settings'] as const
    const renders = Object.fromEntries(names.map((name) => [name, 0]))
    const startup: StartupState = {
      kind: 'ready', cueAssetPaths: { startListening: 'synthetic-start.wav', stopListening: 'synthetic-stop.wav' },
      runtimePhase: 'sleeping', voiceInputAvailable: false, voiceInputError: null, initialSilenceTimeoutMs: 2500,
      silenceTimeoutMs: 750, selectedResponseProfile: 'fast', supportedResponseProfiles: ['fast', 'quality'],
      promptCancellationAvailable: true, ttsEnabled: false, ttsOutputGainDb: 3,
      capabilities: [{ id: 'local_fast', state: 'available', reason: 'Synthetic ready', actualProvider: null }],
      speechServices: {
        stt: { configuredEngine: null, runtimeEngine: null, configuredModel: null, configuredVoice: null, configuredLocale: null },
        tts: { configuredEngine: 'synthetic-engine', runtimeEngine: null, configuredModel: null, configuredVoice: null, configuredLocale: null },
      },
    }
    const models: ModelOption[] = [{ id: 'local-fast', label: 'Synthetic model', available: true, unavailableReason: null }]
    const accept = (state: StartupState, inventory: ModelOption[] | null) => session.transaction(() => {
      session.fieldState('startupState')[1](state)
      session.fieldState('snapshotModelOptions')[1](inventory)
    })
    accept(startup, models)
    function Subscriber({ name }: { name: typeof names[number] }) {
      useSessionView(session, name)
      renders[name]! += 1
      return null
    }
    const root = createRoot(document.createElement('div'))
    try {
      await act(async () => { root.render(names.map((name) => createElement(Subscriber, { key: name, name }))) })
      const initialRenders = { ...renders }
      const snapshots = names.map((name) => session.getViewSnapshot(name))
      for (let repeat = 0; repeat < 2; repeat += 1) {
        const fresh = structuredClone(startup)
        const freshModels = structuredClone(models)
        await act(async () => { accept(fresh, freshModels) })
        expect(session.fieldRef('startupState').current).toBe(fresh)
        expect(session.fieldRef('snapshotModelOptions').current).toBe(freshModels)
        expect(session.getControlsSnapshot().startupState).toBe(fresh)
        expect(session.getControlsSnapshot().snapshotModelOptions).toBe(freshModels)
        names.forEach((name, index) => expect(session.getViewSnapshot(name)).toBe(snapshots[index]))
        expect(renders).toEqual(initialRenders)
      }
      await act(async () => { accept({ ...startup, capabilities: [{ ...startup.capabilities[0]!, reason: 'Synthetic changed reason' }] }, models) })
      names.forEach((name) => expect(renders[name]).toBe(initialRenders[name]! + 1))
    } finally {
      await act(async () => { root.unmount() })
    }
  })

  it('invalidates named views for meaningful startup metadata, variants and inventory boundaries', () => {
    const session = new SessionAuthority()
    const names = ['runtime', 'toolbar', 'composer', 'settings'] as const
    const publish = (startup: StartupState) => {
      const prior = names.map((name) => session.getViewSnapshot(name))
      session.transaction(() => session.fieldState('startupState')[1](startup))
      names.forEach((name, index) => expect(session.getViewSnapshot(name)).not.toBe(prior[index]))
    }
    const ready: Extract<StartupState, { kind: 'ready' }> = {
      kind: 'ready', cueAssetPaths: { startListening: 'synthetic-start.wav', stopListening: 'synthetic-stop.wav' },
      runtimePhase: 'sleeping', voiceInputAvailable: false, voiceInputError: null, initialSilenceTimeoutMs: 2500,
      silenceTimeoutMs: 750, selectedResponseProfile: 'fast', supportedResponseProfiles: ['fast', 'quality'],
      promptCancellationAvailable: true, ttsEnabled: false, ttsOutputGainDb: 3,
      capabilities: [{ id: 'local_fast', state: 'available', reason: 'Synthetic ready', actualProvider: null }],
    }
    publish(ready)
    const changes: Partial<typeof ready>[] = [
      { capabilities: [{ ...ready.capabilities[0]!, actualProvider: 'synthetic-provider' }] },
      { capabilities: [{ ...ready.capabilities[0]!, state: 'warming' }] },
      { capabilities: [{ ...ready.capabilities[0]!, id: 'local_quality' }] },
      { capabilities: [...ready.capabilities, { ...ready.capabilities[0]!, id: 'custom_provider' }] },
      { cueAssetPaths: { ...ready.cueAssetPaths, stopListeningShort: 'synthetic-short.wav' } },
      { initialSilenceTimeoutMs: 3000 }, { silenceTimeoutMs: 900 },
      { selectedResponseProfile: 'quality' }, { supportedResponseProfiles: ['quality', 'fast'] },
      { speechServices: {
        stt: { configuredEngine: null, runtimeEngine: null, configuredModel: null, configuredVoice: null, configuredLocale: null },
        tts: { configuredEngine: null, runtimeEngine: null, configuredModel: null, configuredVoice: null, configuredLocale: 'synthetic-locale' },
      } },
    ]
    for (const change of changes) { publish({ ...ready, ...change }); publish(ready) }
    const capabilities = [...ready.capabilities, { ...ready.capabilities[0]!, id: 'custom_provider' as const }]
    publish({ ...ready, capabilities })
    publish({ ...ready, capabilities: [...capabilities].reverse() })
    const service = { configuredEngine: null, runtimeEngine: null, configuredModel: null, configuredVoice: null, configuredLocale: null }
    const speechServices = { stt: service, tts: service }
    publish({ ...ready, speechServices })
    for (const update of [
      { runtimeEngine: 'synthetic-runtime' }, { configuredEngine: 'synthetic-engine' },
      { configuredModel: 'synthetic-model' }, { configuredVoice: 'synthetic-voice' }, { configuredLocale: 'synthetic-locale' },
    ]) {
      publish({ ...ready, speechServices: { stt: { ...service, ...update }, tts: service } })
      publish({ ...ready, speechServices: { stt: service, tts: { ...service, ...update } } })
    }
    publish(ready)
    publish({ ...ready, kind: 'warming_model', message: 'Synthetic warming' })
    publish({ ...ready, kind: 'warming_model', message: 'Synthetic progress' })
    publish({ kind: 'error', message: 'Synthetic error' })
    publish({ kind: 'error', message: 'Synthetic other error' })
    publish({ kind: 'loading' })
    const model: ModelOption = { id: 'local-fast', label: 'Synthetic model', available: true, unavailableReason: null }
    for (const inventory of [[], [model], [{ ...model, label: 'Synthetic renamed' }], [{ ...model, available: false }], [{ ...model, unavailableReason: 'Synthetic blocked' }],
      [model, { ...model, id: 'local-quality' }], [{ ...model, id: 'local-quality' }, model], [], null] satisfies (ModelOption[] | null)[]) {
      const prior = names.map((name) => session.getViewSnapshot(name))
      session.transaction(() => session.fieldState('snapshotModelOptions')[1](inventory))
      names.forEach((name, index) => expect(session.getViewSnapshot(name)).not.toBe(prior[index]))
    }
  })

  it.each([false, true])('retains model snapshots and subscriber renders for equivalent fresh accepted controls (android=%s)', async (android) => {
    const session = new SessionAuthority({ android })
    const models: ModelOption[] = [{ id: 'local-fast', label: 'Synthetic model', available: true, unavailableReason: null }]
    const accept = () => session.transaction(() => {
      session.fieldState('startupState')[1]({ kind: 'loading' })
      session.fieldState('snapshotModelOptions')[1](models.map((model) => ({ ...model })))
    })
    accept()
    const snapshot = session.getModelsSnapshot()
    const controls = session.getControlsSnapshot()
    let renders = 0
    function ModelsSubscriber() {
      useSyncExternalStore(session.subscribe, session.getModelsSnapshot)
      renders += 1
      return null
    }
    const container = document.createElement('div')
    const root = createRoot(container)
    try {
      await act(async () => { root.render(createElement(ModelsSubscriber)) })
      const initialRenders = renders
      await act(async () => { accept() })
      expect(session.getControlsSnapshot()).not.toBe(controls)
      expect(session.getControlsSnapshot().snapshotModelOptions).not.toBe(controls.snapshotModelOptions)
      expect(session.getModelsSnapshot()).toBe(snapshot)
      expect(renders).toBe(initialRenders)
    } finally {
      await act(async () => { root.unmount() })
    }
  })

  it('invalidates model projection for inventory fields, order, catalog and capabilities', () => {
    const session = new SessionAuthority()
    const first: ModelOption = { id: 'local-fast', label: 'Synthetic fast', available: true, unavailableReason: null }
    const second: ModelOption = { id: 'local-quality', label: 'Synthetic quality', available: false, unavailableReason: 'Synthetic unavailable' }
    const publish = (models: ModelOption[] | null) => {
      const prior = session.getModelsSnapshot()
      session.transaction(() => session.fieldState('snapshotModelOptions')[1](models))
      const next = session.getModelsSnapshot()
      expect(next).not.toBe(prior)
      return next
    }
    publish([first, second])
    expect(publish([{ ...first, label: 'Synthetic renamed' }, second]).instantAssistantOptions.find((o) => o.value === first.id)?.label).toBe('Synthetic renamed')
    publish([first, second])
    expect(publish([{ ...first, available: false, unavailableReason: 'Synthetic blocked' }, second]).instantAssistantOptions.find((o) => o.value === first.id)).toMatchObject({ available: false, reason: 'Synthetic blocked' })
    expect(publish([{ ...first, available: false, unavailableReason: 'Synthetic changed reason' }, second]).instantAssistantOptions.find((o) => o.value === first.id)?.reason).toBe('Synthetic changed reason')
    publish([second, first])
    publish([first])
    publish(null)
    const prior = session.getModelsSnapshot()
    session.transaction(() => session.fieldState('startupState')[1]({
      kind: 'ready', cueAssetPaths: { startListening: 'synthetic-start.wav', stopListening: 'synthetic-stop.wav' },
      runtimePhase: 'sleeping', voiceInputAvailable: false, voiceInputError: null, initialSilenceTimeoutMs: 2500,
      silenceTimeoutMs: 750, selectedResponseProfile: 'fast', supportedResponseProfiles: ['fast'],
      promptCancellationAvailable: true, ttsEnabled: false, ttsOutputGainDb: 3,
      capabilities: [{ id: 'custom_provider', state: 'available', reason: 'Synthetic available', actualProvider: null }],
    }))
    expect(session.getModelsSnapshot()).not.toBe(prior)
    expect(session.getModelsSnapshot().assistantCapabilities.custom).toBe(true)
    const capable = session.getModelsSnapshot()
    const startup = session.getControlsSnapshot().startupState
    if (startup.kind !== 'ready') throw new Error('Expected synthetic ready state')
    session.transaction(() => session.fieldState('startupState')[1]({
      ...startup, ttsEnabled: true, capabilities: startup.capabilities.map((capability) => ({ ...capability })),
    }))
    expect(session.getModelsSnapshot()).toBe(capable)
    session.transaction(() => session.fieldState('startupState')[1]({ ...startup, capabilities: [] }))
    expect(session.getModelsSnapshot()).not.toBe(capable)
    expect(session.getModelsSnapshot().assistantCapabilities.custom).toBe(false)
  })

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
    const context = session.selection.contextRevision
    const composer = session.selection.composerRevision
    composer.current += 1
    expect(context.current).toBe(0)
    expect(session.selection.composerRevision).toBe(composer)
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

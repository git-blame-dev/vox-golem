import { useCallback, useLayoutEffect, useRef, useSyncExternalStore } from 'react'
import type { Dispatch, SetStateAction } from 'react'
import { DEFAULT_ASSISTANT_SETTINGS, deepOptions, instantOptions, reviewOptions } from './lib/assistantSettings'
import type { AssistantCapabilities, AssistantOption, AssistantSettings } from './lib/assistantSettings'
import type { AndroidAssistantProjection, ModelOption } from './lib/appSnapshot'
import type { AndroidVoiceStatus } from './lib/androidVoice'
import type { AudioInputDevice } from './lib/liveAudioSource'
import type { SearchMode } from './lib/searchMode'
import type { ChatMessage, CueAssetPaths, RuntimeStatus, StartupState, UiTextSize, UiTheme, UserNotice } from './types/chat'
import { getInitialMessages } from './state/appShell'
import { applySnapshotModelOptions, capabilityMap, instantMatchesResponseProfile } from './sessionPolicy'
import { admittedSearchMode } from './lib/searchMode'
import { transitionRuntimeStatus } from './state/runtimeMachine'
import type { AnswerSource, AnswerStageStatusEntry, AnswerPriorVersion } from './components/AnswerStage'

export type PromptProjection = {
  id: string
  userId: string
  assistantId: string
  text: string
  error: string | null
  terminal: boolean
  presentationCancelled: boolean
  cancelPending: boolean
  tts: boolean
  corrected: boolean
  stages: AnswerStageStatusEntry[]
  priorVersions: AnswerPriorVersion[]
  sources: AnswerSource[]
  ttsFirstLineSpoken: boolean
  renderFrameId: number | null
  renderFrameRevision: number
  customDirectTimings?: import('./types/chat').CustomDirectTimings
  admittedAt?: number
}

export interface RuntimeProjection {
  readonly startupState: StartupState
  readonly runtimeStatus: RuntimeStatus
  readonly assistantSettings: Readonly<AssistantSettings>
  readonly assistantSettingsPending: boolean
  readonly assistantSettingsLoadError: string | null
  readonly assistantSettingsWriteError: string | null
  readonly snapshotHydrationState: 'unavailable' | 'pending' | 'failed' | 'succeeded'
  readonly snapshotControlsFresh: boolean
  readonly snapshotControlsInvalidated: boolean
  readonly snapshotModelOptions: readonly ModelOption[] | null
  readonly searchMode: SearchMode
  readonly searchModePending: boolean
  readonly uiTextSize: UiTextSize
  readonly uiTheme: UiTheme
  readonly ttsEnabled: boolean
  readonly autoStopOnSilence: boolean
  readonly preparedCueAssetPaths: CueAssetPaths | null
  readonly audioInputDeviceId: string | null
  readonly audioInputDevices: readonly AudioInputDevice[]
}

export interface ConversationInteraction {
  readonly composerValue: string
  readonly shortcutStaging: boolean
  readonly androidAssistant: AndroidAssistantProjection | null
  readonly androidConversationId: string
  readonly conversationPagePending: boolean
  readonly headPagePending: boolean
  readonly androidVoiceActive: boolean
  readonly androidVoiceStatus: AndroidVoiceStatus | null
  readonly androidSpeechStatusError: string | null
  readonly partialTranscript: string
  readonly typedCompletionSuffix: string
  readonly voiceCompletionSuffix: string
  readonly ttsPlaying: boolean
  readonly pendingTtsCommands: number
  readonly wakeConfidence: number | null
  readonly isSwitchingResponseProfile: boolean
  readonly micStarting: boolean
  readonly micActive: boolean
  readonly voiceNotesOpen: boolean
  readonly customRetryPending: boolean
  readonly notices: readonly UserNotice[]
  readonly noticeInteraction: { readonly focused: boolean; readonly hovered: boolean }
  readonly messages: readonly ChatMessage[]
  readonly responseAnnouncement: string
  readonly promptActivity: { readonly text: string; readonly compact: boolean } | null
  readonly promptState: 'idle' | 'executing' | 'stopping' | 'reconciling'
  readonly resetPending: boolean
}

export type SessionState = RuntimeProjection & ConversationInteraction
type State = { readonly runtimeProjection: RuntimeProjection; readonly conversationInteraction: ConversationInteraction }
type Cell<T> = { current: T }
type MutableProjection<T> = { -readonly [K in keyof T]?: T[K] }
export type ControlsSnapshot = Omit<SessionState, 'messages' | 'responseAnnouncement'>
export interface ModelsSnapshot {
  readonly assistantCapabilities: Readonly<AssistantCapabilities>
  readonly instantAssistantOptions: readonly Readonly<AssistantOption<AssistantSettings['instant']>>[]
  readonly deepAssistantOptions: readonly Readonly<AssistantOption<AssistantSettings['deep']>>[]
  readonly reviewAssistantOptions: readonly Readonly<AssistantOption<AssistantSettings['review']>>[]
}

const VIEW_KEYS = {
  shell: ['uiTextSize', 'uiTheme', 'runtimeStatus'],
  conversation: ['startupState', 'customRetryPending', 'voiceNotesOpen', 'shortcutStaging', 'composerValue', 'uiTextSize', 'notices'],
  toolbar: ['startupState', 'runtimeStatus', 'assistantSettings', 'assistantSettingsPending',
    'snapshotHydrationState', 'snapshotControlsFresh', 'snapshotControlsInvalidated', 'snapshotModelOptions',
    'searchMode', 'searchModePending', 'androidAssistant', 'androidConversationId', 'androidVoiceActive',
    'voiceNotesOpen', 'promptState', 'promptActivity', 'resetPending', 'isSwitchingResponseProfile',
    'micStarting', 'micActive', 'headPagePending', 'shortcutStaging', 'uiTheme'],
  composer: ['startupState', 'runtimeStatus', 'assistantSettings', 'assistantSettingsPending',
    'assistantSettingsLoadError', 'snapshotControlsFresh', 'snapshotModelOptions', 'searchMode',
    'searchModePending', 'composerValue', 'partialTranscript', 'typedCompletionSuffix', 'voiceCompletionSuffix',
    'androidAssistant', 'androidVoiceActive', 'conversationPagePending', 'headPagePending', 'voiceNotesOpen',
    'promptState', 'promptActivity', 'resetPending', 'micStarting', 'micActive', 'shortcutStaging',
    'uiTextSize', 'wakeConfidence', 'isSwitchingResponseProfile', 'snapshotControlsInvalidated'],
  settings: ['startupState', 'runtimeStatus', 'assistantSettings', 'assistantSettingsPending',
    'assistantSettingsLoadError', 'assistantSettingsWriteError', 'snapshotControlsFresh', 'snapshotModelOptions',
    'uiTextSize', 'uiTheme', 'ttsEnabled', 'autoStopOnSilence', 'audioInputDevices', 'audioInputDeviceId',
    'androidVoiceStatus', 'androidSpeechStatusError', 'androidVoiceActive', 'promptState', 'resetPending',
    'isSwitchingResponseProfile', 'micStarting', 'ttsPlaying', 'pendingTtsCommands', 'searchModePending', 'androidAssistant'],
} as const satisfies Record<string, readonly (keyof ControlsSnapshot)[]>
type ViewName = keyof typeof VIEW_KEYS
type ViewSnapshot<N extends ViewName> = Pick<ControlsSnapshot, (typeof VIEW_KEYS)[N][number]>

/** Instance-local transition authority. Construction neither starts resources nor performs I/O. */
export class SessionAuthority {
  private state: State
  private controls: ControlsSnapshot
  private messages: readonly ChatMessage[] = getInitialMessages()
  private announcement = ''
  private readonly listeners = new Set<() => void>()
  private readonly fields = new Map<keyof SessionState, Cell<unknown>>()
  private readonly setters = new Map<keyof SessionState, unknown>()
  private readonly resources = new Map<string, Cell<unknown>>()
  private readonly views = new Map<ViewName, { source: ControlsSnapshot; values: readonly unknown[]; snapshot: unknown }>()
  private publicationQueued = false
  private publicationDirty = false
  private transactionDepth = 0
  private controlsChanged = false
  private active = false
  private lifecycle = 0
  private retirement: (() => void) | null = null
  private promptAdmission: { readonly isCurrent: () => boolean } | null = null
  private readonly android: boolean
  private modelProjection: {
    readonly startup: StartupState
    readonly models: readonly ModelOption[] | null
    readonly value: ReturnType<SessionAuthority['projectModels']>
  } | null = null
  readonly activeRef: { readonly current: boolean }

  constructor({ snapshotAvailable = true, nativeSearch = false, android = false, audioInputDeviceId = null }: { snapshotAvailable?: boolean; nativeSearch?: boolean; android?: boolean; audioInputDeviceId?: string | null } = {}) {
    this.android = android
    this.state = {
      runtimeProjection: {
        startupState: { kind: 'loading' }, runtimeStatus: 'initializing',
        assistantSettings: DEFAULT_ASSISTANT_SETTINGS, assistantSettingsPending: snapshotAvailable,
        assistantSettingsLoadError: null, assistantSettingsWriteError: null,
        snapshotHydrationState: snapshotAvailable ? 'pending' : 'unavailable',
        snapshotControlsFresh: !snapshotAvailable, snapshotControlsInvalidated: false,
        snapshotModelOptions: null, searchMode: 'quick', searchModePending: nativeSearch,
        uiTextSize: 'medium', uiTheme: 'dark', ttsEnabled: false, autoStopOnSilence: true,
        preparedCueAssetPaths: null, audioInputDeviceId, audioInputDevices: [],
      },
      conversationInteraction: {
        composerValue: '', shortcutStaging: false, androidAssistant: null, androidConversationId: '0',
        conversationPagePending: false, headPagePending: false, androidVoiceActive: false,
        androidVoiceStatus: null, androidSpeechStatusError: null, partialTranscript: '',
        typedCompletionSuffix: '', voiceCompletionSuffix: '', ttsPlaying: false, pendingTtsCommands: 0,
        wakeConfidence: null, isSwitchingResponseProfile: false, micStarting: false, micActive: false,
        voiceNotesOpen: false, customRetryPending: false, notices: [],
        noticeInteraction: { focused: false, hovered: false }, messages: getInitialMessages(),
        responseAnnouncement: '', promptActivity: null, promptState: 'idle', resetPending: false,
      },
    }
    this.controls = this.projectControls()
    const isActive = () => this.active
    this.activeRef = { get current() { return isActive() } }
  }

  activate(retirement?: () => void): void {
    this.lifecycle += 1
    this.active = true
    this.retirement = retirement ?? null
  }
  dispose(): void {
    if (!this.active) return
    this.active = false
    this.lifecycle += 1
    const retire = this.retirement
    this.retirement = null
    retire?.()
    this.promptAdmission = null
  }

  /** Capture at operation admission, never re-read the epoch when a callback arrives. */
  captureLifetime(): () => boolean {
    const lifecycle = this.lifecycle
    return () => this.active && this.lifecycle === lifecycle
  }

  /** Admission reads the atomic accepted context, not a render's captured props. */
  admitPrompt(prompt: string, source: 'typed' | 'voice', nativeRequestId: string | undefined, updateBlocked: boolean) {
    const runtime = this.state.runtimeProjection
    const interaction = this.state.conversationInteraction
    const startup = runtime.startupState
    const settings = runtime.assistantSettings
    const selected = startup.kind === 'ready' ? applySnapshotModelOptions(
      instantOptions(capabilityMap(startup)), runtime.snapshotModelOptions,
    ).find((option) => option.value === settings.instant) : undefined
    const shortcut = this.resources.get('shortcutDraft')?.current as { readonly needsNew: boolean } | null | undefined
    if (!this.active || this.promptAdmission !== null || prompt.trim().length === 0 || startup.kind !== 'ready' ||
      updateBlocked || selected?.available !== true || !instantMatchesResponseProfile(startup, settings.instant) ||
      runtime.assistantSettingsPending || runtime.searchModePending || !runtime.snapshotControlsFresh ||
      interaction.isSwitchingResponseProfile || interaction.resetPending ||
      shortcut?.needsNew === true ||
      this.resources.get('selectionRecovery')?.current != null ||
      (source === 'typed' && nativeRequestId === undefined && interaction.androidVoiceActive) ||
      (interaction.androidAssistant?.branchChoices.length ?? 0) > 0 ||
      ['saved_local_state_failed', 'draft_not_saved', 'storage_error'].includes(interaction.androidAssistant?.error ?? '') ||
      this.resources.get('prompt')?.current != null) return null
    const executingStatus = transitionRuntimeStatus(runtime.runtimeStatus, 'submit_prompt')
    if (executingStatus === runtime.runtimeStatus) return null
    const admission = {
      isCurrent: this.captureLifetime(), startup, settings, executingStatus,
      mode: admittedSearchMode(runtime.searchMode, settings, true),
    }
    this.promptAdmission = admission
    this.transaction(() => {
      this.transition('promptState', 'executing')
      this.transition('promptActivity', { text: 'Executing prompt…', compact: true })
    })
    return admission
  }

  settlePromptAdmission(admission: { readonly isCurrent: () => boolean }): void {
    if (this.promptAdmission === admission) this.promptAdmission = null
  }

  retirePromptAdmission(): void {
    this.promptAdmission = null
  }

  stopPromptPresentation(): { readonly requestId: string; readonly recover: () => void } | null {
    const prompt = this.resources.get('prompt')?.current as PromptProjection | null | undefined
    if (prompt == null || prompt.terminal || prompt.cancelPending) return null
    prompt.cancelPending = true
    prompt.presentationCancelled = true
    cancelPromptProjectionFrame(prompt)
    this.transition('promptState', 'stopping')
    const isCurrent = this.captureLifetime()
    return { requestId: prompt.id, recover: () => {
      if (!isCurrent() || this.resources.get('prompt')?.current !== prompt || prompt.terminal) return
      prompt.cancelPending = false
      // A rejected command does not revive already-fenced presentation callbacks.
      // The original request still owns native settlement and canonical recovery.
      this.transition('promptState', 'executing')
    } }
  }

  renderPromptProjection(projection: PromptProjection): void {
    if (!this.active || projection.presentationCancelled) return
    const projectedMessage: ChatMessage = {
      id: projection.assistantId, role: 'assistant', content: projection.text,
      ...(projection.customDirectTimings === undefined ? {} : { customDirectTimings: projection.customDirectTimings }),
      // The arrays are replaced at their own semantic transitions, never copied per token.
      answerStage: { stages: projection.stages, priorVersions: projection.priorVersions, sources: projection.sources },
    }
    this.transition('messages', (current) => current.some((message) => message.id === projection.assistantId)
      ? current.map((message) => message.id === projection.assistantId ? { ...message, ...projectedMessage } : message)
      : [...current, projectedMessage])
  }

  flushPromptProjection(projection: PromptProjection): void {
    cancelPromptProjectionFrame(projection)
    this.renderPromptProjection(projection)
  }

  schedulePromptProjection(projection: PromptProjection): void {
    if (projection.renderFrameId !== null) return
    const revision = ++projection.renderFrameRevision
    const isCurrent = this.captureLifetime()
    const frameId = window.requestAnimationFrame(() => {
      if (projection.renderFrameRevision !== revision || projection.renderFrameId !== frameId) return
      projection.renderFrameId = null
      if (!isCurrent() || this.resources.get('prompt')?.current !== projection || projection.terminal) return
      this.renderPromptProjection(projection)
    })
    projection.renderFrameId = frameId
  }

  transaction<T>(intent: () => T): T {
    this.transactionDepth += 1
    try { return intent() } finally {
      this.transactionDepth -= 1
      if (this.transactionDepth === 0) this.publish()
    }
  }

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener)
    return () => { this.listeners.delete(listener) }
  }
  getControlsSnapshot = (): ControlsSnapshot => this.controls
  getMessagesSnapshot = (): readonly ChatMessage[] => this.messages
  getAnnouncementSnapshot = (): string => this.announcement

  getToolbarSnapshot = () => this.getViewSnapshot('toolbar')
  getComposerSnapshot = () => this.getViewSnapshot('composer')
  getSettingsSnapshot = () => this.getViewSnapshot('settings')
  getIntents<T>(): T {
    const binding = this.resources.get('intentBindings')?.current as { readonly stable: T } | undefined
    if (binding === undefined) throw new Error('Frontend session intents require an attached runtime')
    return binding.stable
  }
  getModelsSnapshot = () => {
    const { startupState: startup, snapshotModelOptions: models } = this.controls
    if (this.modelProjection?.startup === startup && this.modelProjection.models === models) return this.modelProjection.value
    const value = this.projectModels(startup, models)
    this.modelProjection = { startup, models, value }
    return value
  }

  private projectModels(startup: StartupState, models: readonly ModelOption[] | null): ModelsSnapshot {
    const assistantCapabilities = capabilityMap(startup)
    return {
      assistantCapabilities,
      instantAssistantOptions: applySnapshotModelOptions(instantOptions(assistantCapabilities).filter((option) =>
        models === null ? !this.android : models.some((model) => model.id === option.value)), models),
      deepAssistantOptions: applySnapshotModelOptions(deepOptions(assistantCapabilities), models),
      reviewAssistantOptions: applySnapshotModelOptions(reviewOptions(assistantCapabilities), models),
    }
  }

  getViewSnapshot<N extends ViewName>(name: N): ViewSnapshot<N> {
    const prior = this.views.get(name)
    if (prior?.source === this.controls) return prior.snapshot as ViewSnapshot<N>
    const keys = VIEW_KEYS[name]
    const values = keys.map((key) => this.controls[key])
    const unchanged = prior !== undefined && values.every((value, index) => Object.is(value, prior.values[index]))
    const snapshot = unchanged ? prior.snapshot : Object.fromEntries(keys.map((key, index) => [key, values[index]]))
    this.views.set(name, { source: this.controls, values, snapshot })
    return snapshot as ViewSnapshot<N>
  }

  /** Runtime-only field access; views subscribe to named immutable snapshots. */
  fieldState<K extends keyof SessionState>(key: K): readonly [SessionState[K], Dispatch<SetStateAction<SessionState[K]>>] {
    let setter = this.setters.get(key) as Dispatch<SetStateAction<SessionState[K]>> | undefined
    if (setter === undefined) {
      setter = (value) => this.transition(key, value)
      this.setters.set(key, setter)
    }
    return [this.read(key), setter]
  }

  /** A live accessor, not a second state/ref mirror. Deferred callbacks read this same authority. */
  fieldRef<K extends keyof SessionState>(key: K): Cell<SessionState[K]> {
    let cell = this.fields.get(key)
    if (cell === undefined) {
      const read = () => this.read(key)
      const write = (value: SessionState[K]) => this.transition(key, value)
      cell = {
        get current() { return read() },
        set current(value: unknown) { write(value as SessionState[K]) },
      }
      this.fields.set(key, cell)
    }
    return cell as Cell<SessionState[K]>
  }

  /** Resource identities, pending writes and distinct revisions share the session's lifetime. */
  operationRef<T>(key: string, initial: () => T): Cell<T> {
    let cell = this.resources.get(key)
    if (cell === undefined) {
      cell = { current: initial() }
      this.resources.set(key, cell)
    }
    return cell as Cell<T>
  }

  private read<K extends keyof SessionState>(key: K): SessionState[K] {
    const domain = key in this.state.runtimeProjection ? this.state.runtimeProjection : this.state.conversationInteraction
    return (domain as SessionState)[key]
  }

  private transition<K extends keyof SessionState>(key: K, proposal: SetStateAction<SessionState[K]>): void {
    const current = this.read(key)
    const next = typeof proposal === 'function'
      ? (proposal as (value: SessionState[K]) => SessionState[K])(current)
      : proposal
    if (Object.is(current, next)) return
    this.state = key in this.state.runtimeProjection
      ? { ...this.state, runtimeProjection: { ...this.state.runtimeProjection, [key]: next } }
      : { ...this.state, conversationInteraction: { ...this.state.conversationInteraction, [key]: next } }
    if (key !== 'messages' && key !== 'responseAnnouncement') this.controlsChanged = true
    this.publicationDirty = true
    if (this.publicationQueued) return
    this.publicationQueued = true
    void Promise.resolve().then(() => {
      this.publicationQueued = false
      if (this.transactionDepth === 0) this.publish()
    })
  }

  private publish(): void {
    if (!this.publicationDirty) return
    if (this.controlsChanged) this.controls = this.projectControls()
    this.messages = this.state.conversationInteraction.messages
    this.announcement = this.state.conversationInteraction.responseAnnouncement
    this.controlsChanged = false
    this.publicationDirty = false
    for (const listener of this.listeners) listener()
  }

  private projectControls(): ControlsSnapshot {
    const interaction: MutableProjection<ConversationInteraction> = { ...this.state.conversationInteraction }
    delete interaction.messages
    delete interaction.responseAnnouncement
    return { ...this.state.runtimeProjection, ...interaction } as ControlsSnapshot
  }
}

export function useSessionControls(session: SessionAuthority): ControlsSnapshot {
  return useSyncExternalStore(session.subscribe, session.getControlsSnapshot, session.getControlsSnapshot)
}

export function useSessionView<N extends ViewName>(session: SessionAuthority, name: N): ViewSnapshot<N> {
  const snapshot = useCallback(() => session.getViewSnapshot(name), [name, session])
  return useSyncExternalStore(session.subscribe, snapshot, snapshot)
}

/** Stable intent identities; only committed runtime closures are installed. */
export function useSessionIntents<T extends Record<string, (...args: never[]) => unknown>>(session: SessionAuthority, handlers: T, bindingKey = 'intentBindings'): T {
  const cell = session.operationRef(bindingKey, () => {
    const binding = { handlers, stable: {} as T }
    binding.stable = Object.fromEntries(Object.keys(handlers).map((key) => [key, (...args: unknown[]): unknown =>
      session.transaction(() => Reflect.apply(binding.handlers[key]!, undefined, args)),
    ])) as unknown as T
    return binding
  }).current
  const bindingRef = useRef(cell)
  useLayoutEffect(() => { bindingRef.current.handlers = handlers }, [handlers])
  return cell.stable
}

// The app-facing facade never exposes field writers, resources or pending seeds.
// This capability is confined to the session runtime and its authority tests.
const authorities = new WeakMap<object, SessionAuthority>()
export function sessionAuthority(owner: object, options?: ConstructorParameters<typeof SessionAuthority>[0]): SessionAuthority {
  let authority = authorities.get(owner)
  if (authority === undefined) {
    authority = new SessionAuthority(options)
    authorities.set(owner, authority)
  }
  return authority
}

export function cancelPromptProjectionFrame(projection: PromptProjection): void {
  if (projection.renderFrameId === null) return
  window.cancelAnimationFrame(projection.renderFrameId)
  projection.renderFrameId = null
  projection.renderFrameRevision += 1
}

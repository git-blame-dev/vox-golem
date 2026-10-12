import {
  Channel as tauriChannel,
  convertFileSrc as tauriConvertFileSrc,
  invoke as tauriInvoke,
  isTauri,
} from '@tauri-apps/api/core'
import { listen as tauriListen } from '@tauri-apps/api/event'
import type { AppSnapshot, ValidatedPreferences } from './appSnapshot'
import { getBrowserPreviewInternals } from './browserRuntime'
import { utf8ByteLength } from './wire'
import { NEW_VOICE_SHORTCUT_COMMANDS } from './newVoiceShortcut'
import { serializeAssistantSettings } from './assistantSettings'
const ANDROID_CAPABILITY_IDS = ['custom_provider', 'opencode', 'local_fast', 'local_quality', 'qwen_prediction', 'wake_word', 'vad', 'parakeet', 'tts', 'deep', 'review'] as const
const COMMAND_ERROR_CODES = ['invalid_input', 'not_found', 'busy', 'unavailable', 'internal'] as const
type CommandErrorCode = typeof COMMAND_ERROR_CODES[number]

export class TauriCommandError extends Error {
  readonly code: CommandErrorCode

  constructor(code: CommandErrorCode, message: string) {
    const validCode = COMMAND_ERROR_CODES.includes(code) ? code : 'internal'
    const validMessage = message.length > 0 && utf8ByteLength(message) <= 512 ? message : 'Native command failed'
    super(validMessage)
    this.name = 'TauriCommandError'
    this.code = validCode
  }
}

function androidCommandError(code: CommandErrorCode, message: string): TauriCommandError {
  return new TauriCommandError(code, message)
}

export interface TauriEvent {
  readonly payload: unknown
}

export interface TauriInternals {
  readonly invoke: (command: string, args?: unknown) => Promise<unknown>
  readonly convertFileSrc?: (filePath: string, protocol?: string) => string
  readonly listen?: (
    event: string,
    handler: (event: TauriEvent) => void,
  ) => Promise<() => void>
  readonly createChannel?: <T>(handler: (message: T) => void) => unknown
}

declare global {
  interface Window {
    __TAURI_INTERNALS__?: TauriInternals
  }
}

export function getTauriInternals(): TauriInternals | null {
  if (typeof window === 'undefined') {
    return null
  }

  const tauriInternals = window.__TAURI_INTERNALS__
  const nativeTauriInternals = tauriInternals as
    | (TauriInternals & { readonly transformCallback?: unknown })
    | undefined

  if (typeof nativeTauriInternals?.transformCallback === 'function') {
    return createTauriApi()
  }

  if (
    tauriInternals &&
    typeof tauriInternals.invoke === 'function'
  ) {
    return withoutAndroidEventListener(tauriInternals, isAndroidRuntime)
  }

  if (!isTauri()) return getBrowserPreviewInternals()

  return createTauriApi()
}

function createTauriApi(): TauriInternals {
  return withoutAndroidEventListener({
    invoke: (command, args) => tauriInvoke(command, args as Parameters<typeof tauriInvoke>[1]),
    convertFileSrc: tauriConvertFileSrc,
    listen: (event, handler) => tauriListen(event, handler),
    createChannel: <T>(handler: (message: T) => void) => new tauriChannel(handler),
  }, isAndroidRuntime)
}

export function withoutAndroidEventListener(internals: TauriInternals, android: boolean): TauriInternals {
  if (!android) return internals
  const withoutListener = { ...internals }
  delete withoutListener.listen
  return withoutListener
}

export function hasInjectedTauriInternals(): boolean {
  return typeof window !== 'undefined' && typeof window.__TAURI_INTERNALS__?.invoke === 'function'
}

export function isNativeTauriRuntime(): boolean {
  if (typeof window === 'undefined') return false
  const internals = window.__TAURI_INTERNALS__ as (TauriInternals & { transformCallback?: unknown }) | undefined
  return typeof internals?.transformCallback === 'function'
}

export function isBrowserPreviewRuntime(): boolean {
  return typeof window !== 'undefined' && !isAndroidRuntime && !hasInjectedTauriInternals() && !isTauri()
}

export const isAndroidRuntime = import.meta.env['VITE_VOXGOLEM_RUNTIME'] === 'android'

let androidSnapshot: AppSnapshot | null = null
let androidPreferenceWriteChain: Promise<void> = Promise.resolve()
let androidPreferencePublicationRevision = 0

function enqueueAndroidPreferenceWrite<T>(operation: () => Promise<T>): Promise<T> {
  const result = androidPreferenceWriteChain.then(operation)
  androidPreferenceWriteChain = result.then(() => undefined, () => undefined)
  return result
}

export function cacheAndroidSnapshot(snapshot: AppSnapshot, expectedPreferenceRevision = androidPreferencePublicationRevision): AppSnapshot {
  const published = expectedPreferenceRevision === androidPreferencePublicationRevision || androidSnapshot === null
    ? snapshot
    : { ...snapshot, preferences: androidSnapshot.preferences }
  androidSnapshot = published
  return published
}

export function getAndroidPreferencePublicationRevision(): number {
  return androidPreferencePublicationRevision
}

export function getCachedAndroidPreferences(): ValidatedPreferences | null {
  return androidSnapshot?.preferences ?? null
}

export function setCachedAndroidPreferences(preferences: ValidatedPreferences): void {
  androidPreferencePublicationRevision += 1
  if (androidSnapshot !== null) androidSnapshot = { ...androidSnapshot, preferences }
}

export function buildAndroidStartupState(snapshot: AppSnapshot): Record<string, unknown> {
  const capabilities = new Map(snapshot.capabilities.map((capability) => [capability.id, capability]))
  return {
    kind: 'ready', cue_asset_paths: { start_listening: 'resources/start-listening.wav', stop_listening: 'resources/stop-listening.wav' },
    runtime_phase: snapshot.runtimePhase, voice_input_available: false, voice_input_error: 'Desktop audio input is unavailable on Android; native dictation has separate availability',
    silence_timeout_ms: 1500, selected_response_profile: snapshot.preferences.responseProfile, supported_response_profiles: [],
    prompt_cancellation_available: true, tts_enabled: snapshot.preferences.ttsEnabled, tts_output_gain_db: 3,
    capabilities: ANDROID_CAPABILITY_IDS.map((id) => {
      const capability = capabilities.get(id)
      return capability === undefined
        ? { id, state: 'not_configured', reason: 'Unavailable on Android zero-asset runtime', actual_provider: null }
        : { id, state: capability.state, reason: capability.reason, actual_provider: capability.actualProvider }
    }),
  }
}

export async function invokeTauriCommand(command: string, args?: unknown): Promise<unknown> {
  if (isAndroidRuntime) return invokeAndroidCommand(command, args)
  if (hasInjectedTauriInternals()) {
    return window.__TAURI_INTERNALS__!.invoke(command, args)
  }
  const internals = getTauriInternals()
  if (internals !== null) return internals.invoke(command, args)
  return tauriInvoke(command, args as Parameters<typeof tauriInvoke>[1])
}

export async function invokeAndroidCommand(command: string, args?: unknown): Promise<unknown> {
  const nativeInvoke = getTauriInternals()?.invoke ?? ((name: string, payload?: unknown) => tauriInvoke(name, payload as Parameters<typeof tauriInvoke>[1]))
  const invoke = async (name: string, payload?: unknown): Promise<unknown> => {
    try { return await nativeInvoke(name, payload) } catch (error) { throw normalizeTauriError(error) }
  }
  if (command === 'get_app_snapshot') {
    return invoke(command, args)
  }
  if (command === 'get_startup_state') {
    const snapshot = androidSnapshot
    if (snapshot === null) throw androidCommandError('internal', 'Android snapshot hydration failed')
    return buildAndroidStartupState(snapshot)
  }
  if (command === 'set_preferences') {
    return enqueueAndroidPreferenceWrite(async () => {
      const result = await invoke(command, args)
      const { parseValidatedPreferences } = await import('./appSnapshot')
      let preferences: ValidatedPreferences
      try { preferences = parseValidatedPreferences(result) } catch { throw androidCommandError('internal', 'Android runtime returned invalid preferences') }
      setCachedAndroidPreferences(preferences)
      return result
    })
  }
  if (command === 'get_ui_theme' || command === 'get_ui_text_size' || command === 'get_assistant_settings') {
    const preferences = androidSnapshot?.preferences
    if (preferences === null || preferences === undefined) throw androidCommandError('internal', 'Android preferences are not hydrated')
    if (command === 'get_ui_theme') return preferences.uiTheme
    if (command === 'get_ui_text_size') return preferences.uiTextSize
    return serializeAssistantSettings(preferences)
  }
  if (command === 'clear_completion' || command === 'request_completion' || command === 'record_frontend_runtime_diagnostic') return null
  if (command === 'check_for_update' || command === 'install_update' || command === 'restart_for_update' || command === 'synthesize_local_tts') throw androidCommandError('unavailable', 'Command is unavailable on Android')
  if (command === 'set_ui_theme' || command === 'set_ui_text_size' || command === 'set_assistant_settings' || command === 'switch_response_profile' || command === 'set_tts_enabled') {
    return enqueueAndroidPreferenceWrite(async () => {
      const current = androidSnapshot?.preferences
      if (current === null || current === undefined) throw androidCommandError('internal', 'Android preferences are not hydrated')
      const proposal = androidPreferenceProposal(command, args, current)
      const result = await invoke('set_preferences', { preferences: proposal })
      const { parseValidatedPreferences } = await import('./appSnapshot')
      let persisted: ValidatedPreferences
      try { persisted = parseValidatedPreferences(result) } catch { throw androidCommandError('internal', 'Android runtime returned invalid preferences') }
      setCachedAndroidPreferences(persisted)
      if (command === 'set_ui_theme') return persisted.uiTheme
      if (command === 'set_ui_text_size') return persisted.uiTextSize
      if (command === 'set_assistant_settings') return serializeAssistantSettings(persisted)
      if (command === 'set_tts_enabled') return { enabled: persisted.ttsEnabled }
      return result
    })
  }
  if (command === 'save_assistant_draft' || command === 'select_conversation' || command === 'select_conversation_head' || command === 'list_assistant_conversations' || command === 'list_assistant_heads') return invoke(command, args)
  if (['assistant_voice_status', 'start_assistant_voice', 'finish_assistant_dictation', 'stop_assistant_voice'].includes(command)) return invoke(command, args)
  if ((NEW_VOICE_SHORTCUT_COMMANDS as readonly string[]).includes(command)) return invoke(command, args)
  if (command === 'cancel_prompt' || command === 'reset_session' || command === 'submit_prompt') {
    const result = await invoke(command, args)
    if (command === 'reset_session') {
      const { parseAppSnapshot } = await import('./appSnapshot')
      try { parseAppSnapshot(result) } catch { throw androidCommandError('internal', 'Android runtime returned an invalid reset snapshot') }
      return result
    }
    return result
  }
  throw androidCommandError('unavailable', 'Command is unavailable on Android')
}

export function androidPreferenceProposal(command: string, args: unknown, current: ValidatedPreferences): Record<string, unknown> {
  const value = (args && typeof args === 'object' ? args : {}) as Record<string, unknown>
  const settings = value['settings'] as Record<string, unknown> | undefined
  const next = { ...current, ...(command === 'set_ui_theme' ? { uiTheme: value['theme'] } : {}), ...(command === 'set_ui_text_size' ? { uiTextSize: value['textSize'] } : {}), ...(command === 'switch_response_profile' ? { responseProfile: value['profile'] } : {}), ...(command === 'set_tts_enabled' ? { ttsEnabled: value['enabled'] } : {}), ...(command === 'set_assistant_settings' && settings ? { instant: settings['instant'], deep: settings['deep'], review: settings['review'], deepEnabled: settings['deep_enabled'], reviewEnabled: settings['review_enabled'], prefetch: settings['prefetch'], completion: settings['completion'] } : {}) }
  return { instant: next.instant, deep: next.deep, review: next.review, deep_enabled: next.deepEnabled, review_enabled: next.reviewEnabled, prefetch: next.prefetch, completion: next.completion, response_profile: next.responseProfile, tts_enabled: next.ttsEnabled, ui_theme: next.uiTheme, ui_text_size: next.uiTextSize,
    web_enabled: command === 'set_assistant_settings' && settings && Object.prototype.hasOwnProperty.call(settings, 'web_enabled') ? settings['web_enabled'] : current.webEnabled,
    speech_network_fallback: command === 'set_assistant_settings' && settings && Object.prototype.hasOwnProperty.call(settings, 'speech_network_fallback') ? settings['speech_network_fallback'] : current.speechNetworkFallback,
    speech_language: command === 'set_assistant_settings' && settings && Object.prototype.hasOwnProperty.call(settings, 'speech_language') ? settings['speech_language'] : current.speechLanguage,
    speech_tts_provider: command === 'set_assistant_settings' && settings && Object.prototype.hasOwnProperty.call(settings, 'speech_tts_provider') ? settings['speech_tts_provider'] : current.speechTtsProvider,
    speech_tts_voice: command === 'set_assistant_settings' && settings && Object.prototype.hasOwnProperty.call(settings, 'speech_tts_voice') ? settings['speech_tts_voice'] : current.speechTtsVoice }
}

export function normalizeTauriError(error: unknown): TauriCommandError {
  if (error instanceof TauriCommandError) return androidCommandError(error.code, error.message)
  if (typeof error === 'object' && error !== null) {
    const payload = error as Record<string, unknown>
    const code = payload['code']
    const message = payload['message']
    if (COMMAND_ERROR_CODES.includes(code as CommandErrorCode) && typeof message === 'string' && message.length > 0 && utf8ByteLength(message) <= 512) {
      return androidCommandError(code as CommandErrorCode, message)
    }
  }
  return androidCommandError('internal', 'Native command failed')
}

export function convertTauriFileSrc(filePath: string): string | null {
  const tauriInternals = getTauriInternals()

  if (tauriInternals?.convertFileSrc) {
    return tauriInternals.convertFileSrc(filePath)
  }

  try {
    return tauriConvertFileSrc(filePath)
  } catch {
    return null
  }
}

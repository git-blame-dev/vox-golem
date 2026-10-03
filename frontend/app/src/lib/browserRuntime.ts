import { utf8ByteLength } from './wire'

type BrowserCommandErrorCode = 'invalid_input' | 'not_found' | 'busy' | 'unavailable' | 'internal'
type BrowserPreviewOutcome = 'completed' | 'refused' | 'error'
type BrowserPreviewUpdateHandler = (message: unknown) => void

interface BrowserPreviewChannel {
  readonly send: BrowserPreviewUpdateHandler
}

interface BrowserPreviewInternals {
  readonly invoke: (command: string, args?: unknown) => Promise<unknown>
  readonly createChannel: <T>(handler: (message: T) => void) => unknown
}

interface BrowserPreviewFixture {
  readonly delayMs: number
  readonly outcome: BrowserPreviewOutcome
  readonly chunks: readonly string[] | null
  readonly errorMessage: string
}

interface BrowserPreviewPreferences {
  readonly instant: string
  readonly deep: string
  readonly review: string
  readonly deep_enabled: boolean
  readonly review_enabled: boolean
  readonly prefetch: boolean
  readonly completion: boolean
  readonly response_profile: string
  readonly tts_enabled: boolean
  readonly ui_theme: string
  readonly ui_text_size: string
}

interface BrowserPreviewExecution {
  readonly epoch: number
  readonly requestId: string
  readonly prompt: string
  readonly sessionId: string
  readonly generation: string
  readonly updates: BrowserPreviewUpdateHandler
  readonly outcome: BrowserPreviewOutcome
  readonly errorMessage: string
  readonly timers: Set<ReturnType<typeof setTimeout>>
  readonly resolve: (value: unknown) => void
  readonly reject: (reason: unknown) => void
}

interface BrowserPreviewState {
  sessionId: bigint
  generation: bigint
  messageId: bigint
  runtimePhase: 'sleeping' | 'executing'
  transcript: Record<string, unknown>[]
  active: BrowserPreviewExecution | null
  preferences: BrowserPreviewPreferences
}

export class BrowserPreviewCommandError extends Error {
  readonly code: BrowserCommandErrorCode

  constructor(code: BrowserCommandErrorCode, message: string) {
    super(message)
    this.name = 'BrowserPreviewCommandError'
    this.code = code
  }
}

const CAPABILITY_IDS = [
  'custom_provider', 'opencode', 'local_fast', 'local_quality', 'qwen_prediction',
  'wake_word', 'vad', 'parakeet', 'tts', 'deep', 'review',
] as const
const INSTANT_CHOICES = new Set(['local-fast', 'local-quality', 'custom-sol-high', 'custom-luna-low', 'opencode-sol-high', 'opencode-luna-low'])
const AGENT_CHOICES = new Set(['custom-sol-high', 'custom-luna-low', 'opencode-sol-high', 'opencode-luna-low'])
const MAX_TIMER_DELAY_MS = 2_147_483_647
const DEFAULT_FIXTURE: BrowserPreviewFixture = {
  delayMs: 0,
  outcome: 'completed',
  chunks: null,
  errorMessage: 'Deterministic browser preview failure',
}

let epoch = 0
let fixture: BrowserPreviewFixture = DEFAULT_FIXTURE
let state = createState()

const internals: BrowserPreviewInternals = {
  invoke: invokeBrowserPreviewCommand,
  createChannel: <T>(handler: (message: T) => void): BrowserPreviewChannel => ({
    send: handler as BrowserPreviewUpdateHandler,
  }),
}

export function getBrowserPreviewInternals(): BrowserPreviewInternals {
  return internals
}

export function configureBrowserPreviewFixture(next: Partial<BrowserPreviewFixture>): void {
  const delayMs = next.delayMs ?? fixture.delayMs
  if (!Number.isSafeInteger(delayMs) || delayMs < 0) {
    throw new BrowserPreviewCommandError('invalid_input', 'Preview delay must be a non-negative integer')
  }
  const outcome = next.outcome ?? fixture.outcome
  if (outcome !== 'completed' && outcome !== 'refused' && outcome !== 'error') {
    throw new BrowserPreviewCommandError('invalid_input', 'Preview outcome is unsupported')
  }
  const chunks = next.chunks === undefined ? fixture.chunks : next.chunks
  const materializedChunks = chunks === null || !Array.isArray(chunks) ? chunks : Array.from(chunks)
  if (materializedChunks !== null && (!Array.isArray(materializedChunks) || materializedChunks.length === 0 || materializedChunks.length > 128 || materializedChunks.some((chunk) => typeof chunk !== 'string' || utf8ByteLength(chunk) > 256 * 1024) || materializedChunks.reduce((total, chunk) => total + utf8ByteLength(chunk), 0) > 256 * 1024)) {
    throw new BrowserPreviewCommandError('invalid_input', 'Preview chunks must contain bounded text')
  }
  const scheduledStepCount = (materializedChunks?.length ?? 1) + 1
  if (delayMs > Math.floor(MAX_TIMER_DELAY_MS / scheduledStepCount)) {
    throw new BrowserPreviewCommandError('invalid_input', 'Preview delay exceeds the supported timer range')
  }
  const errorMessage = next.errorMessage ?? fixture.errorMessage
  if (typeof errorMessage !== 'string' || errorMessage.length === 0 || utf8ByteLength(errorMessage) > 512) {
    throw new BrowserPreviewCommandError('invalid_input', 'Preview error message must be bounded text')
  }
  fixture = { delayMs, outcome, chunks: materializedChunks === null ? null : materializedChunks, errorMessage }
}

export function teardownBrowserPreviewRuntime(): void {
  epoch += 1
  cancelActiveExecution()
  state = createState()
  fixture = DEFAULT_FIXTURE
}

export function resetBrowserPreviewRuntime(): void {
  teardownBrowserPreviewRuntime()
}

async function invokeBrowserPreviewCommand(command: string, args?: unknown): Promise<unknown> {
  switch (command) {
    case 'get_app_snapshot':
      return snapshotPayload()
    case 'get_startup_state':
      return startupPayload()
    case 'submit_prompt':
      return submitPrompt(args)
    case 'cancel_prompt':
      return cancelPrompt(args)
    case 'reset_session':
      return resetSession()
    case 'set_preferences':
      return setPreferences(readRecord(args, 'Preferences command arguments')['preferences'])
    case 'get_ui_theme':
      return state.preferences.ui_theme
    case 'get_ui_text_size':
      return state.preferences.ui_text_size
    case 'get_assistant_settings':
      return assistantSettingsPayload()
    case 'set_ui_theme':
      return setLegacyPreference('ui_theme', readRecord(args, 'Theme command arguments')['theme'])
    case 'set_ui_text_size':
      return setLegacyPreference('ui_text_size', readRecord(args, 'Text-size command arguments')['textSize'])
    case 'set_assistant_settings':
      return setLegacyAssistantSettings(readRecord(args, 'Assistant settings command arguments')['settings'])
    case 'switch_response_profile':
      return setLegacyPreference('response_profile', readRecord(args, 'Profile command arguments')['profile'])
    case 'set_tts_enabled': {
      const enabled = setLegacyPreference('tts_enabled', readRecord(args, 'TTS command arguments')['enabled'])
      return { enabled }
    }
    case 'clear_completion':
    case 'request_completion':
    case 'record_frontend_runtime_diagnostic':
      return null
    case 'check_for_update':
    case 'install_update':
    case 'restart_for_update':
    case 'synthesize_local_tts':
    case 'record_speech_activity':
    case 'mark_silence':
    case 'ingest_audio_frame':
      throw new BrowserPreviewCommandError('unavailable', 'Command is unavailable in browser preview')
    default:
      throw new BrowserPreviewCommandError('unavailable', 'Command is unavailable in browser preview')
  }
}

function submitPrompt(args: unknown): Promise<unknown> {
  if (state.active !== null) throw new BrowserPreviewCommandError('busy', 'A preview response is already active')
  const payload = readRecord(args, 'Prompt command arguments')
  const requestId = boundedString(payload['requestId'], 128, 'Request ID')
  const prompt = boundedString(payload['prompt'], 32 * 1024, 'Prompt')
  if (requestId.length === 0 || prompt.trim().length === 0) {
    throw new BrowserPreviewCommandError('invalid_input', 'Request ID and prompt must not be empty')
  }
  const channel = payload['updates']
  if (!isRecord(channel) || typeof channel['send'] !== 'function') {
    throw new BrowserPreviewCommandError('invalid_input', 'Prompt updates require a preview Channel')
  }
  const chunks = fixture.chunks ?? [`Browser preview only: no backend response was generated. Prompt: ${prompt}`]
  if (utf8ByteLength(prompt) + chunks.reduce((total, chunk) => total + utf8ByteLength(chunk), 0) > 256 * 1024) {
    throw new BrowserPreviewCommandError('invalid_input', 'Preview turn exceeds canonical history bounds')
  }

  state.generation += 1n
  state.runtimePhase = 'executing'
  const sessionId = state.sessionId.toString()
  const generation = state.generation.toString()
  let execution: BrowserPreviewExecution
  const result = new Promise<unknown>((resolve, reject) => {
    execution = {
      epoch,
      requestId,
      prompt,
      sessionId,
      generation,
      updates: channel['send'] as BrowserPreviewUpdateHandler,
      outcome: fixture.outcome,
      errorMessage: fixture.errorMessage,
      timers: new Set(),
      resolve,
      reject,
    }
    state.active = execution
  })

  chunks.forEach((chunk, index) => schedule(execution!, fixture.delayMs * (index + 1), () => {
    publishUpdate(execution!, { kind: 'delta', text: chunk })
  }))
  schedule(execution!, fixture.delayMs * (chunks.length + 1), () => settleExecution(execution!, chunks.join('')))
  return result
}

function schedule(execution: BrowserPreviewExecution, delayMs: number, callback: () => void): void {
  const run = (): void => {
    if (!isCurrent(execution)) return
    callback()
  }
  if (delayMs === 0) {
    run()
    return
  }
  const timer = setTimeout(() => {
    execution.timers.delete(timer)
    run()
  }, delayMs)
  execution.timers.add(timer)
}

function settleExecution(execution: BrowserPreviewExecution, response: string): void {
  if (!isCurrent(execution)) return
  state.active = null
  state.runtimePhase = 'sleeping'
  clearExecutionTimers(execution)
  if (execution.outcome === 'error') {
    execution.reject(new BrowserPreviewCommandError('unavailable', execution.errorMessage))
    return
  }

  state.messageId += 1n
  const userMessageId = state.messageId.toString()
  state.messageId += 1n
  const answerMessageId = state.messageId.toString()
  const contentKind = execution.outcome === 'refused' ? 'refusal' : 'text'
  const turn = {
    request_id: execution.requestId,
    execution_id: { session_id: execution.sessionId, generation: execution.generation },
    user: { id: userMessageId, role: 'user', content: { kind: 'text', text: execution.prompt } },
    answer: { id: answerMessageId, role: 'assistant', content: { kind: contentKind, text: response } },
    final_stage: 'instant',
  }
  state.transcript.push(turn)
  trimTranscript()
  execution.resolve({
    kind: execution.outcome,
    turn: clone(turn),
  })
}

function cancelPrompt(args: unknown): boolean {
  const requestId = boundedString(readRecord(args, 'Cancel command arguments')['requestId'], 128, 'Request ID')
  if (requestId.length === 0) throw new BrowserPreviewCommandError('invalid_input', 'Request ID must not be empty')
  if (state.active?.requestId !== requestId) return false
  cancelActiveExecution()
  return true
}

function cancelActiveExecution(): void {
  const active = state.active
  if (active === null) return
  state.active = null
  state.runtimePhase = 'sleeping'
  clearExecutionTimers(active)
  active.resolve({ kind: 'cancelled' })
}

function publishUpdate(execution: BrowserPreviewExecution, update: unknown): void {
  try {
    execution.updates(update)
  } catch {
    if (state.active === execution) cancelActiveExecution()
  }
}

function resetSession(): Record<string, unknown> {
  cancelActiveExecution()
  state.sessionId += 1n
  state.transcript = []
  return runtimeControlPayload()
}

function setPreferences(value: unknown): BrowserPreviewPreferences {
  const preferences = parsePreferences(value)
  validateAvailablePreferences(preferences)
  state.preferences = preferences
  return clone(preferences)
}

function setLegacyPreference(field: keyof BrowserPreviewPreferences, value: unknown): unknown {
  const proposal = { ...state.preferences, [field]: value }
  return setPreferences(proposal)[field]
}

function setLegacyAssistantSettings(value: unknown): Record<string, unknown> {
  const settings = readRecord(value, 'Assistant settings')
  setPreferences({
    ...state.preferences,
    instant: settings['instant'],
    deep: settings['deep'],
    review: settings['review'],
    deep_enabled: settings['deep_enabled'],
    review_enabled: settings['review_enabled'],
    prefetch: settings['prefetch'],
    completion: settings['completion'],
  })
  return assistantSettingsPayload()
}

function parsePreferences(value: unknown): BrowserPreviewPreferences {
  const payload = readRecord(value, 'Preferences')
  const exactFields = [
    'instant', 'deep', 'review', 'deep_enabled', 'review_enabled', 'prefetch', 'completion',
    'response_profile', 'tts_enabled', 'ui_theme', 'ui_text_size',
  ]
  if (Object.keys(payload).length !== exactFields.length || exactFields.some((field) => !Object.prototype.hasOwnProperty.call(payload, field))) {
    throw new BrowserPreviewCommandError('invalid_input', 'Preferences must include every supported field')
  }
  if (!INSTANT_CHOICES.has(payload['instant'] as string) || !AGENT_CHOICES.has(payload['deep'] as string) || !AGENT_CHOICES.has(payload['review'] as string)) {
    throw new BrowserPreviewCommandError('invalid_input', 'Preferences include an unsupported model')
  }
  for (const field of ['deep_enabled', 'review_enabled', 'prefetch', 'completion', 'tts_enabled']) {
    if (typeof payload[field] !== 'boolean') throw new BrowserPreviewCommandError('invalid_input', `Preference ${field} must be boolean`)
  }
  if (payload['response_profile'] !== 'fast' && payload['response_profile'] !== 'quality') throw new BrowserPreviewCommandError('invalid_input', 'Response profile is unsupported')
  if (payload['ui_theme'] !== 'light' && payload['ui_theme'] !== 'dark') throw new BrowserPreviewCommandError('invalid_input', 'UI theme is unsupported')
  if (!['small', 'medium', 'large', 'extra_large'].includes(payload['ui_text_size'] as string)) throw new BrowserPreviewCommandError('invalid_input', 'UI text size is unsupported')
  return {
    instant: payload['instant'] as string,
    deep: payload['deep'] as string,
    review: payload['review'] as string,
    deep_enabled: payload['deep_enabled'] as boolean,
    review_enabled: payload['review_enabled'] as boolean,
    prefetch: payload['prefetch'] as boolean,
    completion: payload['completion'] as boolean,
    response_profile: payload['response_profile'] as string,
    tts_enabled: payload['tts_enabled'] as boolean,
    ui_theme: payload['ui_theme'] as string,
    ui_text_size: payload['ui_text_size'] as string,
  }
}

function validateAvailablePreferences(preferences: BrowserPreviewPreferences): void {
  if (preferences.instant !== 'local-fast' || preferences.response_profile !== 'fast' || preferences.deep_enabled || preferences.review_enabled || preferences.completion || preferences.tts_enabled) {
    throw new BrowserPreviewCommandError('unavailable', 'Selected preferences require capabilities unavailable in browser preview')
  }
}

function snapshotPayload(): Record<string, unknown> {
  const active = state.active
  return clone({
    session_id: state.sessionId.toString(),
    runtime_phase: state.runtimePhase,
    transcript: state.transcript,
    active_request: active === null ? null : {
      execution_id: { session_id: active.sessionId, generation: active.generation },
      request_id: active.requestId,
      prompt: active.prompt,
    },
    capabilities: CAPABILITY_IDS.map((id) => id === 'local_fast'
      ? { id, state: 'available', reason: 'Deterministic browser preview', actual_provider: 'browser_preview' }
      : { id, state: 'unavailable', reason: 'Unavailable in browser preview', actual_provider: null }),
    model_options: [
      { id: 'local-fast', label: 'Local: Fast (browser preview)', available: true, unavailable_reason: null },
      { id: 'local-quality', label: 'Local: Quality', available: false, unavailable_reason: 'Unavailable in browser preview' },
      { id: 'custom-sol-high', label: 'Custom: GPT-5.6 Sol High', available: false, unavailable_reason: 'Unavailable in browser preview' },
      { id: 'custom-luna-low', label: 'Custom: GPT-5.6 Luna Low', available: false, unavailable_reason: 'Unavailable in browser preview' },
      { id: 'opencode-sol-high', label: 'OpenCode: GPT-5.6 Sol High', available: false, unavailable_reason: 'Unavailable in browser preview' },
      { id: 'opencode-luna-low', label: 'OpenCode: GPT-5.6 Luna Low', available: false, unavailable_reason: 'Unavailable in browser preview' },
    ],
    preferences: state.preferences,
  })
}

function startupPayload(): Record<string, unknown> {
  const snapshot = snapshotPayload()
  return {
    kind: 'ready',
    cue_asset_paths: { start_listening: 'resources/start-listening.wav', stop_listening: 'resources/stop-listening.wav' },
    runtime_phase: snapshot['runtime_phase'],
    voice_input_available: false,
    voice_input_error: 'Voice input is unavailable in browser preview',
    silence_timeout_ms: 1500,
    selected_response_profile: state.preferences.response_profile,
    supported_response_profiles: ['fast'],
    prompt_cancellation_available: true,
    tts_enabled: false,
    tts_output_gain_db: 3,
    capabilities: snapshot['capabilities'],
  }
}

function assistantSettingsPayload(): Record<string, unknown> {
  const preferences = state.preferences
  return {
    instant: preferences.instant,
    deep: preferences.deep,
    review: preferences.review,
    deep_enabled: preferences.deep_enabled,
    review_enabled: preferences.review_enabled,
    prefetch: preferences.prefetch,
    completion: preferences.completion,
  }
}

function runtimeControlPayload(): Record<string, unknown> {
  return {
    runtime_phase: state.runtimePhase,
    transcription_ready_samples: null,
    transcript_text: null,
    last_activity_ms: null,
    capturing_utterance: false,
    preroll_samples: 0,
    utterance_samples: 0,
    telemetry: null,
  }
}

function trimTranscript(): void {
  const textBytes = (): number => state.transcript.reduce((total, turn) => {
    const user = ((turn['user'] as Record<string, unknown>)['content'] as Record<string, unknown>)['text'] as string
    const answer = ((turn['answer'] as Record<string, unknown>)['content'] as Record<string, unknown>)['text'] as string
    return total + utf8ByteLength(user) + utf8ByteLength(answer)
  }, 0)
  while (state.transcript.length > 20 || textBytes() > 256 * 1024) state.transcript.shift()
}

function clearExecutionTimers(execution: BrowserPreviewExecution): void {
  for (const timer of execution.timers) clearTimeout(timer)
  execution.timers.clear()
}

function isCurrent(execution: BrowserPreviewExecution): boolean {
  return execution.epoch === epoch && state.active === execution
}

function createState(): BrowserPreviewState {
  return {
    sessionId: 1n,
    generation: 0n,
    messageId: 0n,
    runtimePhase: 'sleeping',
    transcript: [],
    active: null,
    preferences: {
      instant: 'local-fast',
      deep: 'opencode-sol-high',
      review: 'opencode-sol-high',
      deep_enabled: false,
      review_enabled: false,
      prefetch: false,
      completion: false,
      response_profile: 'fast',
      tts_enabled: false,
      ui_theme: 'dark',
      ui_text_size: 'medium',
    },
  }
}

function readRecord(value: unknown, label: string): Record<string, unknown> {
  if (!isRecord(value)) throw new BrowserPreviewCommandError('invalid_input', `${label} must be an object`)
  return value
}

function boundedString(value: unknown, maxBytes: number, label: string): string {
  if (typeof value !== 'string' || utf8ByteLength(value) > maxBytes) throw new BrowserPreviewCommandError('invalid_input', `${label} must be bounded text`)
  return value
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null
}

function clone<T>(value: T): T {
  return structuredClone(value)
}

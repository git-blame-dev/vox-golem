import type { DeepChoice, InstantChoice, ReviewChoice } from './assistantSettings'
import { invokeTauriCommand } from './tauri'
import { isRecord, parseRuntimePhase, utf8ByteLength } from './wire'
import type { BackendRuntimePhase, ResponseProfile, UiTextSize, UiTheme } from '../types/chat'

const U64_MAX = 18_446_744_073_709_551_615n
const CAPABILITY_ID_MAX_BYTES = 64
const MODEL_ID_MAX_BYTES = 128
const LABEL_MAX_BYTES = 128
const DISPLAY_TEXT_MAX_BYTES = 512
const stages = ['instant', 'deep', 'review'] as const
const capabilityStates = ['available', 'warming', 'not_configured', 'unavailable', 'failed'] as const
const instantChoices: readonly InstantChoice[] = ['local-fast', 'local-quality', 'custom-sol-high', 'custom-luna-low', 'opencode-sol-high', 'opencode-luna-low']
const agentChoices: readonly DeepChoice[] = ['custom-sol-high', 'custom-luna-low', 'opencode-sol-high', 'opencode-luna-low']

export type WireId = string
export type Stage = typeof stages[number]
export type CapabilityState = typeof capabilityStates[number]
export type MessageContent = { readonly kind: 'text' | 'refusal'; readonly text: string }

export interface ExecutionId {
  readonly sessionId: WireId
  readonly generation: WireId
}

export interface Message {
  readonly id: WireId
  readonly role: 'user' | 'assistant'
  readonly content: MessageContent
}

export interface Turn {
  readonly requestId: string
  readonly executionId: ExecutionId
  readonly user: Message & { readonly role: 'user' }
  readonly answer: Message & { readonly role: 'assistant' }
  readonly finalStage: Stage
}

export interface ActiveRequestSummary {
  readonly executionId: ExecutionId
  readonly requestId: string
  readonly prompt: string
}

export interface Capability {
  readonly id: string
  readonly state: CapabilityState
  readonly reason: string
  readonly actualProvider: string | null
}

export interface ModelOption {
  readonly id: string
  readonly label: string
  readonly available: boolean
  readonly unavailableReason: string | null
}

export interface ValidatedPreferences {
  readonly instant: InstantChoice
  readonly deep: DeepChoice
  readonly review: ReviewChoice
  readonly deepEnabled: boolean
  readonly reviewEnabled: boolean
  readonly prefetch: boolean
  readonly completion: boolean
  readonly responseProfile: ResponseProfile
  readonly ttsEnabled: boolean
  readonly uiTheme: UiTheme
  readonly uiTextSize: UiTextSize
}

export interface AppSnapshot {
  readonly sessionId: WireId
  readonly runtimePhase: BackendRuntimePhase
  readonly transcript: readonly Turn[]
  readonly activeRequest: ActiveRequestSummary | null
  readonly capabilities: readonly Capability[]
  readonly modelOptions: readonly ModelOption[]
  readonly preferences: ValidatedPreferences
}

export function parseAppSnapshot(payload: unknown): AppSnapshot {
  if (!isRecord(payload)) throw new Error('App snapshot must be an object')
  if (!Array.isArray(payload['transcript'])) throw new Error('App snapshot transcript must be an array')
  if (!Array.isArray(payload['capabilities'])) throw new Error('App snapshot capabilities must be an array')
  if (!Array.isArray(payload['model_options'])) throw new Error('App snapshot model_options must be an array')

  const capabilities = Array.from(payload['capabilities'], parseCapability)
  rejectDuplicateIds(capabilities, 'capability')
  const modelOptions = Array.from(payload['model_options'], parseModelOption)
  rejectDuplicateIds(modelOptions, 'model option')
  return {
    sessionId: parseWireId(payload['session_id'], 'session_id'),
    runtimePhase: parseRuntimePhase(payload['runtime_phase'], 'runtime_phase'),
    transcript: Array.from(payload['transcript'], parseTurn),
    activeRequest: payload['active_request'] === null ? null : parseActiveRequest(payload['active_request']),
    capabilities,
    modelOptions,
    preferences: parsePreferences(payload['preferences']),
  }
}

export async function loadAppSnapshot(): Promise<AppSnapshot> {
  return parseAppSnapshot(await invokeTauriCommand('get_app_snapshot'))
}

function parseWireId(value: unknown, field: string): WireId {
  if (typeof value !== 'string' || !/^(0|[1-9]\d*)$/.test(value)) throw new Error(`${field} must be a canonical unsigned decimal string`)
  if (BigInt(value) > U64_MAX) throw new Error(`${field} exceeds the u64 range`)
  return value
}

function parseExecutionId(value: unknown): ExecutionId {
  if (!isRecord(value)) throw new Error('execution_id must be an object')
  return {
    sessionId: parseWireId(value['session_id'], 'execution_id.session_id'),
    generation: parseWireId(value['generation'], 'execution_id.generation'),
  }
}

function parseContent(value: unknown): MessageContent {
  if (!isRecord(value) || (value['kind'] !== 'text' && value['kind'] !== 'refusal') || typeof value['text'] !== 'string') {
    throw new Error('message content must include a supported kind and text')
  }
  return { kind: value['kind'], text: value['text'] }
}

function parseMessage(value: unknown): Message {
  if (!isRecord(value) || (value['role'] !== 'user' && value['role'] !== 'assistant')) throw new Error('message must include a supported role')
  return { id: parseWireId(value['id'], 'message.id'), role: value['role'], content: parseContent(value['content']) }
}

function parseTurn(value: unknown): Turn {
  if (!isRecord(value) || typeof value['request_id'] !== 'string' || value['request_id'].length === 0) throw new Error('turn must include request_id')
  const user = parseMessage(value['user'])
  const answer = parseMessage(value['answer'])
  if (user.role !== 'user') throw new Error('turn must include the user role')
  if (answer.role !== 'assistant') throw new Error('turn must include the assistant role')
  return {
    requestId: value['request_id'],
    executionId: parseExecutionId(value['execution_id']),
    user: user as Turn['user'],
    answer: answer as Turn['answer'],
    finalStage: parseStage(value['final_stage']),
  }
}

function parseActiveRequest(value: unknown): ActiveRequestSummary {
  if (!isRecord(value) || typeof value['request_id'] !== 'string' || value['request_id'].length === 0 || typeof value['prompt'] !== 'string') {
    throw new Error('active_request must include request_id and prompt')
  }
  return { executionId: parseExecutionId(value['execution_id']), requestId: value['request_id'], prompt: value['prompt'] }
}

function parseCapability(value: unknown): Capability {
  if (!isRecord(value)) throw new Error('capability must be an object')
  const state = value['state']
  if (!capabilityStates.includes(state as CapabilityState)) throw new Error('capability state is invalid')
  return {
    id: parseBoundedString(value['id'], CAPABILITY_ID_MAX_BYTES, 'capability id'),
    state: state as CapabilityState,
    reason: parseBoundedString(value['reason'], DISPLAY_TEXT_MAX_BYTES, 'capability reason'),
    actualProvider: parseNullableBoundedString(value, 'actual_provider', CAPABILITY_ID_MAX_BYTES),
  }
}

function parseModelOption(value: unknown): ModelOption {
  if (!isRecord(value) || typeof value['available'] !== 'boolean') throw new Error('model option must include available')
  return {
    id: parseBoundedString(value['id'], MODEL_ID_MAX_BYTES, 'model option id'),
    label: parseBoundedString(value['label'], LABEL_MAX_BYTES, 'model option label'),
    available: value['available'],
    unavailableReason: parseNullableBoundedString(value, 'unavailable_reason', DISPLAY_TEXT_MAX_BYTES),
  }
}

function parsePreferences(value: unknown): ValidatedPreferences {
  if (!isRecord(value)) throw new Error('preferences must be an object')
  const instant = value['instant']; const deep = value['deep']; const review = value['review']
  if (!instantChoices.includes(instant as InstantChoice)) throw new Error('preferences instant is invalid')
  if (!agentChoices.includes(deep as DeepChoice) || !agentChoices.includes(review as ReviewChoice)) throw new Error('preferences agent model is invalid')
  const responseProfile = value['response_profile']; const uiTheme = value['ui_theme']; const uiTextSize = value['ui_text_size']
  if (responseProfile !== 'fast' && responseProfile !== 'quality') throw new Error('preferences response_profile is invalid')
  if (uiTheme !== 'light' && uiTheme !== 'dark') throw new Error('preferences ui_theme is invalid')
  if (uiTextSize !== 'small' && uiTextSize !== 'medium' && uiTextSize !== 'large' && uiTextSize !== 'extra_large') throw new Error('preferences ui_text_size is invalid')
  for (const field of ['deep_enabled', 'review_enabled', 'prefetch', 'completion', 'tts_enabled'] as const) {
    if (typeof value[field] !== 'boolean') throw new Error(`preferences ${field} must be boolean`)
  }
  return {
    instant: instant as InstantChoice, deep: deep as DeepChoice, review: review as ReviewChoice,
    deepEnabled: value['deep_enabled'] as boolean, reviewEnabled: value['review_enabled'] as boolean,
    prefetch: value['prefetch'] as boolean, completion: value['completion'] as boolean,
    responseProfile, ttsEnabled: value['tts_enabled'] as boolean, uiTheme, uiTextSize,
  }
}

function parseStage(value: unknown): Stage {
  if (stages.includes(value as Stage)) return value as Stage
  throw new Error('final_stage is invalid')
}

function parseBoundedString(value: unknown, maxBytes: number, field: string): string {
  if (typeof value !== 'string' || utf8ByteLength(value) > maxBytes) throw new Error(`${field} must be a bounded string`)
  return value
}

function parseNullableBoundedString(value: Record<string, unknown>, field: string, maxBytes: number): string | null {
  if (!Object.prototype.hasOwnProperty.call(value, field)) throw new Error(`${field} must be present`)
  if (value[field] === null) return null
  return parseBoundedString(value[field], maxBytes, field)
}

function rejectDuplicateIds(values: readonly { readonly id: string }[], label: string): void {
  if (new Set(values.map(({ id }) => id)).size !== values.length) throw new Error(`App snapshot contains duplicate ${label} ids`)
}

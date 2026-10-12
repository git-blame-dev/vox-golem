import type { DeepChoice, InstantChoice, ReviewChoice, SpeechSelection } from './assistantSettings'
import { parsePolicyBoolean, parseSpeechSelectionString } from './assistantSettings'
import { invokeTauriCommand, isAndroidRuntime } from './tauri'
import { isRecord, parseRuntimePhase, utf8ByteLength } from './wire'
import type { BackendRuntimePhase, ResponseProfile, UiTextSize, UiTheme } from '../types/chat'
import { INSTANT_CHOICES as instantChoices, AGENT_CHOICES as agentChoices } from './modelCatalog'

const U64_MAX = 18_446_744_073_709_551_615n
const CAPABILITY_ID_MAX_BYTES = 64
const MODEL_ID_MAX_BYTES = 128
const LABEL_MAX_BYTES = 128
const DISPLAY_TEXT_MAX_BYTES = 512
const REQUEST_ID_MAX_BYTES = 128
const PROMPT_MAX_BYTES = 32 * 1024
const COMMITTED_TURN_MAX_COUNT = 20
const COMMITTED_TEXT_MAX_BYTES = 256 * 1024
const stages = ['instant', 'deep', 'review'] as const
const capabilityStates = ['available', 'warming', 'not_configured', 'unavailable', 'failed'] as const

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

export interface ValidatedPreferences extends SpeechSelection {
  readonly instant: InstantChoice
  readonly deep: DeepChoice
  readonly review: ReviewChoice
  readonly deepEnabled: boolean
  readonly reviewEnabled: boolean
  readonly prefetch: boolean
  readonly completion: boolean
  readonly webEnabled: boolean
  readonly speechNetworkFallback: boolean
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
  readonly assistant?: AndroidAssistantProjection
}

export interface AndroidAssistantProjection {
  readonly revision: string
  readonly conversations: readonly { readonly id: string; readonly title: string }[]
  readonly conversationCount: number
  readonly conversationOffset: number
  readonly selectedHead: string | null
  readonly branchChoices: readonly string[]
  readonly branchCount: number
  readonly branchOffset: number
  readonly draft: string
  readonly interrupted: boolean
  readonly error: string | null
}

export function parseAppSnapshot(payload: unknown, android = isAndroidRuntime): AppSnapshot {
  if (!isRecord(payload)) throw new Error('App snapshot must be an object')
  if (!Array.isArray(payload['transcript'])) throw new Error('App snapshot transcript must be an array')
  if (!Array.isArray(payload['capabilities'])) throw new Error('App snapshot capabilities must be an array')
  if (!Array.isArray(payload['model_options'])) throw new Error('App snapshot model_options must be an array')

  const capabilities = Array.from(payload['capabilities'], parseCapability)
  rejectDuplicateIds(capabilities, 'capability')
  const modelOptions = Array.from(payload['model_options'], parseModelOption)
  rejectDuplicateIds(modelOptions, 'model option')
  const transcript = Array.from(payload['transcript'], (turn) => parseCanonicalTurn(turn, android))
  if (transcript.length > COMMITTED_TURN_MAX_COUNT || transcriptTextBytes(transcript) > COMMITTED_TEXT_MAX_BYTES) {
    throw new Error('App snapshot transcript exceeds canonical history bounds')
  }
  return {
    sessionId: parseWireId(payload['session_id'], 'session_id', android),
    runtimePhase: parseRuntimePhase(payload['runtime_phase'], 'runtime_phase'),
    transcript,
    activeRequest: payload['active_request'] === null ? null : parseActiveRequest(payload['active_request'], android),
    capabilities,
    modelOptions,
    preferences: parsePreferences(payload['preferences']),
    ...(android && payload['assistant'] !== undefined ? { assistant: parseAndroidAssistant(payload['assistant']) } : {}),
  }
}

export async function loadAppSnapshot(): Promise<AppSnapshot> {
  return parseAppSnapshot(await invokeTauriCommand('get_app_snapshot'))
}

export function appendCanonicalTurn(transcript: readonly Turn[], turn: Turn | undefined): readonly Turn[] {
  if (turn === undefined || transcript.some(({ executionId }) =>
    executionId.sessionId === turn.executionId.sessionId && executionId.generation === turn.executionId.generation)) {
    return transcript
  }
  const next = [...transcript, turn]
  while (next.length > COMMITTED_TURN_MAX_COUNT || transcriptTextBytes(next) > COMMITTED_TEXT_MAX_BYTES) next.shift()
  return next
}

export function canonicalExecutionKey(turn: Turn): string {
  return `${turn.executionId.sessionId}:${turn.executionId.generation}`
}

export function isExpectedCanonicalTurn(
  turn: Turn,
  requestId: string,
  expectedSessionId: string | null,
  priorExecutions: ReadonlySet<string>,
  allowNewConversation = false,
): boolean {
  return expectedSessionId !== null && turn.requestId === requestId && (turn.executionId.sessionId === expectedSessionId || (allowNewConversation && expectedSessionId === '0')) && !priorExecutions.has(canonicalExecutionKey(turn))
}

function parseWireId(value: unknown, field: string, android = false): WireId {
  if (android && typeof value === 'string') {
    const match = /^([0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}):([1-9]\d*)$/.exec(value)
    if (match !== null && BigInt(match[2]!) <= U64_MAX) return value
  }
  if (typeof value !== 'string' || !/^(0|[1-9]\d*)$/.test(value)) throw new Error(`${field} must be a canonical unsigned decimal string`)
  if (BigInt(value) > U64_MAX) throw new Error(`${field} exceeds the u64 range`)
  return value
}

function parseExecutionId(value: unknown, android = false): ExecutionId {
  if (!isRecord(value)) throw new Error('execution_id must be an object')
  return {
    sessionId: parseWireId(value['session_id'], 'execution_id.session_id', android),
    generation: parseWireId(value['generation'], 'execution_id.generation', android),
  }
}

function parseContent(value: unknown): MessageContent {
  if (!isRecord(value) || (value['kind'] !== 'text' && value['kind'] !== 'refusal') || typeof value['text'] !== 'string') {
    throw new Error('message content must include a supported kind and text')
  }
  return { kind: value['kind'], text: value['text'] }
}

function parseMessage(value: unknown, android = false): Message {
  if (!isRecord(value) || (value['role'] !== 'user' && value['role'] !== 'assistant')) throw new Error('message must include a supported role')
  return { id: parseWireId(value['id'], 'message.id', android), role: value['role'], content: parseContent(value['content']) }
}

export function parseCanonicalTurn(value: unknown, android = false): Turn {
  if (!isRecord(value) || typeof value['request_id'] !== 'string' || value['request_id'].length === 0 || utf8ByteLength(value['request_id']) > REQUEST_ID_MAX_BYTES) throw new Error('turn must include a bounded request_id')
  const user = parseMessage(value['user'], android)
  const answer = parseMessage(value['answer'], android)
  if (user.role !== 'user') throw new Error('turn must include the user role')
  if (answer.role !== 'assistant') throw new Error('turn must include the assistant role')
  if (user.content.kind !== 'text' || user.content.text.trim().length === 0 || utf8ByteLength(user.content.text) > PROMPT_MAX_BYTES) throw new Error('turn user content must be a bounded prompt')
  if (utf8ByteLength(user.content.text) + utf8ByteLength(answer.content.text) > COMMITTED_TEXT_MAX_BYTES) throw new Error('turn content exceeds canonical history bounds')
  return {
    requestId: value['request_id'],
    executionId: parseExecutionId(value['execution_id'], android),
    user: user as Turn['user'],
    answer: answer as Turn['answer'],
    finalStage: parseStage(value['final_stage']),
  }
}

function transcriptTextBytes(transcript: readonly Turn[]): number {
  return transcript.reduce((total, turn) => total + utf8ByteLength(turn.user.content.text) + utf8ByteLength(turn.answer.content.text), 0)
}

function parseActiveRequest(value: unknown, android = false): ActiveRequestSummary {
  if (!isRecord(value) || typeof value['request_id'] !== 'string' || value['request_id'].length === 0 || utf8ByteLength(value['request_id']) > REQUEST_ID_MAX_BYTES || typeof value['prompt'] !== 'string' || value['prompt'].trim().length === 0 || utf8ByteLength(value['prompt']) > PROMPT_MAX_BYTES) {
    throw new Error('active_request must include a bounded request_id and prompt')
  }
  return { executionId: parseExecutionId(value['execution_id'], android), requestId: value['request_id'], prompt: value['prompt'] }
}

function parseAndroidAssistant(value: unknown): AndroidAssistantProjection {
  if (!isRecord(value) || !Array.isArray(value['conversations']) || value['conversations'].length > 128 || !Array.isArray(value['branch_choices']) || value['branch_choices'].length > 128 || typeof value['interrupted'] !== 'boolean') throw new Error('Android assistant projection is invalid')
  const conversations = Array.from(value['conversations'], (item: unknown) => {
    if (!isRecord(item)) throw new Error('Android conversation is invalid')
    return { id: parseWireId(item['id'], 'conversation.id', true), title: parseBoundedString(item['title'], 256, 'conversation.title') }
  })
  rejectDuplicateIds(conversations, 'conversation')
  const branches = Array.from(value['branch_choices'], (id: unknown) => parseWireId(id, 'branch choice', true))
  if (new Set(branches).size !== branches.length) throw new Error('Android branch choices contain duplicate IDs')
  const count = value['conversation_count'] ?? conversations.length
  if (!Number.isSafeInteger(count) || (count as number) < conversations.length || (count as number) > 16_384) throw new Error('Android conversation count is invalid')
  const offset = value['conversation_offset'] ?? conversations.length
  if (!Number.isSafeInteger(offset) || (offset as number) < 0 || (offset as number) > (count as number)) throw new Error('Android conversation offset is invalid')
  const branchCount = value['branch_count'] ?? branches.length
  const branchOffset = value['branch_offset'] ?? branches.length
  if (!Number.isSafeInteger(branchCount) || (branchCount as number) < branches.length || (branchCount as number) > 16_384 || !Number.isSafeInteger(branchOffset) || (branchOffset as number) < 0 || (branchOffset as number) > (branchCount as number)) throw new Error('Android continuation count is invalid')
  return {
    revision: parseWireId(value['revision'], 'assistant.revision'), conversations, conversationCount: count as number, conversationOffset: offset as number,
    selectedHead: value['selected_head'] === null ? null : parseWireId(value['selected_head'], 'selected_head', true),
    branchChoices: branches, branchCount: branchCount as number, branchOffset: branchOffset as number,
    draft: parseBoundedString(value['draft'], PROMPT_MAX_BYTES, 'draft'), interrupted: value['interrupted'],
    error: value['error'] === undefined || value['error'] === null ? null : parseBoundedString(value['error'], DISPLAY_TEXT_MAX_BYTES, 'assistant error'),
  }
}

export function parseAndroidConversationPage(value: unknown): { readonly revision: string; readonly conversations: AndroidAssistantProjection['conversations']; readonly nextOffset: number } {
  if (!isRecord(value) || !Array.isArray(value['conversations']) || value['conversations'].length > 128 || !Number.isSafeInteger(value['next_offset']) || (value['next_offset'] as number) < 0 || (value['next_offset'] as number) > 16_384) throw new Error('Android conversation page is invalid')
  const conversations = Array.from(value['conversations'], (item: unknown) => {
    if (!isRecord(item)) throw new Error('Android conversation is invalid')
    return { id: parseWireId(item['id'], 'conversation.id', true), title: parseBoundedString(item['title'], 256, 'conversation.title') }
  })
  rejectDuplicateIds(conversations, 'conversation')
  return { revision: parseWireId(value['revision'], 'revision'), conversations, nextOffset: value['next_offset'] as number }
}

export function parseAndroidHeadPage(value: unknown): { readonly revision: string; readonly heads: readonly string[]; readonly nextOffset: number } {
  if (!isRecord(value) || !Array.isArray(value['heads']) || value['heads'].length > 128 || !Number.isSafeInteger(value['next_offset']) || (value['next_offset'] as number) < 0 || (value['next_offset'] as number) > 16_384) throw new Error('Android continuation page is invalid')
  const heads = Array.from(value['heads'], (id: unknown) => parseWireId(id, 'head', true))
  if (new Set(heads).size !== heads.length) throw new Error('Android continuation page contains duplicate IDs')
  return { revision: parseWireId(value['revision'], 'revision'), heads, nextOffset: value['next_offset'] as number }
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

export function parseValidatedPreferences(value: unknown): ValidatedPreferences {
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
    webEnabled: parsePolicyBoolean(value, 'web_enabled'), speechNetworkFallback: parsePolicyBoolean(value, 'speech_network_fallback'),
    speechLanguage: parseSpeechSelectionString(value, 'speech_language'), speechTtsProvider: parseSpeechSelectionString(value, 'speech_tts_provider'), speechTtsVoice: parseSpeechSelectionString(value, 'speech_tts_voice'),
    responseProfile, ttsEnabled: value['tts_enabled'] as boolean, uiTheme, uiTextSize,
  }
}

const parsePreferences = parseValidatedPreferences

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

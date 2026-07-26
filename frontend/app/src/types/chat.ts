export type MessageRole = 'system' | 'user' | 'assistant'

export type TranscriptRole = Exclude<MessageRole, 'system'>

export interface ChatMessage {
  readonly id: string
  readonly role: MessageRole
  readonly content: string
  readonly answerStage?: AnswerStageMetadata
}

export interface AnswerStageMetadata {
  readonly stages: readonly import('../components/AnswerStage').AnswerStageStatusEntry[]
  readonly priorVersions: readonly import('../components/AnswerStage').AnswerPriorVersion[]
  readonly sources?: readonly import('../components/AnswerStage').AnswerSource[]
}

export interface TranscriptMessage {
  readonly id: string
  readonly role: TranscriptRole
  readonly content: string
  readonly answerStage?: AnswerStageMetadata
}

export interface UserNotice {
  readonly id: string
  readonly tone: 'info' | 'warning' | 'error'
  readonly title: string
  readonly message: string
}

export function isTranscriptMessage(message: ChatMessage): message is TranscriptMessage {
  return message.role !== 'system'
}

export interface CueAssetPaths {
  readonly startListening: string
  readonly stopListening: string
}

export type ResponseProfile = 'fast' | 'quality'

export type UiTextSize = 'small' | 'medium' | 'large' | 'extra_large'

export type UiTheme = 'light' | 'dark'

export interface ResponseProfileState {
  readonly selectedResponseProfile: ResponseProfile
  readonly supportedResponseProfiles: readonly ResponseProfile[]
}

export type CapabilityId =
  | 'custom_provider'
  | 'opencode'
  | 'local_fast'
  | 'local_quality'
  | 'qwen_prediction'
  | 'wake_word'
  | 'vad'
  | 'parakeet'
  | 'tts'
  | 'deep'
  | 'review'

export type CapabilityStatus = 'available' | 'warming' | 'not_configured' | 'unavailable' | 'failed'

export interface StartupCapability {
  readonly id: CapabilityId
  readonly state: CapabilityStatus
  readonly reason: string
  readonly actualProvider: string | null
}

export type BackendRuntimePhase =
  | 'initializing'
  | 'sleeping'
  | 'listening'
  | 'processing'
  | 'executing'
  | 'error'

export type PromptExecutionEvent =
  | { readonly requestId: string; readonly kind: 'text' | 'reasoning'; readonly text: string }
  | { readonly requestId: string; readonly kind: 'correction'; readonly stage: 'instant' | 'deep' | 'review'; readonly text: string; readonly correction: string }
  | { readonly requestId: string; readonly kind: 'stage'; readonly stage: 'instant' | 'deep' | 'review'; readonly status: import('../components/AnswerStage').AnswerStageStatus; readonly detail?: string }
  | { readonly requestId: string; readonly kind: 'sources'; readonly sources: readonly import('../components/AnswerStage').AnswerSource[] }
  | { readonly requestId: string; readonly kind: 'status'; readonly message: string }
  | {
      readonly requestId: string
      readonly kind: 'tool'
      readonly tool: string
      readonly status: 'pending' | 'running' | 'completed' | 'error'
      readonly detail: string
    }
  | { readonly requestId: string; readonly kind: 'error'; readonly message: string }
  | {
      readonly requestId: string
      readonly kind: 'completed' | 'cancelled'
      readonly runtimePhase: BackendRuntimePhase
    }

export interface PromptExecutionResult {
  readonly requestId: string
  readonly runtimePhase: BackendRuntimePhase
  readonly outcome: 'completed' | 'cancelled' | 'error'
  readonly errorMessage: string | null
}

export type StartupState =
  | { readonly kind: 'loading' }
  | {
      readonly kind: 'warming_model'
      readonly cueAssetPaths: CueAssetPaths
      readonly runtimePhase: BackendRuntimePhase
      readonly voiceInputAvailable: boolean
      readonly voiceInputError: string | null
  readonly silenceTimeoutMs: number
  readonly message: string
    readonly selectedResponseProfile: ResponseProfile
    readonly supportedResponseProfiles: readonly ResponseProfile[]
    readonly promptCancellationAvailable: boolean
    readonly ttsEnabled: boolean
      readonly ttsOutputGainDb: number
      readonly capabilities: readonly StartupCapability[]
  }
| {
  readonly kind: 'ready'
  readonly cueAssetPaths: CueAssetPaths
      readonly runtimePhase: BackendRuntimePhase
      readonly voiceInputAvailable: boolean
  readonly voiceInputError: string | null
  readonly silenceTimeoutMs: number
    readonly selectedResponseProfile: ResponseProfile
    readonly supportedResponseProfiles: readonly ResponseProfile[]
    readonly promptCancellationAvailable: boolean
    readonly ttsEnabled: boolean
    readonly ttsOutputGainDb: number
    readonly capabilities: readonly StartupCapability[]
  }
| { readonly kind: 'error'; readonly message: string }

export type RuntimeStatus =
  | 'initializing'
  | 'sleeping'
  | 'listening'
  | 'processing'
  | 'executing'
  | 'error'

export interface RuntimeControlResult {
  readonly runtimePhase: BackendRuntimePhase
  readonly transcriptionReadySamples: number | null
  readonly transcriptText: string | null
  readonly lastActivityMs: number | null
  readonly capturingUtterance: boolean
  readonly prerollSamples: number
  readonly utteranceSamples: number
  readonly telemetry: RuntimeControlTelemetry | null
}

export interface RuntimeControlTelemetry {
  readonly frameId: string | null
  readonly backendIngestStartedMs: number | null
  readonly backendIngestCompletedMs: number | null
  readonly wakeDetectedMs: number | null
  readonly wakeConfidence: number | null
  readonly transcriptionStartedMs: number | null
  readonly transcriptionCompletedMs: number | null
}

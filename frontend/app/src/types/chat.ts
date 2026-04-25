export type MessageRole = 'system' | 'user' | 'assistant'

export interface ChatMessage {
  readonly id: string
  readonly role: MessageRole
  readonly content: string
}

export interface CueAssetPaths {
  readonly startListening: string
  readonly stopListening: string
}

export type ResponseProfile = 'fast' | 'quality'

export interface ResponseProfileState {
  readonly selectedResponseProfile: ResponseProfile
  readonly supportedResponseProfiles: readonly ResponseProfile[]
}

export type BackendRuntimePhase =
  | 'initializing'
  | 'sleeping'
  | 'listening'
  | 'processing'
  | 'executing'
  | 'error'

export type PromptExecutionEvent =
  | { readonly kind: 'text'; readonly text: string }
  | { readonly kind: 'reasoning'; readonly text: string }
  | { readonly kind: 'step_start' }
  | { readonly kind: 'step_finish'; readonly reason: string | null }
  | { readonly kind: 'error'; readonly name: string; readonly message: string }
  | {
      readonly kind: 'tool_use'
      readonly tool: string
      readonly status: 'completed' | 'error'
      readonly detail: string
    }

export interface PromptExecutionResult {
  readonly events: readonly PromptExecutionEvent[]
  readonly stderr: string
  readonly exitCode: number | null
  readonly runtimePhase: BackendRuntimePhase
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

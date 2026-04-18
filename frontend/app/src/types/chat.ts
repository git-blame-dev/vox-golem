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

export type BackendRuntimePhase =
  | 'initializing'
  | 'sleeping'
  | 'listening'
  | 'processing'
  | 'executing'
  | 'result_ready'
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
      readonly kind: 'ready'
      readonly cueAssetPaths: CueAssetPaths
      readonly runtimePhase: BackendRuntimePhase
      readonly voiceInputAvailable: boolean
      readonly voiceInputError: string | null
    }
  | { readonly kind: 'error'; readonly message: string }

export type RuntimeStatus =
  | 'initializing'
  | 'sleeping'
  | 'listening'
  | 'processing'
  | 'executing'
  | 'result_ready'
  | 'error'

export interface RuntimeControlResult {
  readonly runtimePhase: BackendRuntimePhase
  readonly transcriptionReadySamples: number | null
  readonly transcriptText: string | null
  readonly lastActivityMs: number | null
  readonly capturingUtterance: boolean
  readonly prerollSamples: number
  readonly utteranceSamples: number
}

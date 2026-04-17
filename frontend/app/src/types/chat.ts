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

export type PromptExecutionEvent =
  | { readonly kind: 'text'; readonly text: string }
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
}

export type StartupState =
  | { readonly kind: 'loading' }
  | { readonly kind: 'ready'; readonly cueAssetPaths: CueAssetPaths }
  | { readonly kind: 'error'; readonly message: string }

export type RuntimeStatus =
  | 'initializing'
  | 'sleeping'
  | 'listening'
  | 'processing'
  | 'executing'
  | 'result_ready'
  | 'error'

import { invokeTauriCommand } from './tauri'

export const runtimeDiagnosticKinds = [
  'frontend_notice',
  'execution_failed',
  'audio_failed',
  'microphone_status_changed',
  'profile_switch_failed',
  'tts_toggle_failed',
  'benchmark_lifetime_retire',
  'benchmark_controls_retire',
  'benchmark_capability_retire',
  'benchmark_audio_error',
  'benchmark_cue_error',
] as const

export type RuntimeDiagnosticKind = typeof runtimeDiagnosticKinds[number]
const allowedKinds: ReadonlySet<unknown> = new Set(runtimeDiagnosticKinds)

/** Fixed wire categories only. Optional persistence must never affect UI work. */
export function recordRuntimeDiagnostic(kind: RuntimeDiagnosticKind): void {
  if (!allowedKinds.has(kind)) return
  try {
    void invokeTauriCommand('record_frontend_runtime_diagnostic', { event: { kind } }).catch(() => undefined)
  } catch { /* Bridge availability cannot change the operation being observed. */ }
}

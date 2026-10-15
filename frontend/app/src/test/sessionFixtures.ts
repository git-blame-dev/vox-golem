/** Synthetic wire fixtures consumed by the production parsers, not parser replicas. */
export function completeCapabilities(overrides: Record<string, string> = {}): Array<Record<string, unknown>> {
  return ['custom_provider', 'opencode', 'local_fast', 'local_quality', 'qwen_prediction', 'wake_word', 'vad', 'parakeet', 'tts', 'deep', 'review']
    .map((id) => ({ id, state: overrides[id] ?? 'available', reason: 'Synthetic fixture', actual_provider: null }))
}

export function readyStartupState(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return { kind: 'ready', cue_asset_paths: { start_listening: 'start.wav', stop_listening: 'stop.wav' },
    runtime_phase: 'sleeping', voice_input_available: true, voice_input_error: null, silence_timeout_ms: 1500,
    selected_response_profile: 'fast', supported_response_profiles: ['fast', 'quality'], tts_enabled: false,
    capabilities: completeCapabilities(), ...overrides }
}

export function defaultAssistantSettings(instant: 'local-fast' | 'local-quality' = 'local-fast'): Record<string, unknown> {
  return { instant, deep: 'opencode-sol-high', review: 'opencode-sol-high', deep_enabled: false, review_enabled: false, prefetch: false, completion: false }
}

export function appSnapshot(transcript: readonly Record<string, unknown>[] = [], preferenceOverrides: Record<string, unknown> = {}, snapshotOverrides: Record<string, unknown> = {}): Record<string, unknown> {
  return { session_id: '1', runtime_phase: 'sleeping', transcript, active_request: null, capabilities: completeCapabilities(),
    model_options: [
      ['local-fast', 'Local: Fast'], ['local-quality', 'Local: Quality'], ['custom-sol-none', 'Custom: GPT-6 Sol No Reasoning'],
      ['custom-luna-none', 'Custom: GPT-6 Luna No Reasoning'], ['custom-sol-high', 'Custom: GPT-5.6 Sol High'],
      ['custom-luna-low', 'Custom: GPT-5.6 Luna Low'], ['opencode-sol-high', 'OpenCode: GPT-5.6 Sol High'], ['opencode-luna-low', 'OpenCode: GPT-5.6 Luna Low'],
    ].map(([id, label]) => ({ id, label, available: true, unavailable_reason: null })),
    preferences: { ...defaultAssistantSettings(), response_profile: 'fast', tts_enabled: false, ui_theme: 'dark', ui_text_size: 'medium', ...preferenceOverrides },
    ...snapshotOverrides }
}

export function canonicalTurn(userId: string, answerId: string, prompt: string, answer: string, requestId = 'request-hydrated', contentKind: 'text' | 'refusal' = 'text', finalStage: 'instant' | 'deep' | 'review' = 'instant', sessionId = '1'): Record<string, unknown> {
  return { request_id: requestId, execution_id: { session_id: sessionId, generation: answerId },
    user: { id: userId, role: 'user', content: { kind: 'text', text: prompt } },
    answer: { id: answerId, role: 'assistant', content: { kind: contentKind, text: answer } }, final_stage: finalStage }
}

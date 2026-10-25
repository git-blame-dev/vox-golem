import { afterEach, describe, expect, it, vi } from 'vitest'
import { recordRuntimeDiagnostic, runtimeDiagnosticKinds, type RuntimeDiagnosticKind } from './runtimeDiagnostics'
import { invokeTauriCommand } from './tauri'

vi.mock('./tauri', () => ({ invokeTauriCommand: vi.fn().mockResolvedValue(null) }))
afterEach(() => { vi.mocked(invokeTauriCommand).mockReset().mockResolvedValue(null) })

describe('runtime diagnostic wire boundary', () => {
  it.each(runtimeDiagnosticKinds)('sends only the fixed category %s', (kind) => {
    recordRuntimeDiagnostic(kind)
    expect(invokeTauriCommand).toHaveBeenCalledExactlyOnceWith('record_frontend_runtime_diagnostic', { event: { kind } })
  })

  it('rejects forced invalid kinds before IPC, including synthetic private data and old categories', () => {
    for (const kind of ['secret=FAKE_TOKEN https://synthetic.invalid prompt=FAKE_PROMPT', 'audio', 'execution', '', null, {}]) {
      recordRuntimeDiagnostic(kind as RuntimeDiagnosticKind)
    }
    expect(invokeTauriCommand).not.toHaveBeenCalled()
  })

  it('cannot accept an arbitrary detail payload', () => {
    // @ts-expect-error The wire API has no detail argument, even for a valid category.
    recordRuntimeDiagnostic('frontend_notice', 'secret=FAKE_TOKEN prompt=FAKE_PROMPT')
    expect(invokeTauriCommand).toHaveBeenCalledExactlyOnceWith('record_frontend_runtime_diagnostic', { event: { kind: 'frontend_notice' } })
  })

  it('contains both synchronous bridge throws and asynchronous persistence rejection', async () => {
    vi.mocked(invokeTauriCommand).mockImplementationOnce(() => { throw new Error('Synthetic unavailable bridge') })
    expect(() => recordRuntimeDiagnostic('execution_failed')).not.toThrow()
    vi.mocked(invokeTauriCommand).mockRejectedValueOnce(new Error('Synthetic journal failure'))
    expect(() => recordRuntimeDiagnostic('audio_failed')).not.toThrow()
    await Promise.resolve()
    expect(invokeTauriCommand).toHaveBeenCalledTimes(2)
  })
})

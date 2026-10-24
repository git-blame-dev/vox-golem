import { describe, expect, it, vi } from 'vitest'
import { SessionPromptController } from './SessionPromptController'
import type { NativePromptProjection } from '../lib/desktopRuntime'

describe('native prompt presentation identity', () => {
  it('ignores delayed updates, replacement snapshots and finishes for an older execution with the same renderer correlation', async () => {
    const controller = new SessionPromptController()
    const older: NativePromptProjection = { requestId: 'synthetic-reused', executionId: { sessionId: '1', generation: '2' }, prompt: 'Synthetic older prompt', updates: [], result: null }
    const newer = { ...older, executionId: { sessionId: '1', generation: '3' }, prompt: 'Synthetic newer prompt' }
    const update = vi.fn(); const replace = vi.fn(); const settled = vi.fn()
    const pending = controller.observeNative(newer, update, replace).then(settled)
    replace.mockClear()
    controller.updateNative(older.requestId, older.executionId, { requestId: older.requestId, kind: 'text', text: 'Synthetic stale text' })
    controller.replaceNative({ ...older, updates: [{ requestId: older.requestId, kind: 'text', text: 'Synthetic stale snapshot' }], result: { requestId: older.requestId, runtimePhase: 'sleeping', outcome: 'cancelled', errorMessage: null } })
    controller.finishNative(older.requestId, older.executionId, { requestId: older.requestId, runtimePhase: 'sleeping', outcome: 'cancelled', errorMessage: null })
    await Promise.resolve()
    expect(update).not.toHaveBeenCalled(); expect(replace).not.toHaveBeenCalled(); expect(settled).not.toHaveBeenCalled()
    controller.updateNative(newer.requestId, newer.executionId, { requestId: newer.requestId, kind: 'text', text: 'Synthetic current text' })
    expect(update).toHaveBeenCalledWith({ requestId: newer.requestId, kind: 'text', text: 'Synthetic current text' })
    controller.finishNative(newer.requestId, newer.executionId, { requestId: newer.requestId, runtimePhase: 'sleeping', outcome: 'cancelled', errorMessage: null })
    await pending
    expect(settled).toHaveBeenCalledTimes(1)
    expect(controller.isNativeRetired(newer)).toBe(true)
    expect(controller.isNativeRetired(older)).toBe(false)
  })
})

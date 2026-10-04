import { act } from 'react'
import { createRoot } from 'react-dom/client'
import type { Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { CaptureRecord } from '../lib/captures'

const captureMocks = vi.hoisted(() => ({
  listCaptures: vi.fn(), loadCaptureSnapshot: vi.fn(), playCapture: vi.fn(), retryCapture: vi.fn(), finalizeCapture: vi.fn(),
  selectNotesFolder: vi.fn(), stopCapturePlayback: vi.fn(), updateCaptureTranscript: vi.fn(), enableCapture: vi.fn(),
}))

vi.mock('../lib/captures', () => captureMocks)
import { CaptureHistory } from './CaptureHistory'

let root: Root | null = null
let container: HTMLElement | null = null

afterEach(() => {
  act(() => root?.unmount())
  root = null
  container?.remove()
  container = null
  vi.clearAllMocks()
})

beforeEach(() => {
  captureMocks.stopCapturePlayback.mockResolvedValue(undefined)
  captureMocks.updateCaptureTranscript.mockResolvedValue(undefined)
})

describe('CaptureHistory', () => {
  it('requires an explicit action to finalize an interrupted recording', async () => {
    const interrupted = { ...capture(), captureState: 'INTERRUPTED', recognitionState: 'QUEUED' }
    captureMocks.loadCaptureSnapshot.mockResolvedValue({ available: true, notesFolderUri: null, captures: [interrupted], error: null })
    captureMocks.finalizeCapture.mockResolvedValue(undefined)
    await mount()
    await act(async () => { container?.querySelector<HTMLButtonElement>('.capture-history__record')?.click() })
    expect(captureMocks.finalizeCapture).not.toHaveBeenCalled()
    const finalize = Array.from(container?.querySelectorAll('button') ?? []).find((button) => button.textContent === 'Finalize interrupted capture')
    await act(async () => { finalize?.click(); await Promise.resolve() })
    expect(captureMocks.finalizeCapture).toHaveBeenCalledWith('synthetic-capture-1')
  })
  it('distinguishes preview text from final and keeps independent state badges', async () => {
    captureMocks.loadCaptureSnapshot.mockResolvedValue({ available: true, notesFolderUri: null, captures: [capture()], error: null })
    captureMocks.listCaptures.mockResolvedValue([])
    await mount()
    await act(async () => { container?.querySelector<HTMLButtonElement>('.capture-history__record')?.click() })

    expect(container?.textContent).toContain('Last recognized phrase')
    expect(container?.textContent).toContain('Last preview (not final)')
    expect(container?.textContent).toContain('Recognition: running')
    expect(container?.textContent).toContain('Archive: waiting for folder')
    expect(container?.textContent).not.toContain('content://synthetic')
  })

  it('updates an already-exported history transcript without implying inbox rewrite', async () => {
    const archived = { ...capture(), recognitionState: 'FINAL', finalText: 'Original task', orgState: 'EXPORTED' }
    captureMocks.loadCaptureSnapshot.mockResolvedValue({ available: true, notesFolderUri: 'content://synthetic/tree', captures: [archived], error: null })
    captureMocks.listCaptures.mockResolvedValue([])
    await mount()
    await act(async () => { container?.querySelector<HTMLButtonElement>('.capture-history__record')?.click() })
    const editor = container?.querySelector<HTMLTextAreaElement>('textarea')
    if (editor === undefined || editor === null) throw new Error('Missing transcript editor')
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set
      setter?.call(editor, 'Corrected task')
      editor.dispatchEvent(new Event('input', { bubbles: true }))
    })
    await act(async () => {
      const save = Array.from(container?.querySelectorAll('button') ?? []).find((button) => button.textContent === 'Save correction')
      save?.click()
      await Promise.resolve()
    })
    expect(captureMocks.updateCaptureTranscript).toHaveBeenCalledWith('synthetic-capture-1', 'Corrected task')
    expect(container?.textContent).toContain('History edited; inbox entry unchanged.')
  })

  it('loads older pages from the native history instead of truncating at the snapshot page', async () => {
    const firstPage = Array.from({ length: 100 }, (_, index) => ({ ...capture(), captureId: `synthetic-${index}` }))
    captureMocks.loadCaptureSnapshot.mockResolvedValue({ available: true, notesFolderUri: null, captures: firstPage, error: null })
    captureMocks.listCaptures.mockImplementation(async (offset: number, limit: number) => offset === 100 && limit === 1
      ? [capture()]
      : offset === 100 ? [capture()] : [])
    await mount()
    expect(container?.querySelectorAll('.capture-history__record')).toHaveLength(100)
    await act(async () => {
      const older = Array.from(container?.querySelectorAll('button') ?? []).find((button) => button.textContent === 'Older')
      older?.click()
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(captureMocks.listCaptures).toHaveBeenCalledWith(100, 100)
    expect(container?.querySelectorAll('.capture-history__record')).toHaveLength(1)
  })

  it('enables watch dictation explicitly and reports success without using the phone microphone', async () => {
    const pendingSetup = { available: true, setupRequired: true, notesFolderUri: null, captures: [capture()], error: null }
    const enabled = { ...pendingSetup, setupRequired: false }
    captureMocks.loadCaptureSnapshot.mockResolvedValue(pendingSetup)
    captureMocks.enableCapture.mockResolvedValue(enabled)
    captureMocks.listCaptures.mockResolvedValue([])
    await mount()

    expect(container?.textContent).toContain('This flow uses watch-provided audio only. It never records through the phone microphone.')
    expect(container?.textContent).toContain('RECORD_AUDIO')
    const enable = container?.querySelector<HTMLButtonElement>('button[aria-label="Enable watch dictation"]')
    expect(enable).not.toBeNull()
    expect(enable?.disabled).toBe(false)
    await act(async () => {
      enable?.click()
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(captureMocks.enableCapture).toHaveBeenCalledOnce()
    expect(container?.textContent).toContain('Watch dictation is enabled.')
    expect(container?.querySelector('[aria-label="Enable watch dictation"]')).toBeNull()
  })

  it('shows a denied setup permission and leaves an explicit retry action available', async () => {
    captureMocks.loadCaptureSnapshot.mockResolvedValue({ ...snapshotWithSetupRequired(), captures: [capture()] })
    captureMocks.enableCapture.mockRejectedValueOnce(new Error('RECORD_AUDIO permission denied')).mockResolvedValue({
      ...snapshotWithSetupRequired(), setupRequired: false, captures: [capture()],
    })
    captureMocks.listCaptures.mockResolvedValue([])
    await mount()

    await act(async () => {
      container?.querySelector<HTMLButtonElement>('button[aria-label="Enable watch dictation"]')?.click()
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(container?.textContent).toContain('RECORD_AUDIO permission denied')
    const retry = container?.querySelector<HTMLButtonElement>('button[aria-label="Enable watch dictation"]')
    expect(retry?.disabled).toBe(false)
    await act(async () => { retry?.click(); await Promise.resolve(); await Promise.resolve() })
    expect(captureMocks.enableCapture).toHaveBeenCalledTimes(2)
    expect(container?.textContent).toContain('Watch dictation is enabled.')
  })
})

async function mount(): Promise<void> {
  container = document.createElement('div')
  document.body.append(container)
  root = createRoot(container)
  await act(async () => {
    root?.render(<CaptureHistory onBack={() => undefined} />)
    for (let index = 0; index < 6; index += 1) await Promise.resolve()
  })
}

function capture(): CaptureRecord {
  return {
    captureId: 'synthetic-capture-1', createdAtUtc: '2026-01-02T03:04:05Z', finishedAtUtc: null, durationMs: 30_000,
    captureState: 'SAVED', recognitionState: 'RUNNING', transferState: 'PENDING', archiveState: 'WAITING_FOR_FOLDER', orgState: 'WAITING_FOR_TRANSCRIPT',
    previewText: 'Last recognized phrase', finalText: null, archivedAudioUri: null, error: null, transcriptVersion: 1,
  }
}

function snapshotWithSetupRequired(): { available: boolean; setupRequired: boolean; notesFolderUri: null; captures: CaptureRecord[]; error: null } {
  return { available: true, setupRequired: true, notesFolderUri: null, captures: [], error: null }
}

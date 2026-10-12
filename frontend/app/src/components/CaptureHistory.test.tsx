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
  it.each(['ASK', 'TALK', 'DRAFT'] as const)('does not offer Note finalization for interrupted %s audio', async (purpose) => {
    captureMocks.loadCaptureSnapshot.mockResolvedValue({ available: true, notesFolderUri: null,
      captures: [{ ...capture(), purpose, captureState: 'INTERRUPTED' }], error: null })
    await mount()
    await act(async () => { container?.querySelector<HTMLButtonElement>('.capture-history__record')?.click() })
    expect(Array.from(container?.querySelectorAll('button') ?? []).some((button) => button.textContent === 'Finalize interrupted capture')).toBe(false)
    expect(captureMocks.finalizeCapture).not.toHaveBeenCalled()
  })
  it('shows retained audio and unavailable live recognition as separate outcomes with captured policy', async () => {
    const record = { ...capture(), requestedLanguage: 'en-AU', speechNetworkFallback: false,
      liveTranscriptionState: 'UNAVAILABLE' as const, liveStatusRevision: 3,
      canTranscribeLive: false, liveErrorCode: 'external-audio-provider-unavailable' }
    captureMocks.loadCaptureSnapshot.mockResolvedValue({ available: true, notesFolderUri: null, captures: [record], error: null })
    await mount()
    await act(async () => { container?.querySelector<HTMLButtonElement>('.capture-history__record')?.click() })
    expect(container?.textContent).toContain('Live: unavailable')
    expect(container?.textContent).toContain('Audio retained; live transcription unavailable')
    expect(container?.textContent).toContain('Input language: en-AU')
    expect(container?.textContent).toContain('Speech policy: dedicated on-device only')
    expect(container?.textContent).not.toContain('Transcribing audio now')
  })
  it('keeps retained Ask audio separate from Notes processing and transcript correction', async () => {
    const record = { ...capture(), purpose: 'ASK' as const, recognitionState: 'FAILED' as const }
    captureMocks.loadCaptureSnapshot.mockResolvedValue({ available: true, notesFolderUri: null, captures: [record], error: null })
    await mount()
    await act(async () => { container?.querySelector<HTMLButtonElement>('.capture-history__record')?.click() })
    expect(container?.textContent).toContain('Purpose: ask')
    expect(container?.textContent).not.toContain('Archive:')
    expect(container?.textContent).not.toContain('Org:')
    const buttons = Array.from(container?.querySelectorAll('button') ?? [])
    expect(buttons.find((button) => button.textContent === 'Retry processing')?.disabled).toBe(true)
    expect(buttons.find((button) => button.textContent === 'Save correction')?.disabled).toBe(true)
    const play = buttons.find((button) => button.textContent === 'Play recording')!
    await act(async () => { play.click() })
    expect(captureMocks.playCapture).toHaveBeenCalledWith(record.captureId)
    expect(captureMocks.retryCapture).not.toHaveBeenCalled()
    expect(captureMocks.updateCaptureTranscript).not.toHaveBeenCalled()
  })
  it.each(['speech-recognizer-7', 'no-speech-detected'])('quietly presents exact capture no-match %s without promoting the draft or changing retained audio', async (error) => {
    const record = { ...capture(), recognitionState: 'FAILED' as const, previewText: 'Synthetic unconfirmed preview', finalText: null, archivedAudioUri: 'content://synthetic/retained-audio', error }
    captureMocks.loadCaptureSnapshot.mockResolvedValue({ available: true, notesFolderUri: null, captures: [record], error })
    await mount()
    await act(async () => { container?.querySelector<HTMLButtonElement>('.capture-history__record')?.click() })
    expect(container?.querySelector('[role="alert"]')).toBeNull()
    expect(container?.textContent).not.toContain(error)
    expect(container?.textContent).toContain('Last preview (not final)')
    expect(container?.textContent).toContain('Synthetic unconfirmed preview')
    expect(container?.textContent).toContain('Recognition: failed')
    expect(record.error).toBe(error)
    expect(record.finalText).toBeNull()
    expect(record.archivedAudioUri).toBe('content://synthetic/retained-audio')
    const play = Array.from(container?.querySelectorAll('button') ?? []).find((button) => button.textContent === 'Play recording')!
    await act(async () => { play.click() })
    expect(captureMocks.playCapture).toHaveBeenCalledWith(record.captureId)
    expect(captureMocks.updateCaptureTranscript).not.toHaveBeenCalled()
    expect(captureMocks.finalizeCapture).not.toHaveBeenCalled()
    expect(captureMocks.retryCapture).not.toHaveBeenCalled()
  })
  it.each([false, true])('keeps actionable capture errors visible alongside a no-match code (no-match primary: %s)', async (noMatchPrimary) => {
    captureMocks.loadCaptureSnapshot.mockResolvedValue({ available: true, notesFolderUri: null, captures: [{ ...capture(), error: 'Synthetic audio retirement failed' }], error: noMatchPrimary ? 'no-speech-detected' : 'Synthetic archive write failed', onboardingError: noMatchPrimary ? 'Synthetic archive write failed' : 'no-speech-detected' })
    await mount()
    await act(async () => { container?.querySelector<HTMLButtonElement>('.capture-history__record')?.click() })
    expect(container?.querySelector('[role="alert"]')?.textContent).toBe('Synthetic archive write failed')
    expect(container?.textContent).toContain('Synthetic audio retirement failed')
  })
  it('does not suppress composite recognition and retirement failures merely containing a no-match code', async () => {
    const error = 'speech-recognizer-7:retirement-failed'
    captureMocks.loadCaptureSnapshot.mockResolvedValue({ available: true, notesFolderUri: null, captures: [{ ...capture(), error }], error })
    await mount()
    await act(async () => { container?.querySelector<HTMLButtonElement>('.capture-history__record')?.click() })
    expect(container?.querySelector('[role="alert"]')?.textContent).toBe(error)
    expect(container?.querySelector('.capture-history__detail .capture-history__error')?.textContent).toBe(error)
  })
  it('shows starting until a refreshed snapshot confirms foreground readiness', async () => {
    const pending = snapshotWithSetupRequired()
    captureMocks.loadCaptureSnapshot.mockResolvedValue(pending)
    captureMocks.enableCapture.mockResolvedValue({ ...pending, onboardingState: 'starting', onboardingError: null })
    await mount()
    await act(async () => { container?.querySelector<HTMLButtonElement>('button[aria-label="Enable watch dictation"]')?.click() })
    expect(container?.textContent).toContain('Watch dictation is starting…')
    expect(container?.querySelector('[role="alert"]')).toBeNull()
    expect(container?.querySelector<HTMLButtonElement>('button[aria-label="Enable watch dictation"]')?.disabled).toBe(true)
    captureMocks.loadCaptureSnapshot.mockResolvedValue({ ...pending, setupRequired: false, onboardingState: 'enabled', onboardingError: null })
    await act(async () => {
      Array.from(container?.querySelectorAll('button') ?? []).find((button) => button.textContent === 'Refresh')?.click()
    })
    expect(container?.textContent).toContain('Watch dictation is enabled.')
    expect(container?.textContent).not.toContain('Watch dictation is starting…')
  })

  it('shows an asynchronous runtime failure after startup and permits explicit retry', async () => {
    captureMocks.loadCaptureSnapshot.mockResolvedValue({ ...snapshotWithSetupRequired(), onboardingState: 'failed', onboardingError: 'synthetic-service-denied' })
    await mount()
    expect(container?.querySelector('[role="alert"]')?.textContent).toBe('synthetic-service-denied')
    expect(container?.querySelector<HTMLButtonElement>('button[aria-label="Enable watch dictation"]')?.disabled).toBe(false)
  })
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

    expect(container?.textContent).toContain('For Watch Notes, the app supplies Watch audio to the recognizer.')
    expect(container?.textContent).toContain('Provider input-source isolation is not yet verified.')
    expect(container?.textContent).not.toContain('never records through the phone microphone')
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
    captureId: 'synthetic-capture-1', purpose: 'NOTE', requestedLanguage: null, speechNetworkFallback: false,
    liveTranscriptionState: 'NOT_REQUESTED', liveStatusRevision: 0, canTranscribeLive: false, liveErrorCode: null,
    createdAtUtc: '2026-01-02T03:04:05Z', finishedAtUtc: null, durationMs: 30_000,
    captureState: 'SAVED', recognitionState: 'RUNNING', transferState: 'PENDING', archiveState: 'WAITING_FOR_FOLDER', orgState: 'WAITING_FOR_TRANSCRIPT',
    previewText: 'Last recognized phrase', finalText: null, archivedAudioUri: null, error: null, transcriptVersion: 1,
  }
}

function snapshotWithSetupRequired(): { available: boolean; setupRequired: boolean; notesFolderUri: null; captures: CaptureRecord[]; error: null } {
  return { available: true, setupRequired: true, notesFolderUri: null, captures: [], error: null }
}

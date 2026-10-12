import { useCallback, useEffect, useRef, useState } from 'react'
import type { JSX } from 'react'
import {
  enableCapture, finalizeCapture, listCaptures, loadCaptureSnapshot, playCapture, retryCapture, selectNotesFolder, stopCapturePlayback,
  updateCaptureTranscript,
} from '../lib/captures'
import type { CaptureRecord, CaptureSnapshot } from '../lib/captures'
import { visibleSpeechError } from '../lib/androidVoice'

const PAGE_SIZE = 100

export function CaptureHistory({ onBack }: { onBack: () => void }): JSX.Element {
  const [snapshot, setSnapshot] = useState<CaptureSnapshot | null>(null)
  const [records, setRecords] = useState<CaptureRecord[]>([])
  const [offset, setOffset] = useState(0)
  const [hasOlder, setHasOlder] = useState(false)
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [draft, setDraft] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [setupMessage, setSetupMessage] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [playingId, setPlayingId] = useState<string | null>(null)
  const [editedExportedIds, setEditedExportedIds] = useState<ReadonlySet<string>>(() => new Set())
  const mounted = useRef(true)
  const refreshRevision = useRef(0)
  const selected = records.find((record) => record.captureId === selectedId) ?? null

  const refresh = useCallback(async (page: number): Promise<void> => {
    const revision = ++refreshRevision.current
    try {
      const nextSnapshot = await loadCaptureSnapshot()
      let nextRecords = nextSnapshot.captures
      let more = false
      if (page > 0) {
        nextRecords = await listCaptures(page, PAGE_SIZE)
        if (nextRecords.length === PAGE_SIZE) more = (await listCaptures(page + PAGE_SIZE, 1)).length > 0
      } else if (nextRecords.length === PAGE_SIZE) {
        more = (await listCaptures(PAGE_SIZE, 1)).length > 0
      }
      if (!mounted.current || revision !== refreshRevision.current) return
      nextRecords = [...nextRecords].sort((left, right) => Date.parse(right.createdAtUtc) - Date.parse(left.createdAtUtc))
      setSnapshot(nextSnapshot)
      setRecords(nextRecords)
      setOffset(page)
      setHasOlder(more)
      setError(nextSnapshot.error ?? nextSnapshot.onboardingError ?? null)
      setSetupMessage(nextSnapshot.onboardingState === 'starting' ? 'Watch dictation is starting…'
        : nextSnapshot.onboardingState === 'enabled' ? 'Watch dictation is enabled.' : null)
      setSelectedId((current) => current !== null && nextRecords.some(({ captureId }) => captureId === current) ? current : null)
    } catch (reason) {
      if (mounted.current && revision === refreshRevision.current) setError(errorMessage(reason))
    }
  }, [])

  useEffect(() => {
    mounted.current = true
    void Promise.resolve().then(() => refresh(offset))
    const poll = (): void => {
      if (document.visibilityState === 'visible') void refresh(offset)
    }
    const timer = window.setInterval(poll, 5_000)
    document.addEventListener('visibilitychange', poll)
    window.addEventListener('focus', poll)
    return () => {
      mounted.current = false
      window.clearInterval(timer)
      document.removeEventListener('visibilitychange', poll)
      window.removeEventListener('focus', poll)
      void stopCapturePlayback().catch(() => undefined)
    }
  }, [offset, refresh])

  const chooseFolder = async (): Promise<void> => {
    setBusy(true)
    setError(null)
    try {
      const next = await selectNotesFolder()
      if (mounted.current) {
        setSnapshot(next)
        setError(next.error)
        await refresh(offset)
      }
    } catch (reason) { if (mounted.current) setError(errorMessage(reason)) }
    finally { if (mounted.current) setBusy(false) }
  }

  const enableWatchDictation = async (): Promise<void> => {
    setBusy(true)
    setError(null)
    setSetupMessage(null)
    try {
      const next = await enableCapture()
      if (!mounted.current) return
      setSnapshot(next)
      if (offset === 0) setRecords(next.captures)
      if (next.onboardingState === 'starting') {
        setSetupMessage('Watch dictation is starting…')
        setError(null)
      } else if (next.setupRequired) {
        setError(next.error ?? next.onboardingError ?? 'Watch dictation setup is still required. Review the requested permissions and try again.')
      } else {
        setSetupMessage('Watch dictation is enabled.')
        setError(next.error)
      }
    } catch (reason) {
      if (mounted.current) setError(errorMessage(reason))
    } finally {
      if (mounted.current) setBusy(false)
    }
  }

  const selectRecord = (record: CaptureRecord): void => {
    setSelectedId(record.captureId)
    setDraft(record.finalText ?? record.previewText ?? '')
  }

  const play = async (record: CaptureRecord): Promise<void> => {
    setBusy(true)
    setError(null)
    try {
      if (playingId === record.captureId) {
        await stopCapturePlayback()
        setPlayingId(null)
      } else {
        if (playingId !== null) await stopCapturePlayback()
        await playCapture(record.captureId)
        setPlayingId(record.captureId)
      }
    } catch (reason) { if (mounted.current) setError(errorMessage(reason)) }
    finally { if (mounted.current) setBusy(false) }
  }

  const retry = async (record: CaptureRecord): Promise<void> => {
    setBusy(true)
    setError(null)
    try { await retryCapture(record.captureId); await refresh(offset) }
    catch (reason) { if (mounted.current) setError(errorMessage(reason)) }
    finally { if (mounted.current) setBusy(false) }
  }

  const finalize = async (record: CaptureRecord): Promise<void> => {
    if (record.purpose !== 'NOTE') return
    setBusy(true)
    setError(null)
    try { await finalizeCapture(record.captureId); await refresh(offset) }
    catch (reason) { if (mounted.current) setError(errorMessage(reason)) }
    finally { if (mounted.current) setBusy(false) }
  }

  const saveTranscript = async (): Promise<void> => {
    if (selected === null || selected.purpose !== 'NOTE') return
    setBusy(true)
    setError(null)
    try {
      await updateCaptureTranscript(selected.captureId, draft)
      if (isExported(selected.orgState)) setEditedExportedIds((current) => new Set(current).add(selected.captureId))
      await refresh(offset)
    }
    catch (reason) { if (mounted.current) setError(errorMessage(reason)) }
    finally { if (mounted.current) setBusy(false) }
  }

  const displayedError = visibleSpeechError(error) ?? visibleSpeechError(snapshot?.onboardingError)
  return <main className="capture-history" aria-label="Voice notes">
    <header className="capture-history__header">
      <div><h1>Voice notes</h1><p>Capture history and notes-folder settings</p></div>
      <button type="button" className="shell__control" onClick={onBack}>Back to chat</button>
    </header>
    {snapshot?.setupRequired ? <section className="capture-folder" aria-label="Enable watch dictation">
      <div>
        <strong>Enable watch dictation</strong>
        <p>Setup requests RECORD_AUDIO for watch-provided audio, BLUETOOTH_CONNECT, and the required connected-device foreground service access. For Watch Notes, the app supplies Watch audio to the recognizer. Provider input-source isolation is not yet verified.</p>
      </div>
       <button type="button" className="shell__control" aria-label="Enable watch dictation" onClick={() => void enableWatchDictation()} disabled={busy || snapshot.onboardingState === 'starting'}>Enable watch dictation</button>
    </section> : null}
    {setupMessage !== null ? <p role="status">{setupMessage}</p> : null}
    <section className="capture-folder" aria-label="Notes folder">
      <div><strong>Notes folder</strong><p>{snapshot?.notesFolderUri ? 'A notes folder is selected' : 'Choose notes folder to archive/export'}</p></div>
      <button type="button" className="shell__control" onClick={() => void chooseFolder()} disabled={busy || snapshot?.available === false}>Choose folder</button>
    </section>
    {snapshot?.available === false ? <p role="status">Voice capture service is unavailable on this device.</p> : null}
    {displayedError !== null ? <p className="capture-history__error" role="alert">{displayedError}</p> : null}
    <div className="capture-history__layout">
      <section className="capture-history__list" aria-label="Capture history">
        <div className="capture-history__list-heading"><h2>History</h2><button type="button" className="shell__control" onClick={() => void refresh(offset)} disabled={busy}>Refresh</button></div>
        {snapshot === null && error === null ? <p>Loading captures…</p> : null}
        {records.length === 0 && snapshot !== null ? <p>No saved captures yet.</p> : null}
        <ul>{records.map((record) => <li key={record.captureId}>
          <button type="button" className="capture-history__record" aria-current={selectedId === record.captureId ? 'true' : undefined} onClick={() => selectRecord(record)}>
            <strong><span aria-label="Recorded audio" title="Recorded audio">🎙</span>{' '}{record.finalText ?? record.previewText ?? 'Capture without transcript'}</strong>
            <span>{formatDate(record.finishedAtUtc ?? record.createdAtUtc)} · {formatDuration(record.durationMs)}</span>
            <span className="capture-history__badges">
              <StatusBadge label="Capture" value={record.captureState} />
              <StatusBadge label="Purpose" value={record.purpose} />
              <StatusBadge label="Recognition" value={record.recognitionState} />
              {record.liveTranscriptionState !== 'NOT_REQUESTED' ? <StatusBadge label="Live" value={record.liveTranscriptionState} /> : null}
              <StatusBadge label="Transfer" value={record.transferState} />
              {record.purpose === 'NOTE' ? <><StatusBadge label="Archive" value={record.archiveState} />
                <StatusBadge label="Org" value={record.orgState} /></> : null}
            </span>
          </button>
        </li>)}</ul>
        <div className="capture-history__pagination">
          <button type="button" className="shell__control" disabled={offset === 0 || busy} onClick={() => void refresh(Math.max(0, offset - PAGE_SIZE))}>Newer</button>
          <span>{offset + 1}–{offset + records.length}</span>
          <button type="button" className="shell__control" disabled={!hasOlder || busy} onClick={() => void refresh(offset + PAGE_SIZE)}>Older</button>
        </div>
      </section>
      <section className="capture-history__detail" aria-label="Capture details">
        {selected === null ? <p>Select a capture to see its details.</p> : <>
          <h2>Capture details</h2>
          <p>{formatDate(selected.finishedAtUtc ?? selected.createdAtUtc)} · {formatDuration(selected.durationMs)}</p>
          <p>Input language: {selected.requestedLanguage ?? 'Phone default (legacy)'}</p>
          <p>Speech policy: {selected.speechNetworkFallback ? 'Network fallback allowed' : 'dedicated on-device only'}</p>
          {liveStatusMessage(selected) !== null ? <p role="status">{liveStatusMessage(selected)}</p> : null}
          {visibleSpeechError(selected.liveErrorCode) !== null ? <p className="capture-history__error">{visibleSpeechError(selected.liveErrorCode)}</p> : null}
          {selected.finalText !== null
            ? <><h3>Final transcript</h3><p className="capture-history__transcript">{selected.finalText}</p></>
            : selected.previewText !== null
              ? <><h3>Last preview (not final)</h3><p className="capture-history__transcript">{selected.previewText}</p></>
              : <p>No transcript available.</p>}
          {visibleSpeechError(selected.error) !== null ? <p className="capture-history__error">{visibleSpeechError(selected.error)}</p> : null}
          <div className="capture-history__actions">
            {selected.purpose === 'NOTE' && selected.captureState === 'INTERRUPTED' ? <button type="button" className="shell__control" disabled={busy} onClick={() => void finalize(selected)}>Finalize interrupted capture</button> : null}
            <button type="button" className="shell__control" disabled={busy} onClick={() => void play(selected)}>{playingId === selected.captureId ? 'Stop playback' : 'Play recording'}</button>
            <button type="button" className="shell__control" disabled={busy || !canRetry(selected)} onClick={() => void retry(selected)}>Retry processing</button>
          </div>
          <label className="capture-history__edit">Correct transcript
            <textarea value={draft} onChange={(event) => setDraft(event.target.value)} rows={6} disabled={selected.captureState !== 'SAVED' || selected.purpose !== 'NOTE'} />
          </label>
          <button type="button" className="shell__control" disabled={busy || selected.captureState !== 'SAVED' || selected.purpose !== 'NOTE' || draft === (selected.finalText ?? selected.previewText ?? '')} onClick={() => void saveTranscript()}>Save correction</button>
          {selected.purpose !== 'NOTE' ? <p className="capture-history__note">Audio retained for this voice session. Notes export and note corrections do not modify its conversation.</p>
            : isExported(selected.orgState) && editedExportedIds.has(selected.captureId)
            ? <p className="capture-history__note">History edited; inbox entry unchanged.</p>
            : isExported(selected.orgState)
              ? <p className="capture-history__note">Corrections update history only; the exported inbox entry will not be changed.</p>
              : null}
        </>}
      </section>
    </div>
  </main>
}

function StatusBadge({ label, value }: { label: string; value: string }): JSX.Element {
  return <span className="capture-history__badge"><strong>{label}:</strong> {humanize(value)}</span>
}

function liveStatusMessage(record: CaptureRecord): string | null {
  switch (record.liveTranscriptionState) {
    case 'NOT_REQUESTED': return null
    case 'CONNECTING': return 'Connecting to live transcription'
    case 'PENDING': return 'Live transcription is pending'
    case 'ATTEMPTING': return 'Starting speech recognition'
    case 'PROVIDER_READY': return 'Speech provider is ready; waiting for recognized words'
    case 'TRANSCRIBING': return 'Transcribing audio now'
    case 'UNAVAILABLE': return record.captureState === 'SAVED' || record.captureState === 'INTERRUPTED'
      ? 'Audio retained; live transcription unavailable' : 'Live transcription unavailable; audio capture is independent'
    case 'COMPLETED': return 'Live transcription stream completed'
  }
}

function canRetry(record: CaptureRecord): boolean {
  return record.purpose === 'NOTE' && record.captureState === 'SAVED' && (['FAILED', 'QUEUED', 'UNAVAILABLE'].includes(record.recognitionState) || record.transferState === 'PENDING' || ['RETRYABLE_ERROR', 'WAITING_FOR_FOLDER'].includes(record.archiveState) || record.orgState === 'RETRYABLE_ERROR')
}

function isExported(state: string): boolean { return state === 'EXPORTED' || state === 'ORG_EXPORTED' }
function humanize(value: string): string { return value.toLowerCase().replaceAll('_', ' ') }
function formatDuration(milliseconds: number): string {
  const seconds = Math.floor(milliseconds / 1000)
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`
}
function formatDate(value: string): string { return new Date(value).toLocaleString() }
function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message
  if (typeof error === 'string') return error
  if (typeof error === 'object' && error !== null && 'message' in error && typeof error.message === 'string') return error.message
  return 'Capture service request failed'
}

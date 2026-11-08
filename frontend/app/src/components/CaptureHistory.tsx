import { Button } from '../ui/controls'
import { useEffect, useState } from 'react'
import type { JSX } from 'react'
import {
  enableCapture, finalizeCapture, playCapture, retryCapture, selectNotesFolder, stopCapturePlayback,
  updateCaptureTranscript,
} from '../lib/captures'
import type { CaptureRecord } from '../lib/captures'
import { visibleSpeechError } from '../lib/androidVoice'
import { CAPTURE_PAGE_SIZE, captureErrorMessage as errorMessage, useCaptureHistory } from '../lib/useCaptureHistory'
import type { CaptureDraftMemory } from '../lib/useCaptureHistory'
import { Disclosure } from '../ui/controls'

export function CaptureHistory({ onBack, draftMemory }: { onBack: () => void; draftMemory?: CaptureDraftMemory }): JSX.Element {
  const { snapshot, setSnapshot, playback, records, setRecords, offset, hasOlder, selectedId, selected, draft, draftVersion, conflict, offPage,
    error, setError, correctionSaving, setupMessage, setSetupMessage, mounted, refresh, selectRecord, setDraft, correctionRequest } = useCaptureHistory(draftMemory)
  const [operationBusy, setBusy] = useState(false)
  const busy = operationBusy || correctionSaving
  const [pendingSelection, setPendingSelection] = useState<CaptureRecord | null>(null)
  const selectCapture = (record: CaptureRecord) => {
    if (selected !== null && draft !== (selected.finalText ?? selected.previewText ?? '')) {
      setPendingSelection(record)
    } else selectRecord(record)
  }
  const playingId = playback?.phase === 'preparing' || playback?.phase === 'playing' ? playback.captureId : null
  const [editedExportedIds, setEditedExportedIds] = useState<ReadonlySet<string>>(() => new Set())
  useEffect(() => () => { void stopCapturePlayback().catch(() => undefined) }, [])

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

  const play = async (record: CaptureRecord): Promise<void> => {
    setBusy(true)
    setError(null)
    try {
      if (playingId === record.captureId) {
        await stopCapturePlayback()
      } else {
        await playCapture(record.captureId)
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
    if (selected === null || selected.purpose !== 'NOTE' || conflict || offPage || draftVersion === undefined) return
    setBusy(true)
    setError(null)
    const request = correctionRequest()
    try {
      const record = await updateCaptureTranscript(selected.captureId, draft, draftVersion)
      request.accept(record)
      if (!mounted.current) return
      if (isExported(selected.orgState)) setEditedExportedIds((current) => new Set(current).add(selected.captureId))
      await refresh(offset)
    }
    catch (reason) {
      if (errorMessage(reason) === 'transcript-conflict') {
        request.conflict()
        if (request.current()) await refresh(offset)
      } else request.fail(errorMessage(reason))
    }
    finally { request.finish(); if (mounted.current) setBusy(false) }
  }

  const displayedError = visibleSpeechError(error) ?? visibleSpeechError(snapshot?.onboardingError)
  return <main className="capture-history" aria-label="Voice notes">
    <header className="capture-history__header">
      <div><h1>Voice notes</h1><p>Capture history and notes-folder settings</p></div>
      <Button type="button" className="ui-button--utility" onClick={onBack}>Back to chat</Button>
    </header>
    {snapshot?.setupRequired ? <section className="capture-folder" aria-label="Enable watch dictation">
      <div>
        <strong>Enable watch dictation</strong>
        <p>Setup requests RECORD_AUDIO for watch-provided audio, BLUETOOTH_CONNECT, and the required connected-device foreground service access. For Watch Notes, the app supplies Watch audio to the recognizer. Provider input-source isolation is not yet verified.</p>
      </div>
       <Button type="button" className="ui-button--utility" aria-label="Enable watch dictation" onClick={() => void enableWatchDictation()} disabled={busy || snapshot.onboardingState === 'starting'}>Enable watch dictation</Button>
    </section> : null}
    {setupMessage !== null ? <p role="status">{setupMessage}</p> : null}
    <section className="capture-folder" aria-label="Notes folder">
      <div><strong>Notes folder</strong><p>{snapshot?.notesFolderUri ? 'A notes folder is selected' : 'Choose notes folder to archive/export'}</p></div>
      <Button type="button" className="ui-button--utility" onClick={() => void chooseFolder()} disabled={busy || snapshot?.available === false}>Choose folder</Button>
    </section>
    {snapshot?.available === false ? <p role="status">Voice capture service is unavailable on this device.</p> : null}
    {displayedError !== null ? <p className="capture-history__error" role="alert">{displayedError}</p> : null}
    {pendingSelection ? <section className="capture-history__selection-warning" aria-label="Unsaved correction">
      <p role="status">Your correction has not been saved. Keep editing, or discard it before opening this capture.</p>
      <div className="capture-history__actions">
        <Button onClick={() => setPendingSelection(null)}>Keep editing</Button>
        <Button onClick={() => { selectRecord(pendingSelection); setPendingSelection(null) }}>Discard correction and open capture</Button>
      </div>
    </section> : null}
    <div className="capture-history__layout">
      <section className="capture-history__list" aria-label="Capture history">
        <div className="capture-history__list-heading"><h2>History</h2><Button type="button" className="ui-button--utility" onClick={() => void refresh(offset)} disabled={busy}>Refresh</Button></div>
        {snapshot === null && error === null ? <p>Loading captures…</p> : null}
        {records.length === 0 && snapshot !== null ? <p>No saved captures yet.</p> : null}
        <ul>{records.map((record) => <li key={record.captureId}>
          <Button type="button" className="capture-history__record" aria-current={selectedId === record.captureId ? 'true' : undefined} onClick={() => selectCapture(record)}>
            <strong>{record.finalText ?? record.previewText ?? 'Capture without transcript'}</strong>
            <span>{formatDate(record.finishedAtUtc ?? record.createdAtUtc)} · {formatDuration(record.durationMs)}</span>
            <span className="capture-history__badges">
              <StatusBadge label="Capture" value={record.captureState} />
              <StatusBadge label="Purpose" value={record.purpose} />
              <StatusBadge label="Recognition" value={record.recognitionState} />
            </span>
          </Button>
        </li>)}</ul>
        <div className="capture-history__pagination">
          <Button type="button" className="ui-button--utility" disabled={offset === 0 || busy} onClick={() => void refresh(Math.max(0, offset - CAPTURE_PAGE_SIZE))}>Newer</Button>
          <span>{offset + 1}–{offset + records.length}</span>
          <Button type="button" className="ui-button--utility" disabled={!hasOlder || busy} onClick={() => void refresh(offset + CAPTURE_PAGE_SIZE)}>Older</Button>
        </div>
      </section>
      <section className="capture-history__detail" aria-label="Capture details">
        {selected === null ? <p>Select a capture to see its details.</p> : <>
          <h2>Capture details</h2>
          <p>{formatDate(selected.finishedAtUtc ?? selected.createdAtUtc)} · {formatDuration(selected.durationMs)}</p>
          <p>Input language: {selected.requestedLanguage ?? 'Phone default (legacy)'}</p>
          <p>Speech policy: {selected.speechNetworkFallback ? 'Network fallback allowed' : 'dedicated on-device only'}</p>
          <Disclosure title="Processing and transfer details" className="capture-history__processing">
            <StatusBadge label="Transfer" value={selected.transferState} />
            <StatusBadge label="Live" value={selected.liveTranscriptionState} />
            {selected.purpose === 'NOTE' ? <><StatusBadge label="Archive" value={selected.archiveState} /><StatusBadge label="Org" value={selected.orgState} /></> : null}
          </Disclosure>
          {liveStatusMessage(selected) !== null ? <p role="status">{liveStatusMessage(selected)}</p> : null}
          {visibleSpeechError(selected.liveErrorCode) !== null ? <p className="capture-history__error">{visibleSpeechError(selected.liveErrorCode)}</p> : null}
          {selected.finalText !== null
            ? <><h3>Final transcript</h3><p className="capture-history__transcript">{selected.finalText}</p></>
            : selected.previewText !== null
              ? <><h3>Last preview (not final)</h3><p className="capture-history__transcript">{selected.previewText}</p></>
              : <p>No transcript available.</p>}
          {visibleSpeechError(selected.error) !== null ? <p className="capture-history__error">{visibleSpeechError(selected.error)}</p> : null}
          <div className="capture-history__actions">
            {playback?.captureId === selected.captureId ? <p role="status">{playback.phase === 'preparing' ? 'Preparing playback…' : playback.phase === 'playing' ? 'Playing recording' : playback.phase === 'completed' ? 'Playback completed' : playback.phase === 'failed' ? 'Playback failed' : 'Playback stopped'}</p> : null}
            {selected.purpose === 'NOTE' && selected.captureState === 'INTERRUPTED' ? <Button type="button" className="ui-button--utility" disabled={busy} onClick={() => void finalize(selected)}>Finalize interrupted capture</Button> : null}
            <Button type="button" className="ui-button--utility" disabled={busy} onClick={() => void play(selected)}>{playingId === selected.captureId ? 'Stop playback' : 'Play recording'}</Button>
            <Button type="button" className="ui-button--utility" disabled={busy || !canRetry(selected)} onClick={() => void retry(selected)}>Retry processing</Button>
          </div>
          <label className="capture-history__edit">Correct transcript
            <textarea value={draft} onChange={(event) => setDraft(event.target.value)} rows={6} disabled={selected.captureState !== 'SAVED' || selected.purpose !== 'NOTE'} />
          </label>
          {conflict ? <p role="alert">Transcript changed while you were editing. Your correction was preserved. Select the capture again to reload the latest transcript before correcting it.</p> : null}
          {offPage ? <p role="alert">This capture is no longer in this history page. Your correction was preserved. Return to its history page to review the latest transcript before saving.</p> : null}
          <Button type="button" className="ui-button--utility" disabled={busy || conflict || offPage || selected.captureState !== 'SAVED' || selected.purpose !== 'NOTE' || draft === (selected.finalText ?? selected.previewText ?? '')} onClick={() => void saveTranscript()}>Save correction</Button>
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

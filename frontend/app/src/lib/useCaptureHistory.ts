import { useCallback, useEffect, useRef, useState } from 'react'
import { listCaptures, loadCaptureSnapshot, listenCapturePlayback } from './captures'
import type { CaptureRecord, CaptureSnapshot, CapturePlayback } from './captures'

const PAGE_SIZE = 100
interface Correction { captureId: string; text: string; baselineText: string; version: number; conflicted: boolean; record: CaptureRecord }
const transcript = (record: CaptureRecord): string => record.finalText ?? record.previewText ?? ''

/** The history screen owns its polling lifetime, page requests and correction baseline. */
export function useCaptureHistory() {
  const [snapshot, setSnapshot] = useState<CaptureSnapshot | null>(null)
  const [playback, setPlayback] = useState<CapturePlayback | null>(null)
  const projectPlayback = useCallback((next: CapturePlayback): void => {
    setPlayback((previous) => previous === null || next.revision > previous.revision ? next : previous)
  }, [])
  const [records, setRecords] = useState<CaptureRecord[]>([])
  const [offset, setOffset] = useState(0)
  const [hasOlder, setHasOlder] = useState(false)
  const [correction, setCorrection] = useState<Correction | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [setupMessage, setSetupMessage] = useState<string | null>(null)
  const mounted = useRef(false)
  const lifetime = useRef(0)
  const requestedPage = useRef(0)
  const pendingPage = useRef<number | null>(null)
  const inFlight = useRef<Promise<void> | null>(null)

  const refresh = useCallback((page: number): Promise<void> => {
    requestedPage.current = page
    pendingPage.current = page
    if (inFlight.current !== null) return inFlight.current
    const epoch = lifetime.current
    const current = (): boolean => mounted.current && epoch === lifetime.current
    const work = async (): Promise<void> => {
      while (pendingPage.current !== null && current()) {
        const nextPage = pendingPage.current
        pendingPage.current = null
        try {
          const nextSnapshot = await loadCaptureSnapshot()
          let nextRecords = nextPage === 0 ? nextSnapshot.captures : await listCaptures(nextPage, PAGE_SIZE)
          const more = nextRecords.length === PAGE_SIZE && (await listCaptures(nextPage + PAGE_SIZE, 1)).length > 0
          if (!current() || nextPage !== requestedPage.current) continue
          nextRecords = [...nextRecords].sort((left, right) => Date.parse(right.createdAtUtc) - Date.parse(left.createdAtUtc))
          setSnapshot(nextSnapshot)
          if (nextSnapshot.playback !== undefined) projectPlayback(nextSnapshot.playback)
          setRecords(nextRecords)
          setOffset(nextPage)
          setHasOlder(more)
          setError(nextSnapshot.error ?? nextSnapshot.onboardingError ?? null)
          setSetupMessage(nextSnapshot.onboardingState === 'starting' ? 'Watch dictation is starting…'
            : nextSnapshot.onboardingState === 'enabled' ? 'Watch dictation is enabled.' : null)
          setCorrection((previous) => {
            if (previous === null) return null
            const record = nextRecords.find(({ captureId }) => captureId === previous.captureId)
            if (record === undefined) return previous.text !== previous.baselineText || previous.conflicted ? previous : null
            if (previous.text !== previous.baselineText) return { ...previous, record }
            return { captureId: record.captureId, text: transcript(record), baselineText: transcript(record), version: record.transcriptVersion, conflicted: false, record }
          })
        } catch (reason) {
          if (current() && nextPage === requestedPage.current) setError(captureErrorMessage(reason))
        }
      }
    }
    // Start after the promise is registered, including synchronously failing providers.
    const promise = Promise.resolve().then(work).finally(() => {
      if (inFlight.current === promise) inFlight.current = null
    })
    inFlight.current = promise
    return promise
  }, [projectPlayback])

  useEffect(() => {
    mounted.current = true
    lifetime.current += 1
    const listenerEpoch = lifetime.current
    let unlisten: (() => void) | null = null
    void listenCapturePlayback((next) => {
      if (mounted.current && listenerEpoch === lifetime.current) projectPlayback(next)
    }).then((stop) => {
      if (!mounted.current || listenerEpoch !== lifetime.current) stop()
      else { unlisten = stop; void refresh(requestedPage.current) }
    }).catch(() => undefined)
    let timer: number | null = null
    const poll = (): void => {
      if (timer !== null) window.clearTimeout(timer)
      timer = null
      if (document.visibilityState !== 'visible') return
      const epoch = lifetime.current
      void refresh(requestedPage.current).finally(() => {
        if (mounted.current && epoch === lifetime.current && timer === null) timer = window.setTimeout(poll, 5_000)
      })
    }
    poll()
    document.addEventListener('visibilitychange', poll)
    window.addEventListener('focus', poll)
    return () => {
      mounted.current = false
      unlisten?.()
      lifetime.current += 1
      pendingPage.current = null
      inFlight.current = null
      if (timer !== null) window.clearTimeout(timer)
      document.removeEventListener('visibilitychange', poll)
      window.removeEventListener('focus', poll)
    }
  }, [refresh, projectPlayback])

  const pageRecord = records.find(({ captureId }) => captureId === correction?.captureId)
  const selected = pageRecord ?? correction?.record ?? null
  const offPage = correction !== null && pageRecord === undefined
  const conflict = selected !== null && correction !== null && (correction.conflicted || selected.transcriptVersion !== correction.version)
  const selectRecord = (record: CaptureRecord): void => setCorrection({ captureId: record.captureId, text: transcript(record), baselineText: transcript(record), version: record.transcriptVersion, conflicted: false, record })
  const markCorrectionConflict = (captureId: string): void => setCorrection((previous) => previous?.captureId === captureId ? { ...previous, conflicted: true } : previous)
  const setDraft = (text: string): void => setCorrection((previous) => previous === null ? null : { ...previous, text })
  const acceptSavedCorrection = (captureId: string, text: string): void => setCorrection((previous) =>
    previous?.captureId === captureId && previous.text === text ? { ...previous, baselineText: text } : previous)
  return { snapshot, setSnapshot, playback, records, setRecords, offset, hasOlder, selected, selectedId: correction?.captureId ?? null,
    draft: correction?.text ?? '', draftVersion: correction?.version, conflict, offPage, selectRecord, setDraft, acceptSavedCorrection, markCorrectionConflict,
    error, setError, setupMessage, setSetupMessage, mounted, refresh }
}

export function captureErrorMessage(error: unknown): string {
  if (error instanceof Error) return error.message
  if (typeof error === 'string') return error
  if (typeof error === 'object' && error !== null && 'message' in error && typeof error.message === 'string') return error.message
  return 'Capture service request failed'
}

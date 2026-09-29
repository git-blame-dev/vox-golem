import { useEffect, useRef, useState } from 'react'
import type { JSX } from 'react'
import { invokeTauriCommand } from '../lib/tauri'
import { latestWakeScore, parseWakeDiagnosticsSnapshot } from '../lib/wakeDiagnostics'
import type { WakeDiagnosticsSnapshot, WakeMarker } from '../lib/wakeDiagnostics'
import './WakeDiagnostics.css'

let diagnosticsMutationQueue: Promise<void> = Promise.resolve()

interface WakeDiagnosticsProps {
  readonly disabled?: boolean
}

export function WakeDiagnostics({ disabled = false }: WakeDiagnosticsProps): JSX.Element {
  const [snapshot, setSnapshot] = useState<WakeDiagnosticsSnapshot | null>(null)
  const [markedSnapshot, setMarkedSnapshot] = useState<WakeDiagnosticsSnapshot | null>(null)
  const [collecting, setCollecting] = useState(false)
  const [pending, setPending] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const active = useRef(false)
  const backendEnabled = useRef(false)
  const startPending = useRef(false)
  const stopFailed = useRef(false)
  const mounted = useRef(true)
  const generation = useRef(0)
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null)

  useEffect(() => {
    mounted.current = true
    return () => {
      mounted.current = false
      active.current = false
      generation.current += 1
      if (timer.current !== null) clearTimeout(timer.current)
      if (backendEnabled.current || startPending.current) void disableBackend()
    }
  }, [])

  const schedulePoll = (session: number): void => {
    if (!active.current || generation.current !== session) return
    timer.current = setTimeout(() => {
      void poll(session)
    }, 1_000)
  }

  const poll = async (session: number): Promise<void> => {
    try {
      const next = parseWakeDiagnosticsSnapshot(await invokeTauriCommand('get_wake_diagnostics'))
      if (!mounted.current || !active.current || generation.current !== session) return
      setSnapshot(next)
      if (!next.enabled) {
        stopFailed.current = false
        backendEnabled.current = false
        active.current = false
        generation.current += 1
        if (timer.current !== null) clearTimeout(timer.current)
        setCollecting(false)
        setError(null)
      } else if (!stopFailed.current) {
        setError(null)
      }
    } catch (reason) {
      if (!mounted.current || !active.current || generation.current !== session) return
      setError(errorMessage(reason))
    } finally {
      if (mounted.current && active.current && generation.current === session) schedulePoll(session)
    }
  }

  const start = async (): Promise<void> => {
    setPending(true)
    setError(null)
    startPending.current = true
    try {
      const next = parseWakeDiagnosticsSnapshot(await enqueueDiagnosticMutation(
        () => invokeTauriCommand('set_wake_diagnostics', { enabled: true }),
      ))
      if (!mounted.current) return
      if (!next.enabled) throw new Error('Diagnostics did not start')
      if (timer.current !== null) clearTimeout(timer.current)
      generation.current += 1
      stopFailed.current = false
      backendEnabled.current = true
      active.current = true
      setCollecting(true)
      setSnapshot(next)
      setMarkedSnapshot(null)
      schedulePoll(generation.current)
    } catch (reason) {
      if (mounted.current) setError(errorMessage(reason))
    } finally {
      startPending.current = false
      if (mounted.current) setPending(false)
    }
  }

  const stop = async (): Promise<void> => {
    active.current = false
    generation.current += 1
    if (timer.current !== null) clearTimeout(timer.current)
    setPending(true)
    setError(null)
    try {
      const next = parseWakeDiagnosticsSnapshot(await enqueueDiagnosticMutation(
        () => invokeTauriCommand('set_wake_diagnostics', { enabled: false }),
      ))
      if (!mounted.current) return
      setSnapshot(next)
      backendEnabled.current = next.enabled
      if (next.enabled) {
        stopFailed.current = true
        setCollecting(true)
        active.current = true
        setError('Diagnostics are still active; try stopping again.')
        schedulePoll(generation.current)
      } else {
        stopFailed.current = false
        setCollecting(false)
      }
    } catch (reason) {
      if (mounted.current) {
        stopFailed.current = true
        setError(errorMessage(reason))
        if (backendEnabled.current) {
          setCollecting(true)
          active.current = true
          schedulePoll(generation.current)
        }
      }
    } finally {
      if (mounted.current) setPending(false)
    }
  }

  const mark = async (marker: WakeMarker): Promise<void> => {
    setPending(true)
    setError(null)
    try {
      const next = parseWakeDiagnosticsSnapshot(await enqueueDiagnosticMutation(
        () => invokeTauriCommand('mark_wake_diagnostics', { marker }),
      ))
      if (mounted.current) {
        setMarkedSnapshot(next)
        setSnapshot(next)
        if (!next.enabled) {
          backendEnabled.current = false
          active.current = false
          generation.current += 1
          if (timer.current !== null) clearTimeout(timer.current)
          setCollecting(false)
        }
      }
    } catch (reason) {
      if (mounted.current) setError(errorMessage(reason))
    } finally {
      if (mounted.current) setPending(false)
    }
  }

  const exportSnapshot = (value: WakeDiagnosticsSnapshot, fileName: string): void => {
    let url: string | null = null
    let link: HTMLAnchorElement | null = null
    try {
      const blob = new Blob([JSON.stringify(value, null, 2)], { type: 'application/json' })
      url = URL.createObjectURL(blob)
      link = document.createElement('a')
      link.href = url
      link.download = fileName
      document.body.append(link)
      link.click()
    } catch (reason) {
      setError(`Export failed: ${errorMessage(reason)}`)
    } finally {
      link?.remove()
      if (url !== null) {
        const objectUrl = url
        setTimeout(() => {
          try { URL.revokeObjectURL(objectUrl) } catch { /* Best effort cleanup after download. */ }
        }, 0)
      }
    }
  }

  const score = snapshot === null ? null : latestWakeScore(snapshot)
  const displayNumber = (value: unknown, suffix = ''): string => typeof value === 'number' && Number.isFinite(value) ? `${value}${suffix}` : '—'

  return (
    <details className="wake-diagnostics" onToggle={(event) => { if (event.currentTarget.open) setError(null) }}>
      <summary>Wake diagnostics</summary>
      <div className="wake-diagnostics__body">
        <p>This diagnostics panel keeps numeric data only and does not upload audio. Wake clips may separately save local audio on detection; manage them in Settings. Keep this panel open during the test and export the marked snapshot before closing; snapshots are session-local.</p>
        <div className="wake-diagnostics__actions">
          {collecting
            ? <button type="button" disabled={pending} onClick={() => void stop()}>Stop collection</button>
            : <button type="button" disabled={disabled || pending} onClick={() => void start()}>Start collection</button>}
          <button type="button" disabled={disabled || !collecting || pending} onClick={() => void mark('wake_attempt')}>Mark wake attempt</button>
          <button type="button" disabled={disabled || !collecting || pending} onClick={() => void mark('false_wake')}>Mark false wake</button>
          <button type="button" disabled={snapshot === null} onClick={() => snapshot && exportSnapshot(snapshot, 'wake-diagnostics.json')}>Export current snapshot</button>
          <button type="button" disabled={markedSnapshot === null} onClick={() => markedSnapshot && exportSnapshot(markedSnapshot, 'wake-diagnostics-marked.json')}>Export marked snapshot</button>
        </div>
        {error !== null ? <p className="wake-diagnostics__error" role="alert">Wake diagnostics error: {error}</p> : null}
        {snapshot !== null ? (
          <dl className="wake-diagnostics__measurements">
            <div><dt>Model</dt><dd>{snapshot.model}</dd></div>
            <div><dt>Feature profile</dt><dd>{snapshot.feature_profile}</dd></div>
            <div><dt>Threshold</dt><dd>{snapshot.threshold}</dd></div>
            <div><dt>Phase</dt><dd>{snapshot.phase}</dd></div>
            <div><dt>Warm-up remaining</dt><dd>{snapshot.warmup_remaining_samples} samples</dd></div>
            <div><dt>Most recent raw model score</dt><dd>{displayNumber(score?.['score'])}</dd></div>
            <div><dt>Signal RMS</dt><dd>{displayNumber(score?.['rms_dbfs'], ' dBFS')}</dd></div>
            <div><dt>Inference</dt><dd>{displayNumber(score?.['inference_ms'], ' ms')}</dd></div>
          </dl>
        ) : null}
      </div>
    </details>
  )
}

function disableBackend(): Promise<void> {
  return enqueueDiagnosticMutation(() => invokeTauriCommand('set_wake_diagnostics', { enabled: false }))
    .then(() => undefined, () => undefined)
}

function enqueueDiagnosticMutation<T>(mutation: () => Promise<T>): Promise<T> {
  const result = diagnosticsMutationQueue.then(mutation)
  diagnosticsMutationQueue = result.then(() => undefined, () => undefined)
  return result
}

function errorMessage(reason: unknown): string {
  return reason instanceof Error ? reason.message : String(reason)
}

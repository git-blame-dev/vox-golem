import { getTauriInternals, isAndroidRuntime } from './tauri'

export interface CaptureRecord {
  captureId: string
  createdAtUtc: string
  finishedAtUtc: string | null
  durationMs: number
  captureState: string
  recognitionState: string
  transferState: string
  archiveState: string
  orgState: string
  previewText: string | null
  finalText: string | null
  archivedAudioUri: string | null
  error: string | null
  transcriptVersion: number
}

export interface CaptureSnapshot {
  available: boolean
  setupRequired: boolean
  notesFolderUri: string | null
  captures: CaptureRecord[]
  error: string | null
}

function object(value: unknown): Record<string, unknown> | null {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null
}

function nullableString(value: unknown, field: string): string | null {
  if (value === null || typeof value === 'string') return value
  throw new Error(`Capture ${field} is invalid`)
}

export function parseCaptureRecord(value: unknown): CaptureRecord {
  const item = object(value)
  if (item === null || typeof item['captureId'] !== 'string' || item['captureId'].length === 0 ||
    typeof item['createdAtUtc'] !== 'string' || !Number.isFinite(Date.parse(item['createdAtUtc'])) ||
    (item['finishedAtUtc'] !== null && (typeof item['finishedAtUtc'] !== 'string' || !Number.isFinite(Date.parse(item['finishedAtUtc'])))) ||
    typeof item['durationMs'] !== 'number' || !Number.isFinite(item['durationMs']) || item['durationMs'] < 0 ||
    typeof item['transcriptVersion'] !== 'number' || !Number.isSafeInteger(item['transcriptVersion']) || item['transcriptVersion'] < 0) {
    throw new Error('Capture record is invalid')
  }
  for (const field of ['captureState', 'recognitionState', 'transferState', 'archiveState', 'orgState'] as const) {
    if (typeof item[field] !== 'string') throw new Error(`Capture ${field} is invalid`)
  }
  return {
    captureId: item['captureId'] as string, createdAtUtc: item['createdAtUtc'] as string, finishedAtUtc: item['finishedAtUtc'] as string | null,
    durationMs: item['durationMs'] as number, captureState: item['captureState'] as string, recognitionState: item['recognitionState'] as string,
    transferState: item['transferState'] as string, archiveState: item['archiveState'] as string, orgState: item['orgState'] as string,
    previewText: nullableString(item['previewText'], 'previewText'), finalText: nullableString(item['finalText'], 'finalText'),
    archivedAudioUri: nullableString(item['archivedAudioUri'], 'archivedAudioUri'), error: nullableString(item['error'], 'error'),
    transcriptVersion: item['transcriptVersion'],
  }
}

export function parseCaptureSnapshot(value: unknown): CaptureSnapshot {
  const snapshot = object(value)
  if (snapshot === null || typeof snapshot['available'] !== 'boolean' ||
    (snapshot['setupRequired'] !== undefined && typeof snapshot['setupRequired'] !== 'boolean') ||
    !Array.isArray(snapshot['captures']) || snapshot['captures'].length > 100) {
    throw new Error('Capture snapshot is invalid')
  }
  const captures = snapshot['captures'].map(parseCaptureRecord)
  if (typeof snapshot['error'] !== 'string' && snapshot['error'] !== null) throw new Error('Capture snapshot error is invalid')
  return {
    available: snapshot['available'],
    setupRequired: snapshot['setupRequired'] === true,
    notesFolderUri: nullableString(snapshot['notesFolderUri'], 'notesFolderUri'),
    captures,
    error: snapshot['error'],
  }
}

function captureInvoke(command: string, args?: Record<string, unknown>): Promise<unknown> {
  if (!isAndroidRuntime) return Promise.reject(new Error('Voice notes are available only in the Android app'))
  const native = getTauriInternals()
  if (native === null) return Promise.reject(new Error('Native capture service is unavailable'))
  return native.invoke(`plugin:capture|${command}`, args)
}

export async function loadCaptureSnapshot(): Promise<CaptureSnapshot> {
  return parseCaptureSnapshot(await captureInvoke('snapshot'))
}

export async function listCaptures(offset: number, limit = 100): Promise<CaptureRecord[]> {
  if (!Number.isSafeInteger(offset) || offset < 0 || !Number.isSafeInteger(limit) || limit < 1 || limit > 100) {
    throw new Error('Capture page is invalid')
  }
  const result = await captureInvoke('list_captures', { offset, limit })
  if (!Array.isArray(result) || result.length > limit) throw new Error('Capture page is invalid')
  return result.map(parseCaptureRecord)
}

export async function selectNotesFolder(): Promise<CaptureSnapshot> {
  return parseCaptureSnapshot(await captureInvoke('select_notes_folder'))
}

export async function enableCapture(): Promise<CaptureSnapshot> {
  return parseCaptureSnapshot(await captureInvoke('enable_capture'))
}

export async function playCapture(captureId: string): Promise<void> {
  await captureInvoke('play_capture', { captureId })
}

export async function stopCapturePlayback(): Promise<void> {
  await captureInvoke('stop_playback')
}

export async function retryCapture(captureId: string): Promise<void> {
  await captureInvoke('retry_capture', { captureId })
}

export async function finalizeCapture(captureId: string): Promise<void> {
  await captureInvoke('finalize_capture', { captureId })
}

export async function updateCaptureTranscript(captureId: string, text: string): Promise<void> {
  await captureInvoke('update_transcript', { captureId, text })
}

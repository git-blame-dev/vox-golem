import { getTauriInternals, isAndroidRuntime } from './tauri'

// Public enum names from crates/capture-android/.../CaptureContract.kt, not persistence JSON.
const captureStates = ['PREPARING', 'RECORDING', 'STOPPING', 'SAVED', 'INTERRUPTED', 'FAILED'] as const
const capturePurposes = ['NOTE', 'ASK', 'TALK', 'DRAFT'] as const
const recognitionStates = ['UNAVAILABLE', 'QUEUED', 'RUNNING', 'FINAL', 'FAILED', 'NO_SPEECH'] as const
const transferStates = ['LOCAL_ONLY', 'PENDING', 'SENDING', 'PHONE_STAGED', 'PHONE_ARCHIVED'] as const
const archiveStates = ['NOT_STAGED', 'STAGED', 'WAITING_FOR_FOLDER', 'COPYING', 'VERIFIED', 'RETRYABLE_ERROR', 'CONFLICT'] as const
const orgStates = ['WAITING_FOR_TRANSCRIPT', 'WAITING_FOR_FOLDER', 'READY', 'WRITING', 'EXPORTED', 'RETRYABLE_ERROR', 'NEEDS_REVIEW', 'NONE'] as const
const liveTranscriptionStates = ['NOT_REQUESTED', 'CONNECTING', 'PENDING', 'ATTEMPTING', 'PROVIDER_READY', 'TRANSCRIBING', 'UNAVAILABLE', 'COMPLETED'] as const
const onboardingStates = ['setup-required', 'permission-required', 'starting', 'enabled', 'failed', 'unsupported'] as const
export type CaptureOnboardingState = typeof onboardingStates[number]

export interface CaptureRecord {
  captureId: string
  purpose: typeof capturePurposes[number]
  requestedLanguage: string | null
  speechNetworkFallback: boolean
  liveTranscriptionState: typeof liveTranscriptionStates[number]
  liveStatusRevision: number
  canTranscribeLive: boolean
  liveErrorCode: string | null
  createdAtUtc: string
  finishedAtUtc: string | null
  durationMs: number
  captureState: typeof captureStates[number]
  recognitionState: typeof recognitionStates[number]
  transferState: typeof transferStates[number]
  archiveState: typeof archiveStates[number]
  orgState: typeof orgStates[number]
  previewText: string | null
  finalText: string | null
  archivedAudioUri: string | null
  error: string | null
  transcriptVersion: number
}

export interface CaptureSnapshot {
  available: boolean
  setupRequired: boolean
  onboardingState: CaptureOnboardingState
  onboardingError: string | null
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

function state<T extends string>(value: unknown, states: readonly T[], field: string): T {
  if (typeof value !== 'string' || !states.includes(value as T)) throw new Error(`Capture ${field} is invalid`)
  return value as T
}

function legacyBoolean(value: unknown, field: string): boolean {
  if (value === undefined) return false
  if (typeof value !== 'boolean') throw new Error(`Capture ${field} is invalid`)
  return value
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
  const liveStatusRevision = item['liveStatusRevision'] === undefined ? 0 : item['liveStatusRevision']
  if (typeof liveStatusRevision !== 'number' || !Number.isSafeInteger(liveStatusRevision) || liveStatusRevision < 0) {
    throw new Error('Capture liveStatusRevision is invalid')
  }
  return {
    captureId: item['captureId'] as string, createdAtUtc: item['createdAtUtc'] as string, finishedAtUtc: item['finishedAtUtc'] as string | null,
    purpose: item['purpose'] === undefined ? 'NOTE' : state(item['purpose'], capturePurposes, 'purpose'),
    requestedLanguage: item['requestedLanguage'] === undefined ? null : nullableString(item['requestedLanguage'], 'requestedLanguage'),
    speechNetworkFallback: legacyBoolean(item['speechNetworkFallback'], 'speechNetworkFallback'),
    liveTranscriptionState: item['liveTranscriptionState'] === undefined ? 'NOT_REQUESTED'
      : state(item['liveTranscriptionState'], liveTranscriptionStates, 'liveTranscriptionState'),
    liveStatusRevision,
    canTranscribeLive: legacyBoolean(item['canTranscribeLive'], 'canTranscribeLive'),
    liveErrorCode: item['liveErrorCode'] === undefined ? null : nullableString(item['liveErrorCode'], 'liveErrorCode'),
    durationMs: item['durationMs'] as number,
    captureState: state(item['captureState'], captureStates, 'captureState'),
    recognitionState: state(item['recognitionState'], recognitionStates, 'recognitionState'),
    transferState: state(item['transferState'], transferStates, 'transferState'),
    archiveState: state(item['archiveState'], archiveStates, 'archiveState'),
    orgState: state(item['orgState'], orgStates, 'orgState'),
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
  const setupRequired = snapshot['setupRequired'] === true
  const onboardingState = snapshot['onboardingState'] === undefined
    ? setupRequired ? 'setup-required' : 'enabled'
    : state(snapshot['onboardingState'], onboardingStates, 'onboarding state')
  return {
    available: snapshot['available'],
    setupRequired: onboardingState !== 'enabled',
    onboardingState,
    onboardingError: snapshot['onboardingError'] === undefined
      ? snapshot['error'] : nullableString(snapshot['onboardingError'], 'onboarding error'),
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

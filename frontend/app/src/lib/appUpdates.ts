import { invokeTauriCommand } from './tauri'

export type UpdatePhase = 'idle' | 'checking' | 'up_to_date' | 'unavailable' | 'unsupported' | 'available' | 'downloading' | 'ready' | 'installing' | 'restart_required'
export type UpdateOperation = 'check' | 'download' | 'install'
export type UpdateProgressPhase = 'downloading' | 'verifying' | 'installing'

export interface UpdateSnapshot {
  readonly revision: number
  readonly phase: UpdatePhase
  readonly operation: UpdateOperation | null
  readonly currentVersion: string
  readonly version: string | null
  readonly notes: string | null
  readonly progressPhase: UpdateProgressPhase | null
  readonly downloadedBytes: number
  readonly totalBytes: number | null
  readonly error: string | null
  readonly reason: string | null
  readonly autoDownloadEnabled: boolean
}

export async function getUpdateSnapshot(): Promise<UpdateSnapshot> {
  return parseUpdateSnapshot(await invokeTauriCommand('get_update_snapshot'))
}

export async function checkForUpdate(): Promise<UpdateSnapshot> {
  return parseUpdateSnapshot(await invokeTauriCommand('check_for_update'))
}

export async function downloadUpdate(): Promise<UpdateSnapshot> {
  return parseUpdateSnapshot(await invokeTauriCommand('download_update'))
}

export async function discardUpdate(): Promise<UpdateSnapshot> {
  return parseUpdateSnapshot(await invokeTauriCommand('discard_update'))
}

export async function installUpdate(): Promise<UpdateSnapshot> {
  return parseUpdateSnapshot(await invokeTauriCommand('install_update'))
}

export async function restartForUpdate(): Promise<void> {
  await invokeTauriCommand('restart_for_update')
}

export async function setAutoUpdateDownload(enabled: boolean): Promise<UpdateSnapshot> {
  return parseUpdateSnapshot(await invokeTauriCommand('set_auto_update_download', { enabled }))
}

export function selectFreshSnapshot(current: UpdateSnapshot | null, incoming: UpdateSnapshot): UpdateSnapshot {
  return current !== null && current.revision >= incoming.revision ? current : incoming
}

export function retryOperation(snapshot: UpdateSnapshot): UpdateOperation {
  if (snapshot.phase === 'available') return 'download'
  if (snapshot.phase === 'ready') return 'install'
  return 'check'
}

export function parseUpdateSnapshot(payload: unknown): UpdateSnapshot {
  if (!isRecord(payload) || !isSafeCount(payload['revision']) || !isPhase(payload['phase']) ||
    !(payload['operation'] === null || isOperation(payload['operation'])) ||
    typeof payload['current_version'] !== 'string' ||
    !(payload['version'] === null || typeof payload['version'] === 'string') ||
    !(payload['notes'] === null || typeof payload['notes'] === 'string') ||
    !(payload['progress_phase'] === null || isProgressPhase(payload['progress_phase'])) ||
    !isSafeCount(payload['downloaded_bytes']) ||
    !(payload['total_bytes'] === null || isSafeCount(payload['total_bytes'])) ||
    !(payload['error'] === null || typeof payload['error'] === 'string') ||
    !(payload['reason'] === null || typeof payload['reason'] === 'string') ||
    typeof payload['auto_download_enabled'] !== 'boolean') {
    throw new Error('Invalid update snapshot payload')
  }
  if (payload['total_bytes'] !== null && payload['downloaded_bytes'] > payload['total_bytes']) {
    throw new Error('Invalid update snapshot payload')
  }
  return {
    revision: payload['revision'],
    phase: payload['phase'],
    operation: payload['operation'],
    currentVersion: payload['current_version'],
    version: payload['version'],
    notes: payload['notes'],
    progressPhase: payload['progress_phase'],
    downloadedBytes: payload['downloaded_bytes'],
    totalBytes: payload['total_bytes'],
    error: payload['error'],
    reason: payload['reason'],
    autoDownloadEnabled: payload['auto_download_enabled'],
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function isSafeCount(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0
}

function isPhase(value: unknown): value is UpdatePhase {
  return value === 'idle' || value === 'checking' || value === 'up_to_date' || value === 'unavailable' || value === 'unsupported' ||
    value === 'available' || value === 'downloading' || value === 'ready' || value === 'installing' ||
    value === 'restart_required'
}

function isOperation(value: unknown): value is UpdateOperation {
  return value === 'check' || value === 'download' || value === 'install'
}

function isProgressPhase(value: unknown): value is UpdateProgressPhase {
  return value === 'downloading' || value === 'verifying' || value === 'installing'
}

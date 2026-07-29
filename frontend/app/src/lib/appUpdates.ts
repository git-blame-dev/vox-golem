import { invokeTauriCommand } from './tauri'

const MAX_UPDATE_TEXT_LENGTH = 2_048

export type UpdateCheckResult =
  | { readonly status: 'available'; readonly currentVersion: string; readonly version: string; readonly notes: string | null; readonly installBehavior: InstallBehavior }
  | { readonly status: 'up_to_date'; readonly currentVersion: string }
  | { readonly status: 'unavailable'; readonly currentVersion: string; readonly reason: string }
  | { readonly status: 'unsupported'; readonly currentVersion: string; readonly reason: string }
  | { readonly status: 'installing'; readonly currentVersion: string; readonly version: string; readonly installBehavior: InstallBehavior }
  | { readonly status: 'installed'; readonly currentVersion: string; readonly version: string; readonly installBehavior: InstallBehavior }

export type InstallBehavior = 'install_then_restart' | 'install_and_restart'
export type UpdateProgressPhase = 'started' | 'progress' | 'verifying' | 'installing'
export interface UpdateProgress {
  readonly version: string
  readonly phase: UpdateProgressPhase
  readonly downloadedBytes: number
  readonly totalBytes?: number
}

export async function checkForUpdate(): Promise<UpdateCheckResult> {
  return parseUpdateCheckResult(await invokeTauriCommand('check_for_update'))
}

export async function installUpdate(): Promise<string> {
  const payload = await invokeTauriCommand('install_update')
  if (!isRecord(payload) || !isBoundedString(payload['version'])) {
    throw new Error('Invalid update installation payload')
  }
  return payload['version']
}

export async function restartForUpdate(): Promise<void> {
  await invokeTauriCommand('restart_for_update')
}

export function parseUpdateCheckResult(payload: unknown): UpdateCheckResult {
  if (!isRecord(payload) || !isBoundedString(payload['current_version'])) {
    throw new Error('Invalid update check payload')
  }

  if (payload['status'] === 'up_to_date') {
    return { status: 'up_to_date', currentVersion: payload['current_version'] }
  }

  if (
    (payload['status'] === 'installing' || payload['status'] === 'installed') &&
    isBoundedString(payload['version']) && isInstallBehavior(payload['install_behavior'])
  ) {
    return {
      status: payload['status'],
      currentVersion: payload['current_version'],
      version: payload['version'],
      installBehavior: payload['install_behavior'],
    }
  }

  if (
    (payload['status'] === 'unavailable' || payload['status'] === 'unsupported') &&
    isBoundedString(payload['reason'])
  ) {
    return {
      status: payload['status'],
      currentVersion: payload['current_version'],
      reason: payload['reason'],
    }
  }

  if (
    payload['status'] === 'available' &&
    isBoundedString(payload['version']) &&
    isInstallBehavior(payload['install_behavior']) &&
    (payload['notes'] === null || payload['notes'] === undefined || isBoundedString(payload['notes']))
  ) {
    return {
      status: 'available',
      currentVersion: payload['current_version'],
      version: payload['version'],
      notes: typeof payload['notes'] === 'string' ? payload['notes'] : null,
      installBehavior: payload['install_behavior'],
    }
  }

  throw new Error('Invalid update check payload')
}

export function parseUpdateProgress(payload: unknown): UpdateProgress {
  if (!isRecord(payload) || !isBoundedString(payload['version']) ||
    !isProgressPhase(payload['phase']) || !isSafeByteCount(payload['downloaded_bytes'])) {
    throw new Error('Invalid update progress payload')
  }
  const totalBytes = payload['total_bytes']
  if (totalBytes !== undefined && totalBytes !== null && (!isSafeByteCount(totalBytes) || payload['downloaded_bytes'] > totalBytes)) {
    throw new Error('Invalid update progress payload')
  }
  return { version: payload['version'], phase: payload['phase'], downloadedBytes: payload['downloaded_bytes'], ...(totalBytes === undefined || totalBytes === null ? {} : { totalBytes }) }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function isBoundedString(value: unknown): value is string {
  return typeof value === 'string' && value.length > 0 && value.length <= MAX_UPDATE_TEXT_LENGTH
}

function isInstallBehavior(value: unknown): value is InstallBehavior {
  return value === 'install_then_restart' || value === 'install_and_restart'
}

function isProgressPhase(value: unknown): value is UpdateProgressPhase {
  return value === 'started' || value === 'progress' || value === 'verifying' || value === 'installing'
}

function isSafeByteCount(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0
}

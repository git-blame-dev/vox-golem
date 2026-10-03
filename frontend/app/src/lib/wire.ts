import type { BackendRuntimePhase } from '../types/chat'

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

export function parseRuntimePhase(value: unknown, field: string): BackendRuntimePhase {
  if (value === 'initializing' || value === 'sleeping' || value === 'listening' || value === 'processing' || value === 'executing' || value === 'error') return value
  throw new Error(`${field} must be a supported runtime phase`)
}

export function utf8ByteLength(value: string): number {
  return new TextEncoder().encode(value).length
}

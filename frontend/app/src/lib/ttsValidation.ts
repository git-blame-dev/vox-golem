export function firstNonEmptyCompletedLine(value: string): string | null {
  const line = value.split(/\r?\n/u).find((candidate) => candidate.trim().length > 0)?.trim() ?? ''
  if (line.length === 0) return null

  const words = line.split(/\s+/u)
  let excerpt = ''
  for (const word of words) {
    const next = excerpt.length === 0 ? word : `${excerpt} ${word}`
    if (new TextEncoder().encode(next).length > 240) break
    excerpt = next
    if (excerpt.split(/\s+/u).length >= 40) break
  }
  return excerpt.length > 0 ? excerpt : null
}

export function firstNonEmptyStreamingLine(value: string): string | null {
  const line = value.split(/\r?\n/u)
    .slice(0, -1)
    .find((candidate) => candidate.trim().length > 0)?.trim() ?? ''
  if (line.length === 0) return null

  const words = line.split(/\s+/u)
  let excerpt = ''
  for (const word of words) {
    const next = excerpt.length === 0 ? word : `${excerpt} ${word}`
    if (new TextEncoder().encode(next).length > 240) break
    excerpt = next
    if (excerpt.split(/\s+/u).length >= 40) break
  }
  return excerpt.length > 0 ? excerpt : null
}

export function isValidTtsFirstLine(value: string): boolean {
  if (value.trim().length === 0 || value.includes('\n') || value.includes('\r')) return false
  if (new TextEncoder().encode(value).length > 240 || value.trim().split(/\s+/u).length > 40) return false
  if ([...value].some((char) => {
    const code = char.codePointAt(0) ?? 0
    return code < 0x20 || code === 0x7f || code === 0x2028 || code === 0x2029
  })) return false
  return true
}

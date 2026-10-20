export const MAX_TTS_FIRST_PARAGRAPH_BYTES = 600
export const MAX_TTS_FIRST_PARAGRAPH_WORDS = 100

function extractFirstParagraph(value: string): string {
  const parts = value.split(/\r?\n/)
  let paragraph = ''
  for (const part of parts) {
    if (part.trim().length > 0) {
      paragraph = part.trim()
      break
    }
  }
  return paragraph
}

function truncateParagraph(paragraph: string): string {
  const encoder = new TextEncoder()
  const words = paragraph.split(/\s+/)
  let excerpt = ''
  let lastSentenceBoundaryExcerpt = ''
  let truncated = false

  for (let i = 0; i < words.length; i++) {
    const word = words[i]
    if (word === undefined) continue
    const next = excerpt.length === 0 ? word : `${excerpt} ${word}`
    if (encoder.encode(next).length > MAX_TTS_FIRST_PARAGRAPH_BYTES) {
      truncated = true
      break
    }
    if (i >= MAX_TTS_FIRST_PARAGRAPH_WORDS) {
      truncated = true
      break
    }
    excerpt = next

    if (/[.!?;:]$/.test(word)) {
      lastSentenceBoundaryExcerpt = excerpt
    }
  }

  if (truncated && lastSentenceBoundaryExcerpt.length > 0) {
    return lastSentenceBoundaryExcerpt
  }
  return excerpt
}

export function firstNonEmptyCompletedLine(value: string): string | null {
  const paragraph = extractFirstParagraph(value)
  if (paragraph.length === 0) return null
  const truncated = truncateParagraph(paragraph)
  return truncated.length > 0 ? truncated : null
}

export function firstNonEmptyStreamingLine(value: string): string | null {
  const line = value.split(/\r?\n/u)
    .slice(0, -1)
    .find((candidate) => candidate.trim().length > 0)?.trim() ?? ''
  if (line.length === 0) return null
  const truncated = truncateParagraph(line)
  return truncated.length > 0 ? truncated : null
}

export function isValidTtsFirstLine(value: string): boolean {
  if (value.trim().length === 0 || value.includes('\n') || value.includes('\r')) return false
  if (new TextEncoder().encode(value).length > MAX_TTS_FIRST_PARAGRAPH_BYTES || value.trim().split(/\s+/).length > MAX_TTS_FIRST_PARAGRAPH_WORDS) return false
  if ([...value].some((char) => {
    const code = char.codePointAt(0) ?? 0
    return code < 0x20 || code === 0x7f || code === 0x2028 || code === 0x2029
  })) return false
  return true
}

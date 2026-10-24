import { isCoreWhitespaceOnly, isRecord, utf8ByteLength } from './wire'

export interface SourceLink { readonly url: string; readonly title: string }

// Matches the canonical agent-pipeline source limit.
export const MAX_ANSWER_SOURCES = 32

/** Link validation is not permission to fetch the endpoint. */
export function isSourceLink(value: unknown): value is SourceLink {
  if (!isRecord(value) || typeof value['url'] !== 'string' || typeof value['title'] !== 'string' ||
      isCoreWhitespaceOnly(value['title']) || utf8ByteLength(value['title']) > 512 ||
      utf8ByteLength(value['url']) > 2048 || /[\p{White_Space}\p{Cc}]/u.test(value['url'])) return false
  try {
    const url = new URL(value['url'])
    return (url.protocol === 'http:' || url.protocol === 'https:') && !url.username && !url.password
  } catch { return false }
}

export function parseSourceLinks(value: unknown): SourceLink[] {
  if (!Array.isArray(value) || value.length > MAX_ANSWER_SOURCES) throw new Error('Canonical sources must be a bounded array')
  const links = Array.from(value)
  if (!links.every(isSourceLink) || new Set(links.map((link) => link.url)).size !== links.length) {
    throw new Error('Canonical sources must contain unique valid observed links')
  }
  return links.map(({ url, title }) => ({ url, title }))
}

export function sourceBytes(sources: readonly SourceLink[]): number {
  return sources.reduce((bytes, source) => bytes + utf8ByteLength(source.url) + utf8ByteLength(source.title), 0)
}

/** Stable first-occurrence URLs/titles, matching native source projection. */
export function mergeAnswerSources(current: readonly SourceLink[], incoming: readonly SourceLink[]): SourceLink[] {
  const merged: SourceLink[] = []
  const seen = new Set<string>()
  for (const sources of [current, incoming]) {
    for (const source of sources) {
      if (seen.has(source.url)) continue
      if (merged.length === MAX_ANSWER_SOURCES) return merged
      seen.add(source.url)
      merged.push(source)
    }
  }
  return merged
}

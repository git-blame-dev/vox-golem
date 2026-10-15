// @vitest-environment jsdom
import { act } from 'react'
import { createRoot } from 'react-dom/client'
import { describe, expect, it } from 'vitest'
import { readFileSync } from 'node:fs'
import { ChatBubble } from './ChatBubble'

const styles = readFileSync('src/App.css', 'utf8')

describe('shared plain-text transcript', () => {
  it.each(['user', 'assistant'] as const)('retains %s line breaks, indentation and unbroken text without HTML interpretation', (role) => {
    const container = document.createElement('div')
    const stylesheet = document.createElement('style')
    stylesheet.textContent = styles
    document.head.append(stylesheet)
    document.body.append(container)
    const root = createRoot(container)
    const content = `Synthetic paragraph\n\n  indented line\n<script>synthetic plain text</script>\n${'SyntheticLongToken'.repeat(100)}`
    try {
      act(() => root.render(<ChatBubble message={{ id: 'synthetic-message', role, content }} />))
      const paragraph = container.querySelector('p')!
      expect(paragraph.textContent).toBe(content)
      expect(paragraph.querySelector('script')).toBeNull()
      // Exercise the actual shared stylesheet cascade, not a duplicate inline policy.
      const computed = getComputedStyle(paragraph)
      expect(computed.whiteSpace).toBe('pre-wrap')
      expect(computed.overflowWrap).toBe('anywhere')
    } finally {
      act(() => root.unmount())
      container.remove()
      stylesheet.remove()
    }
  })
})

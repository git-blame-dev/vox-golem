// @vitest-environment jsdom
import { act } from 'react'
import { createRoot } from 'react-dom/client'
import { describe, expect, it, vi } from 'vitest'
import { readFileSync } from 'node:fs'
import { ChatBubble, CopyConversationButton, MessageFooter } from './ChatBubble'
import type { FrontendSession } from '../FrontendSession'

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

  it('exports the full conversation to clipboard in User/Assistant format with trailing Sources', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined)
    Object.assign(navigator, { clipboard: { writeText } })
    const messages = [
      { id: '1', role: 'user' as const, content: 'Synthetic question one' },
      {
        id: '2',
        role: 'assistant' as const,
        content: 'Synthetic answer one',
        answerStage: {
          stages: [{ stage: 'instant' as const, status: 'completed' as const }],
          priorVersions: [],
          sources: [
            { title: 'Synthetic Guide', url: 'https://example.test/guide' },
            { title: 'https://example.test/raw', url: 'https://example.test/raw' },
          ],
        },
      },
      { id: '3', role: 'user' as const, content: 'Synthetic follow-up' },
      { id: '4', role: 'assistant' as const, content: 'Synthetic answer two' },
    ]
    const session = {
      subscribe: () => () => undefined,
      getMessagesSnapshot: () => messages,
    } as unknown as FrontendSession
    const container = document.createElement('div')
    document.body.append(container)
    const root = createRoot(container)
    try {
      act(() => root.render(<CopyConversationButton session={session} />))
      const button = container.querySelector<HTMLButtonElement>('button[aria-label="Copy conversation"]')!
      expect(button.disabled).toBe(false)
      await act(async () => { button.click(); await Promise.resolve() })
      expect(writeText).toHaveBeenCalledWith(
        'User:\nSynthetic question one\n\nAssistant:\nSynthetic answer one\n\nSources:\n- Synthetic Guide (https://example.test/guide)\n- https://example.test/raw\n\nUser:\nSynthetic follow-up\n\nAssistant:\nSynthetic answer two',
      )
      expect(button.getAttribute('aria-label')).toBe('Copied conversation')
    } finally {
      act(() => root.unmount())
      container.remove()
    }
  })

  it('appends trailing Sources when copying an individual assistant response in SessionTranscript', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined)
    Object.assign(navigator, { clipboard: { writeText } })
    const { SessionTranscript } = await import('../SessionTranscript')
    const messages = [
      {
        id: 'assistant-sourced',
        role: 'assistant' as const,
        content: 'Grounded synthetic answer.',
        answerStage: {
          stages: [{ stage: 'instant' as const, status: 'completed' as const }],
          priorVersions: [],
          sources: [{ title: 'Synthetic Spec', url: 'https://example.test/spec' }],
        },
      },
    ]
    const session = {
      subscribe: () => () => undefined,
      getMessagesSnapshot: () => messages,
      intents: { onAnswerDomCommitted: () => undefined },
    } as unknown as FrontendSession
    const container = document.createElement('div')
    document.body.append(container)
    const root = createRoot(container)
    try {
      act(() =>
        root.render(
          <SessionTranscript
            session={session}
            staging={false}
            composerValue=""
            textSize="medium"
            noticeId={undefined}
          />,
        ),
      )
      const copyResponse = container.querySelector<HTMLButtonElement>('button[aria-label="Copy response"]')!
      await act(async () => { copyResponse.click(); await Promise.resolve() })
      expect(writeText).toHaveBeenCalledWith(
        'Grounded synthetic answer.\n\nSources:\n- Synthetic Spec (https://example.test/spec)',
      )
    } finally {
      act(() => root.unmount())
      container.remove()
    }
  })

  it('verifies MessageFooter alignment, sizing, speaker control states, and raise measurement', () => {
    const container = document.createElement('div')
    const stylesheet = document.createElement('style')
    stylesheet.textContent = styles
    document.head.append(stylesheet)
    document.body.append(container)
    const root = createRoot(container)
    try {
      act(() => {
        root.render(
          <div>
            <div className="message message--user">
              <p className="message__content">Short</p>
              <MessageFooter role="user" text="Short" canCopy={true} timingLabel={null} />
            </div>
            <div className="message message--assistant">
              <p className="message__content">Short assistant answer</p>
              <MessageFooter role="assistant" text="Short assistant answer" canCopy={true} timingLabel={null} />
            </div>
            <button type="button" className="shell__speaker-control" aria-pressed="false" />
            <button type="button" className="shell__speaker-control" aria-pressed="true" />
          </div>,
        )
      })

      // Check footer alignment classes and copy button margin-inline-start
      const userFooter = container.querySelector('.message__footer--user')!
      expect(getComputedStyle(userFooter).justifyContent).toBe('flex-start')

      const assistantFooter = container.querySelector('.message__footer--assistant')!
      const assistantCopyButton = assistantFooter.querySelector('.message__copy-button')!
      expect(getComputedStyle(assistantCopyButton).marginInlineStart).toBe('auto')

      // Check copy button and svg dimensions (+15%)
      const copyBtn = container.querySelector('.message__copy-button')!
      const copySvg = copyBtn.querySelector('svg')!
      const btnStyle = getComputedStyle(copyBtn)
      expect(btnStyle.width).toBe('25.3px')
      expect(btnStyle.height).toBe('25.3px')
      const svgStyle = getComputedStyle(copySvg)
      expect(svgStyle.width).toBe('20.7px')
      expect(svgStyle.height).toBe('20.7px')

      // Check speaker control pressed states
      const speakerFalse = container.querySelector('.shell__speaker-control[aria-pressed="false"]')!
      const speakerTrue = container.querySelector('.shell__speaker-control[aria-pressed="true"]')!
      expect(getComputedStyle(speakerFalse).color).not.toBe('')
      expect(speakerFalse.getAttribute('aria-pressed')).toBe('false')
      expect(speakerTrue.getAttribute('aria-pressed')).toBe('true')
    } finally {
      act(() => root.unmount())
      container.remove()
      stylesheet.remove()
    }
  })

  it('sets data-raised="true" only when the last line of message text leaves >=32px horizontal clearance on the copy button side', () => {
    const container = document.createElement('div')
    document.body.append(container)
    const root = createRoot(container)
    const paragraphRect = { left: 0, right: 200, top: 0, bottom: 20, width: 200, height: 20, x: 0, y: 0, toJSON: () => ({}) } as DOMRect
    let lastLineRect = { left: 40, right: 160, top: 0, bottom: 20, width: 120, height: 20, x: 40, y: 0, toJSON: () => ({}) } as DOMRect
    const boundSpy = vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function (this: HTMLElement) {
      if (this.classList.contains('message__content')) return paragraphRect
      return { left: 0, right: 0, top: 0, bottom: 0, width: 0, height: 0, x: 0, y: 0, toJSON: () => ({}) } as DOMRect
    })
    const originalGetClientRects = Range.prototype.getClientRects
    Range.prototype.getClientRects = () => {
      const list = [lastLineRect] as unknown as DOMRectList
      Object.defineProperty(list, 'item', { value: (index: number) => list[index] ?? null })
      return list
    }
    try {
      act(() => {
        root.render(
          <div>
            <div className="message message--user">
              <p className="message__content">Right-indented user line</p>
              <MessageFooter role="user" text="Right-indented user line" canCopy={true} timingLabel={null} />
            </div>
            <div className="message message--assistant">
              <p className="message__content">Short assistant line</p>
              <MessageFooter role="assistant" text="Short assistant line" canCopy={true} timingLabel={null} />
            </div>
          </div>,
        )
      })
      expect(container.querySelector('.message__footer--user')?.getAttribute('data-raised')).toBe('true')
      expect(container.querySelector('.message__footer--assistant')?.getAttribute('data-raised')).toBe('true')

      // Now make the last line span nearly the full width (left=10, right=190 -> only 10px clearance on each side)
      lastLineRect = { left: 10, right: 190, top: 0, bottom: 20, width: 180, height: 20, x: 10, y: 0, toJSON: () => ({}) } as DOMRect
      act(() => {
        root.render(
          <div>
            <div className="message message--user">
              <p className="message__content">Full width user line</p>
              <MessageFooter role="user" text="Full width user line" canCopy={true} timingLabel={null} />
            </div>
            <div className="message message--assistant">
              <p className="message__content">Full width assistant line</p>
              <MessageFooter role="assistant" text="Full width assistant line" canCopy={true} timingLabel={null} />
            </div>
          </div>,
        )
      })
      expect(container.querySelector('.message__footer--user')?.getAttribute('data-raised')).toBeNull()
      expect(container.querySelector('.message__footer--assistant')?.getAttribute('data-raised')).toBeNull()
    } finally {
      boundSpy.mockRestore()
      if (originalGetClientRects !== undefined) Range.prototype.getClientRects = originalGetClientRects
      else Reflect.deleteProperty(Range.prototype, 'getClientRects')
      act(() => root.unmount())
      container.remove()
    }
  })
})

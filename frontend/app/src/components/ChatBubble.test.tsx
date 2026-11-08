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
    const conversation = { unsavedResult: null }
    const session = {
      subscribe: () => () => undefined,
      getMessagesSnapshot: () => messages,
      getViewSnapshot: () => conversation,
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
      const answer = copyResponse.closest('.answer-stage')!
      expect(answer).not.toBeNull()
      const footer = answer.querySelector('.message__footer')!
      expect(footer.compareDocumentPosition(answer.querySelector('.answer-stage__details')!) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy()
      await act(async () => { copyResponse.click(); await Promise.resolve() })
      expect(writeText).toHaveBeenCalledWith(
        'Grounded synthetic answer.\n\nSources:\n- Synthetic Spec (https://example.test/spec)',
      )
    } finally {
      act(() => root.unmount())
      container.remove()
    }
  })

  it('verifies MessageFooter alignment, accessible target sizing and speaker control states', () => {
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

      // Both roles share a right-edge action with a smaller footer footprint.
      const userFooter = container.querySelector('.message--user .message__footer')!
      expect(getComputedStyle(userFooter).justifyContent).toBe('flex-end')

      const assistantFooter = container.querySelector('.message--assistant .message__footer')!
      const assistantCopyButton = assistantFooter.querySelector('.message__copy-button')!
      const userCopyButton = userFooter.querySelector('.message__copy-button')!
      for (const property of ['alignItems', 'paddingBottom', 'position', 'insetInlineEnd', 'bottom'] as const) {
        expect(getComputedStyle(userCopyButton)[property]).toBe(getComputedStyle(assistantCopyButton)[property])
      }
      expect(getComputedStyle(assistantCopyButton).position).toBe('absolute')
      expect(getComputedStyle(assistantCopyButton).insetInlineEnd).toBe('0')
      expect(getComputedStyle(assistantFooter).minHeight).toBe('24px')

      // Check copy button and svg dimensions (+15%)
      const copyBtn = container.querySelector('.message__copy-button')!
      const copySvg = copyBtn.querySelector('svg')!
      const btnStyle = getComputedStyle(copyBtn)
      expect(btnStyle.width).toBe('var(--control-target)')
      expect(btnStyle.height).toBe('var(--control-target)')
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

})

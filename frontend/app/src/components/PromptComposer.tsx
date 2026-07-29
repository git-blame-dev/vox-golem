import { forwardRef, useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react'
import type { ChangeEvent, KeyboardEvent, TextareaHTMLAttributes } from 'react'

export interface PromptComposerProps
  extends Omit<TextareaHTMLAttributes<HTMLTextAreaElement>, 'value' | 'onChange' | 'onKeyDown'> {
  readonly value: string
  readonly ghostSuffix?: string
  readonly partialTranscript?: string
  readonly partialCompletionSuffix?: string
  readonly maxRows?: number
  readonly typographyRevision?: string | number
  readonly onChange?: (event: ChangeEvent<HTMLTextAreaElement>) => void
  readonly onKeyDown?: (event: KeyboardEvent<HTMLTextAreaElement>) => void
  readonly onAcceptCompletion?: (suffix: string) => void
  readonly onDismissCompletion?: () => void
}

function cursorIsAtEnd(textarea: HTMLTextAreaElement): boolean {
  return textarea.selectionStart === textarea.value.length && textarea.selectionEnd === textarea.value.length
}

export const PromptComposer = forwardRef<HTMLTextAreaElement, PromptComposerProps>(function PromptComposer({
  value,
  ghostSuffix = '',
  partialTranscript,
  partialCompletionSuffix = '',
  onChange,
  onKeyDown,
  onAcceptCompletion,
  onDismissCompletion,
  maxRows,
  typographyRevision,
  className,
  rows,
  onScroll,
  ...textareaProps
}, forwardedRef) {
  const textareaRef = useRef<HTMLTextAreaElement>(null)
  const [completionVisible, setCompletionVisible] = useState(Boolean(ghostSuffix))

  const updateCompletionVisibility = useCallback(() => {
    const textarea = textareaRef.current
    const visible = Boolean(ghostSuffix) && (!textarea || (textarea.scrollTop <= 0 && cursorIsAtEnd(textarea)))
    setCompletionVisible(visible)
    if (!visible && ghostSuffix) onDismissCompletion?.()
  }, [ghostSuffix, onDismissCompletion])

  useLayoutEffect(() => {
    const resize = () => {
      const textarea = textareaRef.current
      if (!textarea) return

      const styles = window.getComputedStyle(textarea)
      const lineHeight = Number.parseFloat(styles.lineHeight)
      const padding = Number.parseFloat(styles.paddingTop) + Number.parseFloat(styles.paddingBottom)
      const borders = Number.parseFloat(styles.borderTopWidth) + Number.parseFloat(styles.borderBottomWidth)
      const minRows = Number(rows ?? 2)
      const capRows = maxRows === undefined ? Number.MAX_SAFE_INTEGER : Number(maxRows)
      if (![lineHeight, padding, borders, minRows, capRows].every(Number.isFinite) || lineHeight <= 0 || minRows <= 0 || capRows < minRows) return

      textarea.style.height = 'auto'
      const minHeight = minRows * lineHeight + padding + borders
      const maxHeight = capRows * lineHeight + padding + borders
      const desiredHeight = textarea.scrollHeight + borders
      const height = Math.min(Math.max(desiredHeight, minHeight), maxHeight)
      textarea.style.height = `${height}px`
      textarea.style.overflowY = desiredHeight > maxHeight ? 'auto' : 'hidden'
      updateCompletionVisibility()
    }

    resize()
    window.addEventListener('resize', resize)
    return () => window.removeEventListener('resize', resize)
  }, [maxRows, rows, value, typographyRevision, updateCompletionVisibility])

  useEffect(() => {
    updateCompletionVisibility()
  }, [value, ghostSuffix, typographyRevision, updateCompletionVisibility])

  const handleKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === 'Tab' && completionVisible && ghostSuffix && onAcceptCompletion) {
      event.preventDefault()
      event.stopPropagation()
      onAcceptCompletion(ghostSuffix)
    } else if (event.key === 'Escape' && completionVisible && onDismissCompletion) {
      event.preventDefault()
      event.stopPropagation()
      onDismissCompletion()
    }
    onKeyDown?.(event)
  }

  return (
    <div className="prompt-composer">
      <textarea
        {...textareaProps}
        ref={(textarea) => {
          textareaRef.current = textarea
          if (typeof forwardedRef === 'function') forwardedRef(textarea)
          else if (forwardedRef) forwardedRef.current = textarea
        }}
        rows={rows}
        className={className}
        value={value}
        onChange={onChange}
        onKeyDown={handleKeyDown}
        onSelect={updateCompletionVisibility}
        onKeyUp={updateCompletionVisibility}
        onClick={updateCompletionVisibility}
        onScroll={(event) => {
          updateCompletionVisibility()
          onScroll?.(event)
        }}
        aria-describedby="prompt-completion-help"
      />
      {completionVisible && (
        <span className="prompt-composer__ghost" aria-hidden="true">
          <span className="prompt-composer__ghost-prefix">{value}</span>{ghostSuffix}
        </span>
      )}
      {partialTranscript && <span className="prompt-composer__partial" aria-live="polite">{partialTranscript}<span aria-hidden="true">{partialCompletionSuffix}</span></span>}
      <span id="prompt-completion-help" className="sr-only" aria-live="polite">
        {completionVisible ? 'Completion available. Press Tab to accept or Escape to dismiss.' : ''}
      </span>
    </div>
  )
})

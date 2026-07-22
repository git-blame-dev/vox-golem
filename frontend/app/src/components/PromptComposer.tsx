import { useEffect, useRef, useState } from 'react'
import type { ChangeEvent, KeyboardEvent, TextareaHTMLAttributes } from 'react'

export interface PromptComposerProps
  extends Omit<TextareaHTMLAttributes<HTMLTextAreaElement>, 'value' | 'onChange' | 'onKeyDown'> {
  readonly value: string
  readonly ghostSuffix?: string
  readonly partialTranscript?: string
  readonly partialCompletionSuffix?: string
  readonly onChange?: (event: ChangeEvent<HTMLTextAreaElement>) => void
  readonly onKeyDown?: (event: KeyboardEvent<HTMLTextAreaElement>) => void
  readonly onAcceptCompletion?: (suffix: string) => void
  readonly onDismissCompletion?: () => void
}

function cursorIsAtEnd(textarea: HTMLTextAreaElement): boolean {
  return textarea.selectionStart === textarea.value.length && textarea.selectionEnd === textarea.value.length
}

export function PromptComposer({
  value,
  ghostSuffix = '',
  partialTranscript,
  partialCompletionSuffix = '',
  onChange,
  onKeyDown,
  onAcceptCompletion,
  onDismissCompletion,
  className,
  ...textareaProps
}: PromptComposerProps) {
  const textareaRef = useRef<HTMLTextAreaElement>(null)
  const [completionVisible, setCompletionVisible] = useState(Boolean(ghostSuffix))

  const updateCompletionVisibility = () => {
    const textarea = textareaRef.current
    const visible = Boolean(ghostSuffix) && (!textarea || cursorIsAtEnd(textarea))
    setCompletionVisible(visible)
    if (!visible && ghostSuffix) onDismissCompletion?.()
  }

  useEffect(() => {
    const textarea = textareaRef.current
    setCompletionVisible(Boolean(ghostSuffix) && (!textarea || cursorIsAtEnd(textarea)))
  }, [value, ghostSuffix])

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
        ref={textareaRef}
        className={className}
        value={value}
        onChange={onChange}
        onKeyDown={handleKeyDown}
        onSelect={updateCompletionVisibility}
        onKeyUp={updateCompletionVisibility}
        onClick={updateCompletionVisibility}
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
}

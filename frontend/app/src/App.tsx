import { useEffect, useMemo, useState } from 'react'
import type { FormEvent, KeyboardEvent } from 'react'
import { ChatBubble } from './components/ChatBubble'
import { shouldSubmitComposer } from './lib/composer'
import { loadStartupState } from './lib/startupState'
import { createPlaceholderReply, getInitialMessages } from './state/appShell'
import type { ChatMessage, RuntimeStatus, StartupState } from './types/chat'
import './App.css'

function App() {
  const [startupState, setStartupState] = useState<StartupState>({ kind: 'loading' })
  const [runtimeStatus, setRuntimeStatus] = useState<RuntimeStatus>('initializing')
  const [composerValue, setComposerValue] = useState('')
  const [messages, setMessages] = useState<readonly ChatMessage[]>(() =>
    getInitialMessages(),
  )

  useEffect(() => {
    let active = true

    void loadStartupState().then((nextState) => {
      if (!active) {
        return
      }

      setStartupState(nextState)
      setRuntimeStatus(nextState.kind === 'ready' ? 'sleeping' : 'error')
    })

    return () => {
      active = false
    }
  }, [])

  const canSend = useMemo(
    () => startupState.kind === 'ready' && composerValue.trim().length > 0,
    [composerValue, startupState.kind],
  )

  const sendPrompt = (): void => {
    if (startupState.kind !== 'ready') {
      return
    }

    const prompt = composerValue.trim()

    if (prompt.length === 0) {
      return
    }

    const userMessage: ChatMessage = {
      id: `user-${Date.now()}`,
      role: 'user',
      content: prompt,
    }

    const placeholderReply = createPlaceholderReply(prompt)

    setRuntimeStatus('executing')
    setMessages((currentMessages) => [
      ...currentMessages,
      userMessage,
      placeholderReply,
    ])
    setComposerValue('')
    setRuntimeStatus('sleeping')
  }

  const onSubmit = (event: FormEvent<HTMLFormElement>): void => {
    event.preventDefault()
    sendPrompt()
  }

  const onComposerKeyDown = (
    event: KeyboardEvent<HTMLTextAreaElement>,
  ): void => {
    if (!shouldSubmitComposer(event.key, event.shiftKey)) {
      return
    }

    event.preventDefault()
    sendPrompt()
  }

  return (
    <div className="shell">
      <header className="shell__header">
        <p className="shell__eyebrow">VoxGolem</p>
        <h1>Windows Tauri MVP Shell</h1>
        <p className="shell__status">Runtime: {runtimeStatus}</p>
        {startupState.kind === 'error' ? (
          <p className="shell__error">Startup error: {startupState.message}</p>
        ) : null}
      </header>

      <main className="conversation" aria-live="polite">
        {messages.map((message) => (
          <ChatBubble key={message.id} message={message} />
        ))}
      </main>

      <form className="composer" onSubmit={onSubmit}>
        <label className="composer__label" htmlFor="promptComposer">
          Prompt
        </label>
        <textarea
          id="promptComposer"
          className="composer__input"
          value={composerValue}
          onChange={(event) => setComposerValue(event.target.value)}
          onKeyDown={onComposerKeyDown}
          placeholder="Type a prompt..."
          rows={3}
        />
        <div className="composer__actions">
          <span className="composer__hint">Enter to send, Shift+Enter for newline</span>
          <button type="submit" className="composer__button" disabled={!canSend}>
            Send
          </button>
        </div>
      </form>
    </div>
  )
}

export default App

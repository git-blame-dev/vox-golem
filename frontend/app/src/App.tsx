import { useEffect, useMemo, useState } from 'react'
import type { FormEvent, KeyboardEvent } from 'react'
import { ChatBubble } from './components/ChatBubble'
import { playCue } from './lib/audioCues'
import { shouldSubmitComposer } from './lib/composer'
import { executePrompt } from './lib/promptExecution'
import { DEFAULT_CUE_ASSET_PATHS, loadStartupState } from './lib/startupState'
import { createExecutionMessages, getInitialMessages } from './state/appShell'
import { cueForTransition, transitionRuntimeStatus } from './state/runtimeMachine'
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
    () =>
      startupState.kind === 'ready' &&
      (runtimeStatus === 'sleeping' || runtimeStatus === 'result_ready') &&
      composerValue.trim().length > 0,
    [composerValue, runtimeStatus, startupState.kind],
  )

  const canStartListening =
    startupState.kind === 'ready' &&
    (runtimeStatus === 'sleeping' || runtimeStatus === 'result_ready')
  const canMarkSilence =
    startupState.kind === 'ready' && runtimeStatus === 'listening'
  const canMarkResultReady =
    startupState.kind === 'ready' &&
    (runtimeStatus === 'processing' || runtimeStatus === 'executing')
  const canResetToIdle =
    startupState.kind === 'ready' &&
    (runtimeStatus === 'result_ready' || runtimeStatus === 'error')
  const cueAssetPaths =
    startupState.kind === 'ready'
      ? startupState.cueAssetPaths
      : DEFAULT_CUE_ASSET_PATHS

  const applyTransition = (
    previousStatus: RuntimeStatus,
    event: Parameters<typeof transitionRuntimeStatus>[1],
  ): RuntimeStatus => {
    const nextStatus = transitionRuntimeStatus(previousStatus, event)

    if (nextStatus === previousStatus) {
      return previousStatus
    }

    setRuntimeStatus(nextStatus)

    const cueType = cueForTransition(previousStatus, nextStatus)

    if (cueType !== null) {
      void playCue(cueType, cueAssetPaths).catch((error: unknown) => {
        const message = error instanceof Error ? error.message : 'Unknown cue playback error'

        applyTransition(nextStatus, 'fail')
        setMessages((currentMessages) => [
          ...currentMessages,
          {
            id: `system-cue-error-${Date.now()}`,
            role: 'system',
            content: `Cue playback error: ${message}`,
          },
        ])
      })
    }

    return nextStatus
  }

  const transitionFromCurrentStatus = (
    event: Parameters<typeof transitionRuntimeStatus>[1],
  ): void => {
    if (startupState.kind !== 'ready') {
      return
    }

    applyTransition(runtimeStatus, event)
  }

  const sendPrompt = async (): Promise<void> => {
    if (startupState.kind !== 'ready') {
      return
    }

    const prompt = composerValue.trim()

    if (prompt.length === 0) {
      return
    }

    const executingStatus = applyTransition(runtimeStatus, 'submit_prompt')

    if (executingStatus === runtimeStatus) {
      return
    }

    const userMessage: ChatMessage = {
      id: `user-${Date.now()}`,
      role: 'user',
      content: prompt,
    }

    setMessages((currentMessages) => [...currentMessages, userMessage])
    setComposerValue('')

    try {
      const result = await executePrompt(prompt)
      const nextMessages = createExecutionMessages(result)
      const hasStructuredError = result.events.some((event) => event.kind === 'error')

      setMessages((currentMessages) => [...currentMessages, ...nextMessages])

      if (!hasStructuredError && (result.exitCode === null || result.exitCode === 0)) {
        applyTransition(executingStatus, 'response_ready')
      } else {
        applyTransition(executingStatus, 'fail')
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : 'Prompt execution failed'

      applyTransition(executingStatus, 'fail')
      setMessages((currentMessages) => [
        ...currentMessages,
        {
          id: `system-exec-error-${Date.now()}`,
          role: 'system',
          content: `Execution error: ${message}`,
        },
      ])
    }
  }

  const onSubmit = (event: FormEvent<HTMLFormElement>): void => {
    event.preventDefault()
    void sendPrompt()
  }

  const onComposerKeyDown = (
    event: KeyboardEvent<HTMLTextAreaElement>,
  ): void => {
    if (!shouldSubmitComposer(event.key, event.shiftKey)) {
      return
    }

    event.preventDefault()
    void sendPrompt()
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
        <div className="shell__controls" role="group" aria-label="Runtime controls">
          <button
            type="button"
            className="shell__control"
            onClick={() => transitionFromCurrentStatus('begin_listening')}
            disabled={!canStartListening}
          >
            Start listening
          </button>
          <button
            type="button"
            className="shell__control"
            onClick={() => transitionFromCurrentStatus('end_listening')}
            disabled={!canMarkSilence}
          >
            Mark silence
          </button>
          <button
            type="button"
            className="shell__control"
            onClick={() => transitionFromCurrentStatus('response_ready')}
            disabled={!canMarkResultReady}
          >
            Mark result ready
          </button>
          <button
            type="button"
            className="shell__control"
            onClick={() =>
              transitionFromCurrentStatus(
                runtimeStatus === 'error' ? 'recover_from_error' : 'reset_to_sleeping',
              )
            }
            disabled={!canResetToIdle}
          >
            Reset to idle
          </button>
        </div>
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

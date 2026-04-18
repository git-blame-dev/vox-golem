import { useEffect, useMemo, useState } from 'react'
import type { FormEvent, KeyboardEvent } from 'react'
import { ChatBubble } from './components/ChatBubble'
import { playCue } from './lib/audioCues'
import { shouldSubmitComposer } from './lib/composer'
import { executePrompt } from './lib/promptExecution'
import {
  ingestAudioFrame,
  invokeRuntimeControl,
} from './lib/runtimeControl'
import type { RuntimeControlArgs, RuntimeControlResult } from './lib/runtimeControl'
import { DEFAULT_CUE_ASSET_PATHS, loadStartupState } from './lib/startupState'
import { createExecutionMessages, getInitialMessages } from './state/appShell'
import { cueForTransition, transitionRuntimeStatus } from './state/runtimeMachine'
import type {
  BackendRuntimePhase,
  ChatMessage,
  RuntimeStatus,
  StartupState,
} from './types/chat'
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
      setRuntimeStatus(nextState.kind === 'ready' ? toRuntimeStatus(nextState.runtimePhase) : 'error')
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
  const canRecordSpeechActivity = canMarkSilence
  const canMarkResultReady =
    startupState.kind === 'ready' &&
    (runtimeStatus === 'processing' || runtimeStatus === 'executing')
  const canResetToIdle =
    startupState.kind === 'ready' &&
    (runtimeStatus === 'result_ready' || runtimeStatus === 'error')
  const canIngestTestFrame = startupState.kind === 'ready'
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

  const applyRuntimeStatus = (nextStatus: RuntimeStatus): RuntimeStatus => {
    if (nextStatus === runtimeStatus) {
      return runtimeStatus
    }

    setRuntimeStatus(nextStatus)

    const cueType = cueForTransition(runtimeStatus, nextStatus)

    if (cueType !== null) {
      void playCue(cueType, cueAssetPaths).catch((error: unknown) => {
        const message = error instanceof Error ? error.message : 'Unknown cue playback error'

        setRuntimeStatus('error')
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

  const applyRuntimeControlResult = (runtimePhase: RuntimeControlResult): void => {
    applyRuntimeStatus(toRuntimeStatus(runtimePhase.runtimePhase))

    const lastActivityText =
      runtimePhase.lastActivityMs === null ? 'none' : String(runtimePhase.lastActivityMs)
    const nextMessages: ChatMessage[] = [
      {
        id: `system-runtime-control-status-${Date.now()}`,
        role: 'system',
        content: `runtime_control_status:\npreroll=${runtimePhase.prerollSamples} utterance=${runtimePhase.utteranceSamples} capturing=${String(runtimePhase.capturingUtterance)} last_activity=${lastActivityText}`,
      },
    ]

    if (runtimePhase.transcriptionReadySamples !== null) {
      nextMessages.push({
        id: `system-transcription-ready-${Date.now()}`,
        role: 'system',
        content: `transcription_ready:\n${runtimePhase.transcriptionReadySamples} samples captured`,
      })
    }

    setMessages((currentMessages) => [...currentMessages, ...nextMessages])
  }

  const syncRuntimeControl = async (
    command:
      | 'begin_listening'
      | 'record_speech_activity'
      | 'mark_silence'
      | 'mark_result_ready'
      | 'reset_session',
    options: {
      readonly args?: RuntimeControlArgs
      readonly fallbackEvent?: Parameters<typeof transitionRuntimeStatus>[1]
    } = {},
  ): Promise<void> => {
    if (startupState.kind !== 'ready') {
      return
    }

    const { args, fallbackEvent } = options

    try {
      const runtimePhase = await invokeRuntimeControl(command, args)

      if (runtimePhase === null) {
        if (fallbackEvent !== undefined) {
          transitionFromCurrentStatus(fallbackEvent)
        }

        return
      }

      applyRuntimeControlResult(runtimePhase)
    } catch (error) {
      const message = error instanceof Error ? error.message : 'Runtime control failed'

      setRuntimeStatus('error')
      setMessages((currentMessages) => [
        ...currentMessages,
        {
          id: `system-runtime-control-error-${Date.now()}`,
          role: 'system',
          content: `Runtime control error: ${message}`,
        },
      ])
    }
  }

  const ingestTestFrame = async (): Promise<void> => {
    if (startupState.kind !== 'ready') {
      return
    }

    try {
      const status = await ingestAudioFrame([0.1, 0.2, 0.3])

      if (status === null) {
        return
      }

      applyRuntimeStatus(toRuntimeStatus(status.runtimePhase))
      setMessages((currentMessages) => [
        ...currentMessages,
        {
          id: `system-audio-frame-${Date.now()}`,
          role: 'system',
          content: `audio_frame_status:\npreroll=${status.prerollSamples} utterance=${status.utteranceSamples} capturing=${String(status.capturingUtterance)}`,
        },
      ])
    } catch (error) {
      const message = error instanceof Error ? error.message : 'Audio frame ingestion failed'

      setRuntimeStatus('error')
      setMessages((currentMessages) => [
        ...currentMessages,
        {
          id: `system-audio-frame-error-${Date.now()}`,
          role: 'system',
          content: `Audio frame ingestion error: ${message}`,
        },
      ])
    }
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

      setMessages((currentMessages) => [...currentMessages, ...nextMessages])
      applyRuntimeStatus(toRuntimeStatus(result.runtimePhase))
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
            onClick={() => {
              void syncRuntimeControl('begin_listening', { fallbackEvent: 'begin_listening' })
            }}
            disabled={!canStartListening}
          >
            Start listening
          </button>
          <button
            type="button"
            className="shell__control"
            onClick={() => {
              void syncRuntimeControl('record_speech_activity', {
                args: { nowMs: Date.now() },
              })
            }}
            disabled={!canRecordSpeechActivity}
          >
            Record speech activity
          </button>
          <button
            type="button"
            className="shell__control"
            onClick={() => {
              void syncRuntimeControl('mark_silence', { fallbackEvent: 'end_listening' })
            }}
            disabled={!canMarkSilence}
          >
            Mark silence
          </button>
          <button
            type="button"
            className="shell__control"
            onClick={() => {
              void syncRuntimeControl('mark_result_ready', { fallbackEvent: 'response_ready' })
            }}
            disabled={!canMarkResultReady}
          >
            Mark result ready
          </button>
          <button
            type="button"
            className="shell__control"
            onClick={() => {
              void syncRuntimeControl(
                'reset_session',
                {
                  fallbackEvent:
                    runtimeStatus === 'error' ? 'recover_from_error' : 'reset_to_sleeping',
                },
              )
            }}
            disabled={!canResetToIdle}
          >
            Reset to idle
          </button>
          <button
            type="button"
            className="shell__control"
            onClick={() => {
              void ingestTestFrame()
            }}
            disabled={!canIngestTestFrame}
          >
            Ingest test frame
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

function toRuntimeStatus(runtimePhase: BackendRuntimePhase): RuntimeStatus {
  return runtimePhase
}

export default App

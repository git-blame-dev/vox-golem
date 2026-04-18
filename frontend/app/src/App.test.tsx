import { act } from 'react'
import { createRoot } from 'react-dom/client'
import type { Root } from 'react-dom/client'
import { afterEach, describe, expect, it } from 'vitest'
import App from './App'

const mountedContainers: HTMLElement[] = []
const mountedRoots: Root[] = []
const originalAudio = globalThis.Audio
const originalDateNow = Date.now

afterEach(() => {
  for (const root of mountedRoots) {
    act(() => {
      root.unmount()
    })
  }

  for (const container of mountedContainers) {
    container.remove()
  }

  mountedRoots.length = 0
  mountedContainers.length = 0

  if (originalAudio === undefined) {
    Reflect.deleteProperty(globalThis, 'Audio')
  } else {
    Object.defineProperty(globalThis, 'Audio', {
      configurable: true,
      value: originalAudio,
    })
  }

  Reflect.deleteProperty(window, '__TAURI_INTERNALS__')
  Date.now = originalDateNow
})

describe('App', () => {
  it('submits from send button and renders prompt/response history', async () => {
    const { container } = await renderApp()
    const composer = getComposer(container)
    const sendButton = getSendButton(container)

    await act(async () => {
      setTextAreaValue(composer, 'Draft release notes')
    })

    await act(async () => {
      sendButton.click()
    })

    expect(container.textContent).toContain('Draft release notes')
    expect(container.textContent).toContain('Placeholder response for: Draft release notes')
  })

  it('submits from Enter and ignores Shift+Enter', async () => {
    const { container } = await renderApp()
    const composer = getComposer(container)

    await act(async () => {
      setTextAreaValue(composer, 'Line one')
    })

    await act(async () => {
      composer.dispatchEvent(
        new KeyboardEvent('keydown', {
          key: 'Enter',
          shiftKey: true,
          bubbles: true,
        }),
      )
    })

    expect(container.textContent).not.toContain('Placeholder response for: Line one')

    await act(async () => {
      composer.dispatchEvent(
        new KeyboardEvent('keydown', {
          key: 'Enter',
          bubbles: true,
        }),
      )
    })

    expect(container.textContent).toContain('Placeholder response for: Line one')
  })

  it('renders tauri prompt execution output when submit command succeeds', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'assets/start-listening.mp3',
              stop_listening: 'assets/stop-listening.mp3',
            },
            runtime_phase: 'sleeping',
          }
        }

        expect(command).toBe('submit_prompt')
        expect(args).toEqual({ prompt: 'Draft release notes' })

        return {
          events: [
            { kind: 'step_start' },
            { kind: 'reasoning', text: 'Need to inspect the repo state first' },
            { kind: 'tool_use', tool: 'bash', status: 'completed', detail: 'Shows working tree status' },
            { kind: 'text', text: 'OpenCode response' },
            { kind: 'step_finish', reason: 'stop' },
          ],
          stderr: 'warning output',
          exit_code: 0,
          runtime_phase: 'result_ready',
        }
      },
    }

    const { container } = await renderApp()
    const composer = getComposer(container)
    const sendButton = getSendButton(container)

    await act(async () => {
      setTextAreaValue(composer, 'Draft release notes')
    })

    await act(async () => {
      sendButton.click()
      await Promise.resolve()
    })

    expect(container.textContent).toContain('step_start:\nOpenCode started a run step.')
    expect(container.textContent).toContain('reasoning:\nNeed to inspect the repo state first')
    expect(container.textContent).toContain('tool_use:\nbash (completed)\nShows working tree status')
    expect(container.textContent).toContain('OpenCode response')
    expect(container.textContent).toContain('step_finish:\nstop')
    expect(container.textContent).toContain('stderr:\nwarning output')
  })

  it('moves runtime to error when opencode exits non-zero', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'assets/start-listening.mp3',
              stop_listening: 'assets/stop-listening.mp3',
            },
            runtime_phase: 'sleeping',
          }
        }

        return {
          events: [],
          stderr: 'bad prompt',
          exit_code: 7,
          runtime_phase: 'error',
        }
      },
    }

    const { container } = await renderApp()
    const composer = getComposer(container)
    const sendButton = getSendButton(container)

    await act(async () => {
      setTextAreaValue(composer, 'Bad prompt')
    })

    await act(async () => {
      sendButton.click()
      await Promise.resolve()
    })

    expect(container.textContent).toContain('Runtime: error')
    expect(container.textContent).toContain('stderr:\nbad prompt')
    expect(container.textContent).toContain('exit_code:\n7')
  })

  it('moves runtime to error when opencode emits a structured error event', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'assets/start-listening.mp3',
              stop_listening: 'assets/stop-listening.mp3',
            },
            runtime_phase: 'sleeping',
          }
        }

        return {
          events: [
            {
              kind: 'error',
              name: 'APIError',
              message: 'Provider failed',
            },
          ],
          stderr: '',
          exit_code: 0,
          runtime_phase: 'error',
        }
      },
    }

    const { container } = await renderApp()
    const composer = getComposer(container)
    const sendButton = getSendButton(container)

    await act(async () => {
      setTextAreaValue(composer, 'Bad prompt')
    })

    await act(async () => {
      sendButton.click()
      await Promise.resolve()
    })

    expect(container.textContent).toContain('Runtime: error')
    expect(container.textContent).toContain('opencode_error:\nAPIError: Provider failed')
  })

  it('plays the configured start-listening cue path from startup state', async () => {
    const playedSources: string[] = []
    let recordedSpeechActivityMs: number | null = null

    class FakeAudio {
      private readonly source: string

      constructor(source: string) {
        this.source = source
      }

      play(): Promise<void> {
        playedSources.push(this.source)
        return Promise.resolve()
      }
    }

    Object.defineProperty(globalThis, 'Audio', {
      configurable: true,
      value: FakeAudio,
    })
    let nowMs = 100
    Date.now = () => {
      nowMs += 1
      return nowMs
    }

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'test-assets/configured-start.mp3',
              stop_listening: 'test-assets/configured-stop.mp3',
            },
            runtime_phase: 'sleeping',
          }
        }

        if (command === 'begin_listening') {
          return {
            runtime_phase: 'listening',
            transcription_ready_samples: null,
            last_activity_ms: 100,
            capturing_utterance: true,
            preroll_samples: 4,
            utterance_samples: 4,
          }
        }

        if (command === 'record_speech_activity') {
          if (
            typeof args !== 'object' ||
            args === null ||
            typeof (args as { nowMs?: unknown }).nowMs !== 'number'
          ) {
            throw new Error('record_speech_activity must include a numeric nowMs')
          }

          recordedSpeechActivityMs = (args as { nowMs: number }).nowMs

          return {
            runtime_phase: 'listening',
            transcription_ready_samples: null,
            last_activity_ms: recordedSpeechActivityMs,
            capturing_utterance: true,
            preroll_samples: 4,
            utterance_samples: 4,
          }
        }

        if (command === 'mark_silence') {
          return {
            runtime_phase: 'processing',
            transcription_ready_samples: 3200,
            last_activity_ms: null,
            capturing_utterance: false,
            preroll_samples: 4,
            utterance_samples: 0,
          }
        }

        return {
          runtime_phase: 'processing',
          last_activity_ms: null,
          capturing_utterance: false,
          preroll_samples: 3,
          utterance_samples: 0,
        }
      },
    }

    const { container } = await renderApp()
    const startListeningButton = getControlButton(container, 'Start listening')

    await act(async () => {
      startListeningButton.click()
      await Promise.resolve()
    })

    expect(playedSources).toEqual(['test-assets/configured-start.mp3'])
    expect(container.textContent).toContain('Runtime: listening')
    expect(container.textContent).toContain(
      'runtime_control_status:\npreroll=4 utterance=4 capturing=true last_activity=100',
    )

    const recordSpeechActivityButton = getControlButton(container, 'Record speech activity')

    await act(async () => {
      recordSpeechActivityButton.click()
      await Promise.resolve()
    })

    expect(container.textContent).toContain('Runtime: listening')
    expect(recordedSpeechActivityMs).not.toBeNull()
    expect(container.textContent).toContain(
      `runtime_control_status:\npreroll=4 utterance=4 capturing=true last_activity=${String(recordedSpeechActivityMs)}`,
    )

    const markSilenceButton = getControlButton(container, 'Mark silence')

    await act(async () => {
      markSilenceButton.click()
      await Promise.resolve()
    })

    expect(container.textContent).toContain('Runtime: processing')
    expect(container.textContent).toContain(
      'runtime_control_status:\npreroll=4 utterance=0 capturing=false last_activity=none',
    )
    expect(container.textContent).not.toContain(
      'runtime_control_status:\npreroll=undefined utterance=undefined capturing=undefined last_activity=undefined',
    )
    expect(container.textContent).toContain('transcription_ready:\n3200 samples captured')

    const ingestTestFrameButton = getControlButton(container, 'Ingest test frame')

    await act(async () => {
      ingestTestFrameButton.click()
      await Promise.resolve()
    })

    expect(container.textContent).toContain(
      'audio_frame_status:\npreroll=3 utterance=0 capturing=false',
    )
  })
})

async function renderApp(): Promise<{ container: HTMLElement }> {
  const container = document.createElement('div')
  document.body.append(container)
  mountedContainers.push(container)
  const root = createRoot(container)
  mountedRoots.push(root)

  await act(async () => {
    root.render(<App />)
  })

  await act(async () => {
    await Promise.resolve()
  })

  return { container }
}

function getComposer(container: HTMLElement): HTMLTextAreaElement {
  const composer = container.querySelector<HTMLTextAreaElement>('#promptComposer')

  if (composer === null) {
    throw new Error('Missing composer textarea')
  }

  return composer
}

function getSendButton(container: HTMLElement): HTMLButtonElement {
  const button = container.querySelector<HTMLButtonElement>('button[type="submit"]')

  if (button === null) {
    throw new Error('Missing send button')
  }

  return button
}

function getControlButton(
  container: HTMLElement,
  label:
    | 'Start listening'
    | 'Record speech activity'
    | 'Mark silence'
    | 'Mark result ready'
    | 'Reset to idle'
    | 'Ingest test frame',
): HTMLButtonElement {
  const buttons = Array.from(container.querySelectorAll<HTMLButtonElement>('button'))
  const button = buttons.find((candidate) => candidate.textContent?.trim() === label)

  if (button === undefined) {
    throw new Error(`Missing ${label} control button`)
  }

  return button
}

function setTextAreaValue(textArea: HTMLTextAreaElement, value: string): void {
  const descriptor = Object.getOwnPropertyDescriptor(
    HTMLTextAreaElement.prototype,
    'value',
  )

  if (descriptor?.set === undefined) {
    throw new Error('Textarea value setter is unavailable')
  }

  descriptor.set.call(textArea, value)
  textArea.dispatchEvent(new Event('input', { bubbles: true }))
}

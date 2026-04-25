import { act } from 'react'
import { createRoot } from 'react-dom/client'
import type { Root } from 'react-dom/client'
import { afterEach, describe, expect, it, vi } from 'vitest'
import * as liveAudioSourceModule from './lib/liveAudioSource'
import App from './App'

const startLiveAudioSourceMock = vi.spyOn(liveAudioSourceModule, 'startLiveAudioSource')

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
  startLiveAudioSourceMock.mockReset()
})

describe('App', () => {
  it('auto-follows the latest message when conversation grows', async () => {
    const { container } = await renderApp()
    const composer = getComposer(container)
    const sendButton = getSendButton(container)
    const conversation = getConversation(container)
    const scrollToSpy = vi.fn()

    Object.defineProperty(conversation, 'scrollTo', {
      configurable: true,
      value: scrollToSpy,
    })

    const baselineCalls = scrollToSpy.mock.calls.length

    await act(async () => {
      setTextAreaValue(composer, 'Scroll check prompt')
    })

    await act(async () => {
      sendButton.click()
      await Promise.resolve()
    })

    expect(scrollToSpy.mock.calls.length).toBeGreaterThan(baselineCalls)
  })

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

  it('renders response profile dropdown from startup state', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command !== 'get_startup_state') {
          throw new Error(`unexpected command: ${command}`)
        }

        return {
          kind: 'ready',
          cue_asset_paths: {
            start_listening: 'resources/start-listening.wav',
            stop_listening: 'resources/stop-listening.wav',
          },
          runtime_phase: 'sleeping',
          voice_input_available: true,
          voice_input_error: null,
          silence_timeout_ms: 1500,
          selected_response_profile: 'fast',
          supported_response_profiles: ['fast', 'quality'],
        }
      },
    }

    const { container } = await renderApp()
    const select = getResponseProfileSelect(container)

    expect(select.value).toBe('fast')
    expect(Array.from(select.options).map((option) => option.text)).toEqual(['Fast', 'Quality'])
  })

  it('invokes switch_response_profile when selecting Quality profile', async () => {
    const invokedCommands: string[] = []
    let selectedProfile: 'fast' | 'quality' = 'fast'

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        invokedCommands.push(command)

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: selectedProfile,
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        if (command === 'switch_response_profile') {
          expect(args).toEqual({ profile: 'quality' })
          selectedProfile = 'quality'
          return {
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = getResponseProfileSelect(container)

    await act(async () => {
      setSelectValue(select, 'quality')
      await Promise.resolve()
    })

    expect(invokedCommands).toContain('switch_response_profile')
    expect(select.value).toBe('quality')
  })

  it('shows model loading indicator while profile switching is in progress', async () => {
    let selectedProfile: 'fast' | 'quality' = 'fast'
    let switchCommandPending = false
    let resolveSwitchCommand: () => void = () => {
      throw new Error('Expected pending switch command resolver')
    }

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: selectedProfile,
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        if (command === 'switch_response_profile') {
          expect(args).toEqual({ profile: 'quality' })

          return await new Promise((resolve) => {
            switchCommandPending = true
            resolveSwitchCommand = () => {
              switchCommandPending = false
              selectedProfile = 'quality'
              resolve({
                selected_response_profile: 'quality',
                supported_response_profiles: ['fast', 'quality'],
              })
            }
          })
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = getResponseProfileSelect(container)

    await act(async () => {
      setSelectValue(select, 'quality')
      await Promise.resolve()
    })

    expect(container.textContent).toContain('Model loading: Switching response profile to Quality...')
    expect(switchCommandPending).toBe(true)

    await act(async () => {
      resolveSwitchCommand()
      await Promise.resolve()
    })

    expect(select.value).toBe('quality')
  })

  it('restores previous profile and surfaces an error when profile switching fails', async () => {
    const invokedCommands: string[] = []
    let startupStateCallCount = 0

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        invokedCommands.push(command)

        if (command === 'get_startup_state') {
          startupStateCallCount += 1

          if (startupStateCallCount === 1) {
            return {
              kind: 'ready',
              cue_asset_paths: {
                start_listening: 'resources/start-listening.wav',
                stop_listening: 'resources/stop-listening.wav',
              },
              runtime_phase: 'sleeping',
              voice_input_available: true,
              voice_input_error: null,
              silence_timeout_ms: 1500,
              selected_response_profile: 'fast',
              supported_response_profiles: ['fast', 'quality'],
            }
          }

          if (startupStateCallCount === 2) {
            return {
              kind: 'warming_model',
              cue_asset_paths: {
                start_listening: 'resources/start-listening.wav',
                stop_listening: 'resources/stop-listening.wav',
              },
              runtime_phase: 'initializing',
              voice_input_available: true,
              voice_input_error: null,
              silence_timeout_ms: 1500,
              message: 'Loading local Gemma model...',
              selected_response_profile: 'fast',
              supported_response_profiles: ['fast', 'quality'],
            }
          }

          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'executing',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        if (command === 'switch_response_profile') {
          throw new Error('response backend is busy; wait for the active operation to finish')
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = getResponseProfileSelect(container)

    await act(async () => {
      setSelectValue(select, 'quality')
      await Promise.resolve()
      await new Promise((resolve) => window.setTimeout(resolve, 600))
    })

    expect(startupStateCallCount).toBe(3)
    expect(invokedCommands).toEqual([
      'get_startup_state',
      'switch_response_profile',
      'get_startup_state',
      'get_startup_state',
    ])
    expect(select.value).toBe('fast')
    expect(container.textContent).toContain(
      'Response profile switch error: response backend is busy; wait for the active operation to finish',
    )
  })

  it('surfaces startup error when profile switch polling settles to error', async () => {
    let startupStateCallCount = 0

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        if (command === 'get_startup_state') {
          startupStateCallCount += 1

          if (startupStateCallCount === 1) {
            return {
              kind: 'ready',
              cue_asset_paths: {
                start_listening: 'resources/start-listening.wav',
                stop_listening: 'resources/stop-listening.wav',
              },
              runtime_phase: 'sleeping',
              voice_input_available: true,
              voice_input_error: null,
              silence_timeout_ms: 1500,
              selected_response_profile: 'fast',
              supported_response_profiles: ['fast', 'quality'],
            }
          }

          return {
            kind: 'error',
            message: 'failed to initialize local llama.cpp runtime: boom',
          }
        }

        if (command === 'switch_response_profile') {
          expect(args).toEqual({ profile: 'quality' })
          return {
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = getResponseProfileSelect(container)

    await act(async () => {
      setSelectValue(select, 'quality')
      await Promise.resolve()
    })

    expect(startupStateCallCount).toBe(2)
    expect(container.textContent).toContain('Startup error: failed to initialize local llama.cpp runtime: boom')
    expect(container.textContent).not.toContain('Response profile switch error:')
  })

  it('renders tauri prompt execution output when submit command succeeds', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
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
          runtime_phase: 'sleeping',
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
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
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
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
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

    expect(container.textContent).toContain('opencode_error:\nAPIError: Provider failed')
  })

  it('polls startup state while the local model is warming and becomes ready later', async () => {
    let startupCalls = 0

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command !== 'get_startup_state') {
          throw new Error(`unexpected command: ${command}`)
        }

        startupCalls += 1
        if (startupCalls === 1) {
          return {
            kind: 'warming_model',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'initializing',
            voice_input_available: true,
            voice_input_error: null,
            message: 'Loading local Gemma model...',
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        return {
          kind: 'ready',
          cue_asset_paths: {
            start_listening: 'resources/start-listening.wav',
            stop_listening: 'resources/stop-listening.wav',
          },
           runtime_phase: 'sleeping',
           voice_input_available: true,
           voice_input_error: null,
           silence_timeout_ms: 1500,
           selected_response_profile: 'quality',
           supported_response_profiles: ['fast', 'quality'],
         }
      },
    }

    const { container } = await renderApp()

    expect(container.textContent).toContain('Voice limited')

    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 600))
    })

    expect(container.textContent).toContain('Startup ready: runtime=sleeping')
  })

  it('surfaces an error if the model warming state later fails', async () => {
    let startupCalls = 0

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command !== 'get_startup_state') {
          throw new Error(`unexpected command: ${command}`)
        }

        startupCalls += 1
        if (startupCalls === 1) {
          return {
            kind: 'warming_model',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'initializing',
            voice_input_available: true,
            voice_input_error: null,
            message: 'Loading local Gemma model...',
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        return {
          kind: 'error',
          message: 'failed to initialize local llama.cpp runtime: boom',
        }
      },
    }

    const { container } = await renderApp()

    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 600))
    })

    expect(container.textContent).toContain('Startup error: failed to initialize local llama.cpp runtime: boom')
  })

  it('plays the configured start-listening cue path from startup state', async () => {
    const playedSources: string[] = []

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
      invoke: async (command) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'test-assets/configured-start.mp3',
              stop_listening: 'test-assets/configured-stop.mp3',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        if (command === 'begin_listening') {
          return {
            runtime_phase: 'listening',
            transcription_ready_samples: null,
            transcript_text: null,
            last_activity_ms: 100,
            capturing_utterance: true,
            preroll_samples: 4,
            utterance_samples: 4,
          }
        }

        if (command === 'mark_silence') {
          return {
            runtime_phase: 'processing',
            transcription_ready_samples: 3200,
            transcript_text: null,
            last_activity_ms: null,
            capturing_utterance: false,
            preroll_samples: 4,
            utterance_samples: 0,
          }
        }

        return {
          runtime_phase: 'processing',
          transcription_ready_samples: null,
          transcript_text: null,
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
    const stopListeningButton = getControlButton(container, 'Stop listening and process')

    await act(async () => {
      stopListeningButton.click()
      await Promise.resolve()
    })

    expect(playedSources).toEqual([
      'test-assets/configured-start.mp3',
      'test-assets/configured-stop.mp3',
    ])
    expect(container.textContent).toContain('transcription_ready:\n3200 samples captured')
  })

  it('shows wake confidence badge while listening when telemetry includes wake confidence', async () => {
    class FakeAudio {
      play(): Promise<void> {
        return Promise.resolve()
      }
    }

    Object.defineProperty(globalThis, 'Audio', {
      configurable: true,
      value: FakeAudio,
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        if (command === 'begin_listening') {
          return {
            runtime_phase: 'listening',
            transcription_ready_samples: null,
            transcript_text: null,
            last_activity_ms: 100,
            capturing_utterance: true,
            preroll_samples: 0,
            utterance_samples: 3,
            telemetry: {
              wake_detected_ms: 100,
              wake_confidence: 0.67,
            },
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()

    await act(async () => {
      getControlButton(container, 'Start listening').click()
      await Promise.resolve()
    })

    expect(container.textContent).toContain('Wake trigger score 0.670')
  })

  it('starts and stops default microphone capture and forwards live frames', async () => {
    const stop = vi.fn()
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        expect(command).toBe('ingest_audio_frame')
        expect(args).toEqual({ frame: [0.1, 0.2, 0.3] })

        return {
          runtime_phase: 'sleeping',
          transcription_ready_samples: null,
          transcript_text: null,
          last_activity_ms: null,
          capturing_utterance: false,
          preroll_samples: 3,
          utterance_samples: 0,
        }
      },
    }

    const { container } = await renderApp()

    expect(startLiveAudioSourceMock).toHaveBeenCalledTimes(1)
    expect(container.textContent).toContain('live_audio:\ndefault microphone started')

    await act(async () => {
      await onFrame?.([0.1, 0.2, 0.3])
      await Promise.resolve()
    })

    const stopMicButton = getControlButton(container, 'Stop mic')

    await act(async () => {
      stopMicButton.click()
      await Promise.resolve()
    })

    expect(stop).toHaveBeenCalledTimes(1)
    expect(container.textContent).toContain('live_audio:\ndefault microphone stopped')
  })

  it('ignores stale live frames after profile switch stops capture', async () => {
    const stop = vi.fn()
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let selectedProfile: 'fast' | 'quality' = 'fast'

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        invokedCommands.push(command)

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: selectedProfile,
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        if (command === 'switch_response_profile') {
          expect(args).toEqual({ profile: 'quality' })
          selectedProfile = 'quality'
          return {
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        if (command === 'ingest_audio_frame') {
          return {
            runtime_phase: 'sleeping',
            transcription_ready_samples: null,
            transcript_text: null,
            last_activity_ms: null,
            capturing_utterance: false,
            preroll_samples: 0,
            utterance_samples: 0,
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = getResponseProfileSelect(container)

    await act(async () => {
      setSelectValue(select, 'quality')
      await Promise.resolve()
    })

    expect(stop).toHaveBeenCalledTimes(1)

    await act(async () => {
      await onFrame?.([0.2, -0.2, 0.2, -0.2])
      await Promise.resolve()
    })

    expect(invokedCommands).toContain('switch_response_profile')
    expect(invokedCommands.filter((command) => command === 'ingest_audio_frame')).toHaveLength(0)
  })

  it('ignores in-flight frame results that resolve after profile switch', async () => {
    const stop = vi.fn()
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let selectedProfile: 'fast' | 'quality' = 'fast'
    let ingestFramePending = false
    let resolveIngestFrame: () => void = () => {
      throw new Error('Expected an in-flight ingest frame promise')
    }

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        invokedCommands.push(command)

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: selectedProfile,
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        if (command === 'ingest_audio_frame') {
          expect(args).toBeDefined()
          ingestFramePending = true

          return await new Promise((resolve) => {
            resolveIngestFrame = () => {
              ingestFramePending = false
              resolve({
                runtime_phase: 'error',
                transcription_ready_samples: null,
                transcript_text: null,
                last_activity_ms: null,
                capturing_utterance: false,
                preroll_samples: 0,
                utterance_samples: 0,
              })
            }
          })
        }

        if (command === 'switch_response_profile') {
          expect(args).toEqual({ profile: 'quality' })
          selectedProfile = 'quality'
          return {
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = getResponseProfileSelect(container)

    expect(onFrame).not.toBeNull()
    const frameHandler = onFrame as unknown as (frame: readonly number[]) => Promise<void> | void

    let pendingFrame: Promise<void> | void
    await act(async () => {
      pendingFrame = frameHandler([0.2, -0.2, 0.2, -0.2])
      await Promise.resolve()
    })

    await act(async () => {
      setSelectValue(select, 'quality')
      await Promise.resolve()
    })

    expect(stop).toHaveBeenCalledTimes(1)
    expect(ingestFramePending).toBe(true)

    resolveIngestFrame()

    await act(async () => {
      await pendingFrame
      await new Promise((resolve) => setTimeout(resolve, 50))
    })

    expect(invokedCommands).toContain('switch_response_profile')
    expect(invokedCommands.filter((command) => command === 'ingest_audio_frame')).toHaveLength(1)
    expect(getControlButton(container, 'Reset to idle').disabled).toBe(true)
    expect(container.textContent).not.toContain('Runtime control error')
  })

  it('waits for in-flight ingest to drain before invoking profile switch command', async () => {
    const stop = vi.fn()
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let selectedProfile: 'fast' | 'quality' = 'fast'
    let ingestPending = false
    let resolveIngestFrame: () => void = () => {
      throw new Error('Expected delayed ingest resolver')
    }

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        invokedCommands.push(command)

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: selectedProfile,
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        if (command === 'ingest_audio_frame') {
          ingestPending = true

          return await new Promise((resolve) => {
            resolveIngestFrame = () => {
              ingestPending = false
              resolve({
                runtime_phase: 'sleeping',
                transcription_ready_samples: null,
                transcript_text: null,
                last_activity_ms: null,
                capturing_utterance: false,
                preroll_samples: 0,
                utterance_samples: 0,
              })
            }
          })
        }

        if (command === 'switch_response_profile') {
          expect(ingestPending).toBe(false)
          expect(args).toEqual({ profile: 'quality' })
          selectedProfile = 'quality'
          return {
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = getResponseProfileSelect(container)

    expect(onFrame).not.toBeNull()
    const frameHandler = onFrame as unknown as (frame: readonly number[]) => Promise<void> | void

    let pendingFrame: Promise<void> | void
    await act(async () => {
      pendingFrame = frameHandler([0.2, -0.2, 0.2, -0.2])
      await Promise.resolve()
    })

    expect(ingestPending).toBe(true)

    await act(async () => {
      setSelectValue(select, 'quality')
      await Promise.resolve()
    })

    expect(invokedCommands).not.toContain('switch_response_profile')
    expect(stop).toHaveBeenCalledTimes(1)

    resolveIngestFrame()

    await act(async () => {
      await pendingFrame
      await new Promise((resolve) => setTimeout(resolve, 50))
    })

    expect(invokedCommands).toContain('switch_response_profile')
    expect(container.textContent).not.toContain('Response profile switch error: response backend is busy')
  })

  it('ignores re-entrant profile switch attempts while draining in-flight ingest', async () => {
    const stop = vi.fn()
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let selectedProfile: 'fast' | 'quality' = 'fast'
    let resolveIngestFrame: () => void = () => {
      throw new Error('Expected delayed ingest resolver')
    }

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        invokedCommands.push(command)

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: selectedProfile,
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        if (command === 'ingest_audio_frame') {
          return await new Promise((resolve) => {
            resolveIngestFrame = () => {
              resolve({
                runtime_phase: 'sleeping',
                transcription_ready_samples: null,
                transcript_text: null,
                last_activity_ms: null,
                capturing_utterance: false,
                preroll_samples: 0,
                utterance_samples: 0,
              })
            }
          })
        }

        if (command === 'switch_response_profile') {
          expect(args).toEqual({ profile: 'quality' })
          selectedProfile = 'quality'
          return {
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = getResponseProfileSelect(container)

    expect(onFrame).not.toBeNull()
    const frameHandler = onFrame as unknown as (frame: readonly number[]) => Promise<void> | void

    let pendingFrame: Promise<void> | void
    await act(async () => {
      pendingFrame = frameHandler([0.2, -0.2, 0.2, -0.2])
      await Promise.resolve()
    })

    await act(async () => {
      setSelectValue(select, 'quality')
      setSelectValue(select, 'quality')
      await Promise.resolve()
    })

    expect(invokedCommands).not.toContain('switch_response_profile')

    resolveIngestFrame()

    await act(async () => {
      await pendingFrame
      await new Promise((resolve) => setTimeout(resolve, 50))
    })

    expect(invokedCommands.filter((command) => command === 'switch_response_profile')).toHaveLength(1)
  })

  it('ignores stale delayed microphone start after profile switch invalidates session', async () => {
    const stop = vi.fn()
    let delayedStartPending = false
    let resolveLiveAudioSource: (source: { stop: () => void }) => void = () => {
      throw new Error('Expected delayed microphone start resolver')
    }
    const invokedCommands: string[] = []
    let selectedProfile: 'fast' | 'quality' = 'fast'

    startLiveAudioSourceMock.mockImplementation(
      async () =>
        await new Promise((resolve) => {
          delayedStartPending = true
          resolveLiveAudioSource = resolve
        }),
    )

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        invokedCommands.push(command)

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: selectedProfile,
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        if (command === 'switch_response_profile') {
          expect(args).toEqual({ profile: 'quality' })
          selectedProfile = 'quality'
          return {
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = getResponseProfileSelect(container)

    await act(async () => {
      setSelectValue(select, 'quality')
      await Promise.resolve()
    })

    expect(delayedStartPending).toBe(true)

    await act(async () => {
      delayedStartPending = false
      resolveLiveAudioSource({ stop })
      await Promise.resolve()
    })

    expect(invokedCommands).toContain('switch_response_profile')
    expect(invokedCommands.filter((command) => command === 'ingest_audio_frame')).toHaveLength(0)
    expect(stop).toHaveBeenCalledTimes(1)
    expect(container.textContent).not.toContain('live_audio:\ndefault microphone started')
  })

  it('ignores stale delayed microphone start rejection after profile switch', async () => {
    let delayedStartPending = false
    let rejectLiveAudioSource: (error: unknown) => void = () => {
      throw new Error('Expected delayed microphone start rejector')
    }
    const invokedCommands: string[] = []
    let selectedProfile: 'fast' | 'quality' = 'fast'

    startLiveAudioSourceMock.mockImplementation(
      async () =>
        await new Promise((_resolve, reject) => {
          delayedStartPending = true
          rejectLiveAudioSource = reject
        }),
    )

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        invokedCommands.push(command)

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: selectedProfile,
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        if (command === 'switch_response_profile') {
          expect(args).toEqual({ profile: 'quality' })
          selectedProfile = 'quality'
          return {
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = getResponseProfileSelect(container)

    await act(async () => {
      setSelectValue(select, 'quality')
      await Promise.resolve()
    })

    expect(delayedStartPending).toBe(true)

    await act(async () => {
      delayedStartPending = false
      rejectLiveAudioSource(new Error('Permission denied'))
      await Promise.resolve()
    })

    expect(invokedCommands).toContain('switch_response_profile')
    expect(container.textContent).not.toContain('live_audio_error:\nPermission denied')
    expect(container.textContent).not.toContain('live_audio:\ndefault microphone started')
  })

  it('automatically marks silence from backend speech activity updates', async () => {
    const stop = vi.fn()
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let nowMs = 1_000

    class FakeAudio {
      play(): Promise<void> {
        return Promise.resolve()
      }
    }

    Date.now = () => nowMs
    Object.defineProperty(globalThis, 'Audio', {
      configurable: true,
      value: FakeAudio,
    })

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        invokedCommands.push(command)

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        if (command === 'ingest_audio_frame') {
          return {
            runtime_phase: 'listening',
            transcription_ready_samples: null,
            transcript_text: null,
            last_activity_ms: 1_000,
            capturing_utterance: true,
            preroll_samples: 4,
            utterance_samples: 4,
          }
        }

        expect(command).toBe('mark_silence')

        return {
          runtime_phase: 'processing',
          transcription_ready_samples: 3200,
          transcript_text: null,
          last_activity_ms: null,
          capturing_utterance: false,
          preroll_samples: 4,
          utterance_samples: 0,
        }
      },
    }

    const { container } = await renderApp()

    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })

    nowMs = 3_600

    await act(async () => {
      await onFrame?.([0.001, -0.001, 0.001, -0.001])
      await Promise.resolve()
    })

    expect(invokedCommands).toEqual([
      'get_startup_state',
      'ingest_audio_frame',
      'ingest_audio_frame',
      'mark_silence',
    ])
    expect(container.textContent).toContain('transcription_ready:\n3200 samples captured')
  })

  it('submits the transcribed voice prompt after silence and returns to wake-word waiting', async () => {
    const stop = vi.fn()
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let nowMs = 1_000

    class FakeAudio {
      play(): Promise<void> {
        return Promise.resolve()
      }
    }

    Date.now = () => nowMs
    Object.defineProperty(globalThis, 'Audio', {
      configurable: true,
      value: FakeAudio,
    })

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        invokedCommands.push(command)

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        if (command === 'ingest_audio_frame') {
          return {
            runtime_phase: 'listening',
            transcription_ready_samples: null,
            transcript_text: null,
            last_activity_ms: 1_000,
            capturing_utterance: true,
            preroll_samples: 4,
            utterance_samples: 4,
          }
        }

        if (command === 'mark_silence') {
          return {
            runtime_phase: 'processing',
            transcription_ready_samples: 3200,
            transcript_text: 'Open the pull request',
            last_activity_ms: null,
            capturing_utterance: false,
            preroll_samples: 4,
            utterance_samples: 0,
          }
        }

        if (command === 'submit_prompt') {
          expect(args).toEqual({ prompt: 'Open the pull request' })

          return {
            events: [{ kind: 'text', text: 'Voice execution response' }],
            stderr: '',
            exit_code: null,
            runtime_phase: 'sleeping',
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()

    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })

    nowMs = 3_600

    await act(async () => {
      await onFrame?.([0.001, -0.001, 0.001, -0.001])
      await Promise.resolve()
    })

    expect(invokedCommands).toEqual([
      'get_startup_state',
      'ingest_audio_frame',
      'ingest_audio_frame',
      'mark_silence',
      'submit_prompt',
    ])
    expect(container.textContent).toContain('transcript:\nOpen the pull request')
    expect(container.textContent).toContain('Open the pull request')
    expect(container.textContent).toContain('Voice execution response')
    expect(container.textContent).toContain('Mic on')
  })

  it('uses configured startup silence timeout for auto-stop timing', async () => {
    const stop = vi.fn()
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let nowMs = 1_000

    class FakeAudio {
      play(): Promise<void> {
        return Promise.resolve()
      }
    }

    Date.now = () => nowMs
    Object.defineProperty(globalThis, 'Audio', {
      configurable: true,
      value: FakeAudio,
    })

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        invokedCommands.push(command)

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 3000,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        if (command === 'ingest_audio_frame') {
          return {
            runtime_phase: 'listening',
            transcription_ready_samples: null,
            transcript_text: null,
            last_activity_ms: 1_000,
            capturing_utterance: true,
            preroll_samples: 4,
            utterance_samples: 4,
          }
        }

        if (command === 'mark_silence') {
          return {
            runtime_phase: 'processing',
            transcription_ready_samples: 3200,
            transcript_text: null,
            last_activity_ms: null,
            capturing_utterance: false,
            preroll_samples: 4,
            utterance_samples: 0,
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    await renderApp()

    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })

    nowMs = 3_600

    await act(async () => {
      await onFrame?.([0.001, -0.001, 0.001, -0.001])
      await Promise.resolve()
    })

    expect(invokedCommands).toEqual([
      'get_startup_state',
      'ingest_audio_frame',
      'ingest_audio_frame',
    ])

    nowMs = 4_000

    await act(async () => {
      await onFrame?.([0.001, -0.001, 0.001, -0.001])
      await Promise.resolve()
    })

    expect(invokedCommands).toEqual([
      'get_startup_state',
      'ingest_audio_frame',
      'ingest_audio_frame',
      'ingest_audio_frame',
      'mark_silence',
    ])
  })

  it('returns to waiting when mark_silence transcription fails', async () => {
    const stop = vi.fn()
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let nowMs = 1_000

    class FakeAudio {
      play(): Promise<void> {
        return Promise.resolve()
      }
    }

    Date.now = () => nowMs
    Object.defineProperty(globalThis, 'Audio', {
      configurable: true,
      value: FakeAudio,
    })

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        invokedCommands.push(command)

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        if (command === 'ingest_audio_frame') {
          return {
            runtime_phase: 'listening',
            transcription_ready_samples: null,
            transcript_text: null,
            last_activity_ms: 1_000,
            capturing_utterance: true,
            preroll_samples: 4,
            utterance_samples: 4,
          }
        }

        if (command === 'mark_silence') {
          throw 'utterance transcription failed: InvalidTranscript(EmptyText)'
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()

    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })

    nowMs = 3_600

    await act(async () => {
      await onFrame?.([0.001, -0.001, 0.001, -0.001])
      await Promise.resolve()
    })

    expect(invokedCommands).toEqual([
      'get_startup_state',
      'ingest_audio_frame',
      'ingest_audio_frame',
      'mark_silence',
    ])
    expect(container.textContent).toContain('Runtime control error (mark_silence): utterance transcription failed: InvalidTranscript(EmptyText)')
    expect(container.textContent).toContain('Mic on')
  })

  it('waits for the stop cue before starting silence processing', async () => {
    const stop = vi.fn()
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let nowMs = 1_000
    let hasPendingStopCue = false
    let resolveStopCue: () => void = () => {
      throw new Error('stop cue was not pending')
    }

    class FakeAudio {
      source: string

      constructor(source: string) {
        this.source = source
      }

      play(): Promise<void> {
        if (this.source === 'resources/stop-listening.wav') {
          return new Promise<void>((resolve) => {
            hasPendingStopCue = true
            resolveStopCue = resolve
          })
        }

        return Promise.resolve()
      }
    }

    Date.now = () => nowMs
    Object.defineProperty(globalThis, 'Audio', {
      configurable: true,
      value: FakeAudio,
    })

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        invokedCommands.push(command)

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        if (command === 'ingest_audio_frame') {
          return {
            runtime_phase: 'listening',
            transcription_ready_samples: null,
            transcript_text: null,
            last_activity_ms: 1_000,
            capturing_utterance: true,
            preroll_samples: 4,
            utterance_samples: 4,
          }
        }

        expect(command).toBe('mark_silence')

        return {
          runtime_phase: 'processing',
          transcription_ready_samples: 3200,
          transcript_text: null,
          last_activity_ms: null,
          capturing_utterance: false,
          preroll_samples: 4,
          utterance_samples: 0,
        }
      },
    }

    await renderApp()

    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })

    nowMs = 3_600

    let pendingFrame: Promise<void> | void
    await act(async () => {
      pendingFrame = onFrame?.([0.001, -0.001, 0.001, -0.001])
      await Promise.resolve()
    })

    expect(invokedCommands).toEqual([
      'get_startup_state',
      'ingest_audio_frame',
      'ingest_audio_frame',
    ])

    if (hasPendingStopCue) {
      resolveStopCue()
    }

    await act(async () => {
      await pendingFrame
      await Promise.resolve()
    })

    expect(invokedCommands).toEqual([
      'get_startup_state',
      'ingest_audio_frame',
      'ingest_audio_frame',
      'mark_silence',
    ])
  })

  it('shows microphone capture errors without changing the backend contract', async () => {
    startLiveAudioSourceMock.mockRejectedValue(new Error('Permission denied'))

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        expect(command).toBe('get_startup_state')

        return {
          kind: 'ready',
          cue_asset_paths: {
            start_listening: 'resources/start-listening.wav',
            stop_listening: 'resources/stop-listening.wav',
          },
          runtime_phase: 'sleeping',
          voice_input_available: true,
          voice_input_error: null,
          silence_timeout_ms: 1500,
          selected_response_profile: 'quality',
          supported_response_profiles: ['fast', 'quality'],
        }
      },
    }

    const { container } = await renderApp()

    expect(container.textContent).toContain('live_audio_error:\nPermission denied')
    expect(container.textContent).toContain('Start mic')
  })

  it('auto-starts microphone capture when voice input is ready', async () => {
    const stop = vi.fn()

    startLiveAudioSourceMock.mockResolvedValue({ stop })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        expect(command).toBe('get_startup_state')

        return {
          kind: 'ready',
          cue_asset_paths: {
            start_listening: 'resources/start-listening.wav',
            stop_listening: 'resources/stop-listening.wav',
          },
          runtime_phase: 'sleeping',
          voice_input_available: true,
          voice_input_error: null,
          silence_timeout_ms: 1500,
          selected_response_profile: 'quality',
          supported_response_profiles: ['fast', 'quality'],
        }
      },
    }

    const { container } = await renderApp()

    expect(startLiveAudioSourceMock).toHaveBeenCalledTimes(1)
    expect(container.textContent).toContain('live_audio:\ndefault microphone started')
    expect(getControlButton(container, 'Start listening').disabled).toBe(true)
  })

  it('does not auto-stop on silence when the toggle is disabled', async () => {
    const stop = vi.fn()
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let nowMs = 1_000

    class FakeAudio {
      play(): Promise<void> {
        return Promise.resolve()
      }
    }

    Date.now = () => nowMs
    Object.defineProperty(globalThis, 'Audio', {
      configurable: true,
      value: FakeAudio,
    })

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        invokedCommands.push(command)

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        if (command === 'ingest_audio_frame') {
          return {
            runtime_phase: 'listening',
            transcription_ready_samples: null,
            transcript_text: null,
            last_activity_ms: 1_000,
            capturing_utterance: true,
            preroll_samples: 4,
            utterance_samples: 4,
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()

    await act(async () => {
      getAutoStopToggle(container).click()
      await Promise.resolve()
    })

    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })

    nowMs = 3_600

    await act(async () => {
      await onFrame?.([0.001, -0.001, 0.001, -0.001])
      await Promise.resolve()
    })

    expect(invokedCommands).toEqual([
      'get_startup_state',
      'ingest_audio_frame',
      'ingest_audio_frame',
    ])
    expect(container.textContent).toContain('Mic on')
  })

  it('surfaces raw runtime control rejection messages', async () => {
    startLiveAudioSourceMock.mockRejectedValue(new Error('Permission denied'))

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: true,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
          }
        }

        if (command === 'begin_listening') {
          throw 'invalid transition: begin_listening from listening'
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()

    await act(async () => {
      getControlButton(container, 'Start listening').click()
      await Promise.resolve()
    })

    expect(container.textContent).toContain(
      'Runtime control error (begin_listening): invalid transition: begin_listening from listening',
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

  for (let index = 0; index < 5; index += 1) {
    await act(async () => {
      await Promise.resolve()
    })
  }

  return { container }
}

function getComposer(container: HTMLElement): HTMLTextAreaElement {
  const composer = container.querySelector<HTMLTextAreaElement>('#promptComposer')

  if (composer === null) {
    throw new Error('Missing composer textarea')
  }

  return composer
}

function getConversation(container: HTMLElement): HTMLElement {
  const conversation = container.querySelector<HTMLElement>('main.conversation')

  if (conversation === null) {
    throw new Error('Missing conversation timeline')
  }

  return conversation
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
    | 'Start mic'
    | 'Stop mic'
    | 'Stop listening and process'
    | 'Start listening'
    | 'Reset to idle',
): HTMLButtonElement {
  const buttons = Array.from(container.querySelectorAll<HTMLButtonElement>('button'))
  const button = buttons.find((candidate) => candidate.textContent?.trim() === label)

  if (button === undefined) {
    throw new Error(`Missing ${label} control button`)
  }

  return button
}

function getAutoStopToggle(container: HTMLElement): HTMLInputElement {
  const toggle = container.querySelector<HTMLInputElement>('input[type="checkbox"]')

  if (toggle === null) {
    throw new Error('Missing auto stop on silence toggle')
  }

  return toggle
}

function getResponseProfileSelect(container: HTMLElement): HTMLSelectElement {
  const select = container.querySelector<HTMLSelectElement>('#responseProfileSelect')

  if (select === null) {
    throw new Error('Missing response profile select')
  }

  return select
}

function setSelectValue(select: HTMLSelectElement, value: string): void {
  select.value = value
  select.dispatchEvent(new Event('change', { bubbles: true }))
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

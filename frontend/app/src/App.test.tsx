import { act } from 'react'
import { createRoot } from 'react-dom/client'
import type { Root } from 'react-dom/client'
import { afterEach, describe, expect, it, vi } from 'vitest'
import * as liveAudioSourceModule from './lib/liveAudioSource'
import type { StartLiveAudioSourceOptions } from './lib/liveAudioSource'
import App from './App'

const startLiveAudioSourceMock = vi.spyOn(liveAudioSourceModule, 'startLiveAudioSource')
const listAudioInputDevicesMock = vi.spyOn(liveAudioSourceModule, 'listAudioInputDevices')
  .mockResolvedValue([])

const mountedContainers: HTMLElement[] = []
const mountedRoots: Root[] = []
const originalAudio = globalThis.Audio
const originalAudioContext = globalThis.AudioContext
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

  if (originalAudioContext === undefined) {
    Reflect.deleteProperty(globalThis, 'AudioContext')
  } else {
    Object.defineProperty(globalThis, 'AudioContext', {
      configurable: true,
      value: originalAudioContext,
    })
  }

  Reflect.deleteProperty(window, '__TAURI_INTERNALS__')
  delete document.documentElement.dataset['uiTheme']
  Date.now = originalDateNow
  vi.useRealTimers()
  startLiveAudioSourceMock.mockReset()
  listAudioInputDevicesMock.mockReset()
  listAudioInputDevicesMock.mockResolvedValue([])
  window.localStorage.removeItem('voxgolem.audioInputDeviceId')
})

describe('App', () => {
  it('auto-follows the latest message when conversation grows', async () => {
    const { container } = await renderApp()
    const composer = getComposer(container)
    const sendButton = getSendButton(container)
    const conversation = getConversation(container)
    expect(container.querySelector('.shell__header')).toBeNull()
    expect(conversation).toBeTruthy()
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
    expect(container.textContent).toContain(
      'Browser preview only — no backend response was generated. Prompt: Draft release notes',
    )
  })

  it('starts with an empty chat transcript', async () => {
    const { container } = await renderApp()
    const conversation = getConversation(container)

    expect(conversation.textContent).toBe('')
  })

  it('keeps the typed shell nonfatal and lists every zero-asset capability in one red notice', async () => {
    const capabilityIds = [
      'custom_provider', 'opencode', 'local_fast', 'local_quality', 'qwen_prediction',
      'wake_word', 'vad', 'parakeet', 'tts', 'deep', 'review',
    ]
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_ui_text_size') return 'medium'
        if (command === 'get_ui_theme') return 'dark'
        if (command === 'get_assistant_settings') return defaultAssistantSettings()
        if (command === 'get_startup_state') return {
          kind: 'ready',
          cue_asset_paths: { start_listening: 'start.wav', stop_listening: 'stop.wav' },
          runtime_phase: 'sleeping',
          voice_input_available: false,
          voice_input_error: 'voice assets are not configured',
          silence_timeout_ms: 1500,
          selected_response_profile: 'fast',
          supported_response_profiles: [],
          capabilities: capabilityIds.map((id) => ({
            id,
            state: id === 'review' ? 'failed' : 'not_configured',
            reason: `${id} unavailable`,
            actual_provider: null,
          })),
        }
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const composer = getComposer(container)
    await act(async () => setTextAreaValue(composer, 'typed input remains editable'))

    expect(composer.disabled).toBe(false)
    expect(getSendButton(container).disabled).toBe(true)
    expect(container.textContent).toContain('Sending disabled:')
    expect(container.querySelector('.notice-toast--error')).not.toBeNull()
    for (const id of capabilityIds) {
      expect(container.textContent).toContain(`${id} unavailable`)
    }
    const settingsButton = getButtonByLabel(container, 'Settings')
    await act(async () => { settingsButton.click(); await Promise.resolve() })
    const instantSelect = container.querySelector<HTMLSelectElement>('#assistantInstantSelect')
    expect(instantSelect).not.toBeNull()
    expect(Array.from(instantSelect?.options ?? []).every((option) => option.disabled || option.value === 'local-fast')).toBe(true)
  })

  it('opens settings and live-updates persisted app text size', async () => {
    const invoked: Array<{ command: string; args: unknown }> = []
    let textSize = 'medium'

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        invoked.push({ command, args })

        if (command === 'get_ui_text_size') {
          return textSize
        }

        if (command === 'set_ui_text_size') {
          if (!isRecord(args) || typeof args['textSize'] !== 'string') {
            throw new Error('textSize argument is required')
          }

          textSize = args['textSize']
          return textSize
        }

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
            tts_enabled: false,
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const shell = getShell(container)

    expect(shell.dataset['uiTextSize']).toBe('medium')

    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })

    expect(container.textContent).toContain('Text size')
    expect(container.textContent).toContain('Medium (100%)')

    await act(async () => {
      getButtonByLabel(container, 'Increase text size').click()
      await Promise.resolve()
    })

    expect(shell.dataset['uiTextSize']).toBe('large')
    expect(container.textContent).toContain('Large (112.5%)')

    await act(async () => {
      getButtonByLabel(container, 'Decrease text size').click()
      await Promise.resolve()
    })

    expect(shell.dataset['uiTextSize']).toBe('medium')
    expect(container.textContent).toContain('Medium (100%)')
    expect(invoked).toContainEqual({ command: 'set_ui_text_size', args: { textSize: 'large' } })
    expect(invoked).toContainEqual({ command: 'set_ui_text_size', args: { textSize: 'medium' } })
  })

  it('keeps an immediate text-size change when delayed persisted settings load later', async () => {
    let resolvePersistedTextSize: (value: string) => void = () => undefined
    const persistedTextSize = new Promise<string>((resolve) => {
      resolvePersistedTextSize = resolve
    })
    let textSize = 'medium'

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        if (command === 'get_ui_text_size') {
          return persistedTextSize
        }

        if (command === 'set_ui_text_size') {
          if (!isRecord(args) || typeof args['textSize'] !== 'string') {
            throw new Error('textSize argument is required')
          }

          textSize = args['textSize']
          return textSize
        }

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
            tts_enabled: false,
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const shell = getShell(container)

    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })

    await act(async () => {
      getButtonByLabel(container, 'Increase text size').click()
      await Promise.resolve()
    })

    expect(shell.dataset['uiTextSize']).toBe('large')

    await act(async () => {
      resolvePersistedTextSize('small')
      await persistedTextSize
      await Promise.resolve()
    })

    expect(shell.dataset['uiTextSize']).toBe('large')
  })

  it('starts in persisted dark mode and toggles to light mode', async () => {
    const invoked: Array<{ command: string; args: unknown }> = []
    let theme = 'dark'

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        invoked.push({ command, args })

        if (command === 'get_ui_theme') {
          return theme
        }

        if (command === 'set_ui_theme') {
          if (!isRecord(args) || typeof args['theme'] !== 'string') {
            throw new Error('theme argument is required')
          }

          theme = args['theme']
          return theme
        }

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
            tts_enabled: false,
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const shell = getShell(container)

    expect(shell.dataset['uiTheme']).toBe('dark')
    expect(getButtonByLabel(container, 'Switch to light mode').textContent).toBe('☀')

    await act(async () => {
      getButtonByLabel(container, 'Switch to light mode').click()
      await Promise.resolve()
    })

    expect(shell.dataset['uiTheme']).toBe('light')
    expect(getButtonByLabel(container, 'Switch to dark mode').textContent).toBe('☾')
    expect(invoked).toContainEqual({ command: 'set_ui_theme', args: { theme: 'light' } })
  })

  it('keeps an immediate theme toggle when delayed persisted theme loads later', async () => {
    let resolvePersistedTheme: (value: string) => void = () => undefined
    const persistedTheme = new Promise<string>((resolve) => {
      resolvePersistedTheme = resolve
    })
    let theme = 'dark'

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        if (command === 'get_ui_theme') {
          return persistedTheme
        }

        if (command === 'set_ui_theme') {
          if (!isRecord(args) || typeof args['theme'] !== 'string') {
            throw new Error('theme argument is required')
          }

          theme = args['theme']
          return theme
        }

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
            tts_enabled: false,
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const shell = getShell(container)

    await act(async () => {
      getButtonByLabel(container, 'Switch to light mode').click()
      await Promise.resolve()
    })

    expect(shell.dataset['uiTheme']).toBe('light')

    await act(async () => {
      resolvePersistedTheme('dark')
      await persistedTheme
      await Promise.resolve()
    })

    expect(shell.dataset['uiTheme']).toBe('light')
  })

  it('defaults to dark mode when persisted theme is unsupported', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_ui_theme') {
          return 'solarized'
        }

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
            tts_enabled: false,
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()

    expect(getShell(container).dataset['uiTheme']).toBe('dark')
    expect(getButtonByLabel(container, 'Switch to light mode').textContent).toBe('☀')
  })

  it('reverts theme and shows a notice when theme persistence fails', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_ui_theme') {
          return 'dark'
        }

        if (command === 'set_ui_theme') {
          throw new Error('theme write failed')
        }

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
            tts_enabled: false,
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()

    await act(async () => {
      getButtonByLabel(container, 'Switch to light mode').click()
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(getShell(container).dataset['uiTheme']).toBe('dark')
    expect(container.textContent).toContain('Theme not saved')
    expect(container.textContent).toContain('theme write failed')
  })

  it('reverts text size and shows a notice when settings persistence fails', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_ui_text_size') {
          return 'medium'
        }

        if (command === 'set_ui_text_size') {
          throw new Error('state write failed')
        }

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
            tts_enabled: false,
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const shell = getShell(container)

    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })

    await act(async () => {
      getButtonByLabel(container, 'Increase text size').click()
      await Promise.resolve()
      await Promise.resolve()
    })

    expect(shell.dataset['uiTextSize']).toBe('medium')
    expect(container.textContent).toContain('Setting not saved')
    expect(container.textContent).toContain('state write failed')
  })

  it('supports keyboard focus and Escape close for the settings overlay', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_ui_text_size') {
          return 'medium'
        }

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
            tts_enabled: false,
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const settingsButton = getButtonByLabel(container, 'Settings')

    await act(async () => {
      settingsButton.click()
      await Promise.resolve()
    })

    const closeButton = getButtonByLabel(container, 'Close settings')
    expect(document.activeElement).toBe(closeButton)

    await act(async () => {
      closeButton.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
      await new Promise((resolve) => window.setTimeout(resolve, 0))
    })

    expect(container.textContent).not.toContain('Adjust the app text size live.')
    expect(document.activeElement).toBe(settingsButton)
  })

  it('falls back to medium when persisted text size is unsupported', async () => {
    let textSize = 'giant'

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        if (command === 'get_ui_text_size') {
          return textSize
        }

        if (command === 'set_ui_text_size') {
          if (!isRecord(args) || typeof args['textSize'] !== 'string') {
            throw new Error('textSize argument is required')
          }

          textSize = args['textSize']
          return textSize
        }

        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
            tts_enabled: false,
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const shell = getShell(container)

    expect(shell.dataset['uiTextSize']).toBe('medium')

    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })

    expect(container.textContent).toContain('Medium (100%)')

    await act(async () => {
      getButtonByLabel(container, 'Increase text size').click()
      await Promise.resolve()
    })

    expect(shell.dataset['uiTextSize']).toBe('large')
    expect(container.textContent).toContain('Large (112.5%)')
  })

  it('submits from Enter and ignores Shift+Enter', async () => {
    const { container } = await renderApp()
    const composer = getComposer(container)
    expect(container.querySelector('.prompt-composer')).not.toBeNull()
    expect(container.querySelector('.prompt-composer__ghost')).toBeNull()

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

    expect(container.textContent).not.toContain(
      'Browser preview only — no backend response was generated. Prompt: Line one',
    )

    await act(async () => {
      composer.dispatchEvent(
        new KeyboardEvent('keydown', {
          key: 'Enter',
          bubbles: true,
        }),
      )
    })

    expect(container.textContent).toContain(
      'Browser preview only — no backend response was generated. Prompt: Line one',
    )
  })

  it('replaces composer hint with a TTS toggle and invokes backend toggle command', async () => {
    const invoked: string[] = []

    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        invoked.push(command)

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
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
            tts_enabled: false,
          }
        }

        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')

        if (command === 'set_tts_enabled') {
          expect(args).toEqual({ enabled: true })
          return { enabled: true, sample_rate_hz: 22050 }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const ttsToggle = getTtsToggle(container)

    expect(container.textContent).not.toContain('Enter to send, Shift+Enter for newline')
    expect(ttsToggle.checked).toBe(false)

    await act(async () => {
      ttsToggle.click()
      await Promise.resolve()
    })

    expect(invoked).toContain('set_tts_enabled')
    expect(ttsToggle.checked).toBe(true)
  })

  it('synthesizes assistant response when TTS is enabled', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    const invoked: string[] = []

    let resumeCallCount = 0

    class FakeAudioContext {
      destination = {} as AudioDestinationNode
      state: AudioContextState = 'suspended'

    createBuffer(): AudioBuffer {
        return {
          copyToChannel: () => {},
        } as unknown as AudioBuffer
      }

      createBufferSource(): AudioBufferSourceNode {
        return {
          buffer: null,
          connect: () => {},
          onended: null,
          start: function start(this: AudioBufferSourceNode) {
            this.onended?.(new Event('ended'))
          },
        } as unknown as AudioBufferSourceNode
      }

      createGain(): GainNode {
        return {
          gain: { value: 1 } as AudioParam,
          connect: () => {},
        } as unknown as GainNode
      }

      async resume(): Promise<void> {
        resumeCallCount += 1
        this.state = 'running'
      }

      async close(): Promise<void> {}
    }

    Object.defineProperty(globalThis, 'AudioContext', {
      configurable: true,
      value: FakeAudioContext,
    })

    window.__TAURI_INTERNALS__ = {
      listen: async (_event, handler) => {
        promptEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        invoked.push(command)

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
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
            tts_enabled: false,
          }
        }

        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')

        if (command === 'set_tts_enabled') {
          return { enabled: true, sample_rate_hz: 22050 }
        }

        if (command === 'submit_prompt') {
          expect(args).toMatchObject({ requestId: expect.any(String), prompt: 'Hello voice' })
          promptEventHandler?.({ payload: { request_id: (args as { requestId: string }).requestId, kind: 'text', text: 'Voice response' } })
          return {
            request_id: (args as { requestId: string }).requestId,
            outcome: 'completed',
            error_message: null,
            runtime_phase: 'sleeping',
          }
        }

        if (command === 'synthesize_local_tts') {
          expect(args).toEqual({
            text: 'Voice response',
            playbackId: 2,
          })
          return {
            pcm_f32: [0.0, 0.1, -0.1],
            sample_rate_hz: 22050,
            duration_ms: 1,
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const ttsToggle = getTtsToggle(container)
    const composer = getComposer(container)
    const sendButton = getSendButton(container)

    await act(async () => {
      ttsToggle.click()
      await Promise.resolve()
    })

    await act(async () => {
      setTextAreaValue(composer, 'Hello voice')
    })

    await act(async () => {
      sendButton.click()
      await Promise.resolve()
    })

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0))
      await new Promise((resolve) => setTimeout(resolve, 0))
      await new Promise((resolve) => setTimeout(resolve, 10))
    })

    expect(invoked).toContain('synthesize_local_tts')
    expect(resumeCallCount).toBe(1)
  })

  it('stops active TTS playback when Escape cancels a prompt', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let finishPrompt: ((value: unknown) => void) | undefined
    let requestId = ''
    const stop = vi.fn()
    const started = vi.fn()
    class FakeAudioContext {
      destination = {} as AudioDestinationNode
      state: AudioContextState = 'running'
      createBuffer(): AudioBuffer { return { copyToChannel: () => {} } as unknown as AudioBuffer }
      createBufferSource(): AudioBufferSourceNode {
        return { buffer: null, connect: () => {}, onended: null, start: started, stop } as unknown as AudioBufferSourceNode
      }
      createGain(): GainNode { return { gain: { value: 1 } as AudioParam, connect: () => {} } as unknown as GainNode }
      async close(): Promise<void> {}
    }
    Object.defineProperty(globalThis, 'AudioContext', { configurable: true, value: FakeAudioContext })
    window.__TAURI_INTERNALS__ = {
      listen: async (_event, handler) => { promptEventHandler = handler; return () => undefined },
      invoke: async (command, args) => {
        if (command === 'get_startup_state') return readyStartupState({ prompt_cancellation_available: true, tts_enabled: false })
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        if (command === 'set_tts_enabled') return { enabled: true }
        if (command === 'submit_prompt') { requestId = (args as { requestId: string }).requestId; return new Promise((resolve) => { finishPrompt = resolve }) }
        if (command === 'synthesize_local_tts') return { pcm_f32: [0], sample_rate_hz: 22050, duration_ms: 1 }
        if (command === 'cancel_prompt') return null
        throw new Error(`unexpected command: ${command}`)
      },
    }
    const { container } = await renderApp()
    await act(async () => { getTtsToggle(container).click(); await new Promise((resolve) => setTimeout(resolve, 0)); setTextAreaValue(getComposer(container), 'Say this'); getSendButton(container).click(); await Promise.resolve() })
    await act(async () => { promptEventHandler?.({ payload: { request_id: requestId, kind: 'text', text: 'Valid first line\nmore' } }); await new Promise((resolve) => setTimeout(resolve, 0)) })
    await act(async () => { window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' })); await Promise.resolve() })
    expect(stop).toHaveBeenCalledOnce()
    expect(started).toHaveBeenCalledOnce()
    await act(async () => {
      finishPrompt?.({ request_id: requestId, runtime_phase: 'sleeping', outcome: 'cancelled' })
      await Promise.resolve()
    })
  })

  it('does not start TTS playback when synthesis resolves after Escape', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let resolveSynthesis: ((value: unknown) => void) | undefined
    let requestId = ''
    const started = vi.fn()
    let synthesisInvoked = false
    class FakeAudioContext {
      destination = {} as AudioDestinationNode
      state: AudioContextState = 'running'
      createBuffer(): AudioBuffer { return { copyToChannel: () => {} } as unknown as AudioBuffer }
      createBufferSource(): AudioBufferSourceNode { return { buffer: null, connect: () => {}, onended: null, start: started, stop: vi.fn() } as unknown as AudioBufferSourceNode }
      createGain(): GainNode { return { gain: { value: 1 } as AudioParam, connect: () => {} } as unknown as GainNode }
      async close(): Promise<void> {}
    }
    Object.defineProperty(globalThis, 'AudioContext', { configurable: true, value: FakeAudioContext })
    window.__TAURI_INTERNALS__ = {
      listen: async (_event, handler) => { promptEventHandler = handler; return () => undefined },
      invoke: async (command, args) => {
        if (command === 'get_startup_state') return readyStartupState({ prompt_cancellation_available: true })
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        if (command === 'set_tts_enabled') return { enabled: true }
        if (command === 'submit_prompt') { requestId = (args as { requestId: string }).requestId; return new Promise(() => {}) }
        if (command === 'synthesize_local_tts') {
          synthesisInvoked = true
          return new Promise((resolve) => { resolveSynthesis = resolve })
        }
        if (command === 'cancel_prompt') return null
        throw new Error(`unexpected command: ${command}`)
      },
    }
    const { container } = await renderApp()
    await act(async () => { getTtsToggle(container).click(); await new Promise((resolve) => setTimeout(resolve, 0)); setTextAreaValue(getComposer(container), 'Say this'); getSendButton(container).click(); await Promise.resolve() })
    await act(async () => {
      promptEventHandler?.({ payload: { request_id: requestId, kind: 'text', text: 'Valid first line\nmore' } })
      await new Promise((resolve) => setTimeout(resolve, 0))
    })
    expect(synthesisInvoked).toBe(true)
    expect(resolveSynthesis).toBeDefined()
    await act(async () => { window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' })); await Promise.resolve() })
    await act(async () => { resolveSynthesis?.({ pcm_f32: [0], sample_rate_hz: 22050, duration_ms: 1 }); await Promise.resolve() })
    expect(started).not.toHaveBeenCalled()
  })

  it('disables update installation from TTS synthesis through playback completion', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let resolveSynthesis: ((value: unknown) => void) | undefined
    let source: AudioBufferSourceNode | undefined
    class FakeAudioContext {
      destination = {} as AudioDestinationNode
      state: AudioContextState = 'running'
      createBuffer(): AudioBuffer { return { copyToChannel: () => {} } as unknown as AudioBuffer }
      createBufferSource(): AudioBufferSourceNode {
        source = { buffer: null, connect: () => {}, onended: null, start: vi.fn(), stop: vi.fn() } as unknown as AudioBufferSourceNode
        return source
      }
      createGain(): GainNode { return { gain: { value: 1 } as AudioParam, connect: () => {} } as unknown as GainNode }
      async close(): Promise<void> {}
    }
    Object.defineProperty(globalThis, 'AudioContext', { configurable: true, value: FakeAudioContext })
    window.__TAURI_INTERNALS__ = {
      listen: async (event, handler) => {
        if (event === 'prompt-execution-event') promptEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        if (command === 'get_startup_state') return readyStartupState({ tts_enabled: true })
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        if (command === 'check_for_update') {
          return {
            status: 'available',
            current_version: '2026.7.27-1',
            version: '2026.7.28-1',
            notes: null,
            install_behavior: 'install_and_restart',
          }
        }
        if (command === 'submit_prompt') {
          const requestId = (args as { requestId: string }).requestId
          promptEventHandler?.({ payload: { request_id: requestId, kind: 'text', text: 'Spoken response' } })
          return { request_id: requestId, outcome: 'completed', error_message: null, runtime_phase: 'sleeping' }
        }
        if (command === 'synthesize_local_tts') {
          return new Promise((resolve) => { resolveSynthesis = resolve })
        }
        if (command === 'finish_tts_playback') return null
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    await act(async () => { setTextAreaValue(getComposer(container), 'Speak'); getSendButton(container).click(); await Promise.resolve() })
    await act(async () => { getButtonByLabel(container, 'Settings').click(); await Promise.resolve() })
    const install = getButtonByText(container, 'Install and restart')
    expect(resolveSynthesis).toBeDefined()
    expect(install.disabled).toBe(true)

    await act(async () => {
      resolveSynthesis?.({ pcm_f32: [0], sample_rate_hz: 22050, duration_ms: 1 })
      await Promise.resolve()
    })
    expect(source).toBeDefined()
    expect(install.disabled).toBe(true)

    await act(async () => {
      source?.onended?.(new Event('ended'))
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(install.disabled).toBe(false)
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
            capabilities: completeCapabilities(),
        }
      },
    }

    const { container } = await renderApp()
    const select = await getResponseProfileSelect(container)

    expect(select.value).toBe('local-fast')
    expect(Array.from(select.options).map((option) => option.text)).toEqual(['Local: Fast', 'Local: Quality', 'Custom: GPT-5.6 Sol High', 'Custom: GPT-5.6 Luna Low', 'OpenCode: GPT-5.6 Sol High', 'OpenCode: GPT-5.6 Luna Low'])
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
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'switch_response_profile') {
          expect(args).toEqual({ profile: 'quality' })
          selectedProfile = 'quality'
          return {
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'set_assistant_settings') {
          return (args as { settings: unknown }).settings
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = await getResponseProfileSelect(container)

    await act(async () => {
      setSelectValue(select, 'quality')
      await Promise.resolve()
    })

    expect(invokedCommands).toContain('switch_response_profile')
    expect(select.value).toBe('local-quality')
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
            capabilities: completeCapabilities(),
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
            capabilities: completeCapabilities(),
              })
            }
          })
        }

        if (command === 'set_assistant_settings') {
          return (args as { settings: unknown }).settings
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = await getResponseProfileSelect(container)

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

    expect(select.value).toBe('local-quality')
  })

  it('restores previous profile and hides profile switch errors from chat', async () => {
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
              voice_input_available: false,
              voice_input_error: null,
              silence_timeout_ms: 1500,
              selected_response_profile: 'fast',
              supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
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
              voice_input_available: false,
              voice_input_error: null,
              silence_timeout_ms: 1500,
              message: 'Loading local Gemma model...',
              selected_response_profile: 'fast',
              supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
            }
          }

          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'executing',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'switch_response_profile') {
          throw new Error('response backend is busy; wait for the active operation to finish')
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = await getResponseProfileSelect(container)

    await act(async () => {
      setSelectValue(select, 'quality')
      await Promise.resolve()
      await new Promise((resolve) => window.setTimeout(resolve, 600))
    })

    expect(startupStateCallCount).toBe(3)
    expect(nonDiagnosticCommands(invokedCommands)).toEqual([
      'get_startup_state',
      'switch_response_profile',
      'get_startup_state',
      'get_startup_state',
    ])
    expect(select.value).toBe('local-fast')
    expect(container.textContent).toContain('Profile switch failed')
    expect(container.textContent).not.toContain(
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
            capabilities: completeCapabilities(),
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
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'set_assistant_settings') {
          return (args as { settings: unknown }).settings
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = await getResponseProfileSelect(container)

    await act(async () => {
      setSelectValue(select, 'quality')
      await Promise.resolve()
    })

    expect(startupStateCallCount).toBe(2)
    expect(container.textContent).toContain('Startup error: failed to initialize local llama.cpp runtime: boom')
    expect(container.textContent).not.toContain('Response profile switch error:')
  })

  it('surfaces requested profile failure when the previous runtime is restored', async () => {
    let startupStateCallCount = 0
    const sources: Array<{ stop: ReturnType<typeof vi.fn> }> = []
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      const source = { stop: vi.fn(), onFrame: options.onFrame }
      sources.push(source)
      return source
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_startup_state') {
          startupStateCallCount += 1
          if (startupStateCallCount === 1) return readyStartupState()
          return readyStartupState({
            selected_response_profile: 'fast',
            capabilities: completeCapabilities({
              local_quality: 'failed',
              qwen_prediction: 'warming',
            }).map((capability) => capability['id'] === 'local_quality'
              ? { ...capability, reason: 'failed to initialize requested quality profile: boom' }
              : capability),
          })
        }

        if (command === 'switch_response_profile') return {
          selected_response_profile: 'quality',
          supported_response_profiles: ['fast', 'quality'],
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = await getResponseProfileSelect(container)

    await act(async () => {
      setSelectValue(select, 'quality')
      await Promise.resolve()
    })

    expect(select.value).toBe('local-fast')
    expect(container.textContent).toContain('Profile switch failed')
    expect(container.textContent).toContain('failed to initialize requested quality profile: boom')
    expect(startupStateCallCount).toBe(2)
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0))
    })
    expect(sources).toHaveLength(2)
    expect(sources[0]?.stop).toHaveBeenCalledOnce()
    expect(sources[1]?.stop).not.toHaveBeenCalled()
  })

  it('restarts auto-started microphone once after a failed profile rollback', async () => {
    const sources: Array<{ stop: ReturnType<typeof vi.fn>; onFrame: (frame: readonly number[]) => Promise<void> | void }> = []
    const invokedCommands: string[] = []
    let startupCalls = 0
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      const source = { stop: vi.fn(), onFrame: options.onFrame }
      sources.push(source)
      return source
    })
    window.__TAURI_INTERNALS__ = {
      invoke: async (command, args) => {
        invokedCommands.push(command)
        if (command === 'get_startup_state') {
          startupCalls += 1
          return readyStartupState({
            selected_response_profile: startupCalls === 1 ? 'fast' : 'fast',
            capabilities: completeCapabilities(startupCalls === 1 ? {} : { local_quality: 'failed' }),
          })
        }
        if (command === 'switch_response_profile') {
          expect(args).toEqual({ profile: 'quality' })
          return null
        }
        if (command === 'ingest_audio_frame') return { runtime_phase: 'sleeping', last_activity_ms: null, transcription_ready_samples: null, transcript_text: null, capturing_utterance: false, preroll_samples: 0, utterance_samples: 0 }
        if (command === 'get_assistant_settings') return defaultAssistantSettings()
        return null
      },
    }
    const { container } = await renderApp()
    const select = await getResponseProfileSelect(container)
    await act(async () => { setSelectValue(select, 'quality'); await Promise.resolve() })
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); await Promise.resolve() })
    expect(sources).toHaveLength(2)
    expect(sources[0]?.stop).toHaveBeenCalledOnce()
    expect(sources[1]?.stop).not.toHaveBeenCalled()
    await act(async () => { await sources[0]?.onFrame([0.1]); await sources[1]?.onFrame([0.1]); await Promise.resolve() })
    expect(invokedCommands.filter((command) => command === 'ingest_audio_frame')).toHaveLength(1)
  })

  it('keeps a failed active local profile unsendable and retries it explicitly', async () => {
    const invokedCommands: string[] = []
    let recovered = false
    const startupState = (): Record<string, unknown> => readyStartupState({
      capabilities: completeCapabilities(recovered ? {} : { local_fast: 'failed' }).map((capability) =>
        capability['id'] === 'local_fast' && !recovered
          ? { ...capability, reason: 'failed to initialize requested fast profile: boom' }
          : capability),
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        invokedCommands.push(command)
        if (command === 'get_startup_state') return startupState()
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        if (command === 'switch_response_profile') {
          recovered = true
          return { selected_response_profile: 'fast', supported_response_profiles: ['fast', 'quality'] }
        }
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'hello')
      await Promise.resolve()
    })
    expect(getSendButton(container).disabled).toBe(true)

    const select = await getResponseProfileSelect(container)
    expect(select.querySelector<HTMLOptionElement>('option[value="local-fast"]')?.disabled).toBe(false)
    await act(async () => {
      getButtonByLabel(container, 'Retry local profile').click()
      await new Promise((resolve) => setTimeout(resolve, 0))
    })

    expect(nonDiagnosticCommands(invokedCommands)).toContain('switch_response_profile')
    await act(async () => { setTextAreaValue(getComposer(container), 'another prompt'); await Promise.resolve(); await Promise.resolve(); })
    expect(getSendButton(container).disabled).toBe(false)
  })

  it('keeps unrelated providers and retry available after a persistent local retry failure', async () => {
    const failedState = readyStartupState({
      capabilities: completeCapabilities({ local_fast: 'failed' }).map((capability) =>
        capability['id'] === 'local_fast'
          ? { ...capability, reason: 'failed to initialize requested fast profile: still broken' }
          : capability),
    })
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_startup_state') return failedState
        if (command === 'switch_response_profile') {
          return { selected_response_profile: 'fast', supported_response_profiles: ['fast'] }
        }
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = await getResponseProfileSelect(container)
    await act(async () => {
      getButtonByLabel(container, 'Retry local profile').click()
      await Promise.resolve()
    })

    expect(container.textContent).toContain('Profile switch failed')
    expect(getButtonByLabel(container, 'Retry local profile')).not.toBeNull()
    expect(select.querySelector<HTMLOptionElement>('option[value="opencode-sol-high"]')?.disabled).toBe(false)
    expect(getSendButton(container).disabled).toBe(true)
  })

  it('renders only user and assistant prompt execution output when submit command succeeds', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    window.__TAURI_INTERNALS__ = {
      listen: async (_event, handler) => {
        promptEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-quality')

        expect(command).toBe('submit_prompt')
          expect(args).toMatchObject({ requestId: expect.any(String), prompt: 'Draft release notes' })
        promptEventHandler?.({
          payload: { request_id: args && (args as { requestId: string }).requestId, kind: 'text', text: 'OpenCode response' },
        })

        return {
          request_id: (args as { requestId: string }).requestId,
          outcome: 'completed',
          error_message: null,
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

    expect(container.textContent).toContain('Draft release notes')
    expect(container.textContent).toContain('OpenCode response')
  })

  it('preserves typed prompt bytes in the user display and command payload', async () => {
    let submittedPrompt: string | undefined
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command, args) => {
        if (command === 'get_startup_state') return readyStartupState({ selected_response_profile: 'fast' })
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        expect(command).toBe('submit_prompt')
        submittedPrompt = (args as { prompt: string }).prompt
        return {
          request_id: (args as { requestId: string }).requestId,
          outcome: 'completed',
          error_message: null,
          runtime_phase: 'sleeping',
        }
      },
    }

    const { container } = await renderApp()
    const prompt = '  keep me  \nwith spaces  '
    await act(async () => setTextAreaValue(getComposer(container), prompt))
    await act(async () => {
      getSendButton(container).click()
      await Promise.resolve()
    })

    expect(submittedPrompt).toBe(prompt)
    expect(container.querySelector('.message--user .message__content')?.textContent).toBe(prompt)
  })

  it('does not render no-output execution fallback as an assistant message', async () => {
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command, args) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-quality')

        return {
          request_id: (args as { requestId: string }).requestId,
          outcome: 'completed',
          error_message: null,
          runtime_phase: 'sleeping',
        }
      },
    }

    const { container } = await renderApp()
    const composer = getComposer(container)
    const sendButton = getSendButton(container)

    await act(async () => {
      setTextAreaValue(composer, 'No output prompt')
    })

    await act(async () => {
      sendButton.click()
      await Promise.resolve()
    })

    expect(container.textContent).toContain('No output prompt')
    expect(container.textContent).not.toContain('OpenCode backend returned no output.')
    expect(container.textContent).toContain('No response')
    expect(container.textContent).toContain('No response was returned. Try again.')
  })

  it('streams correlated deltas into one assistant bubble and ignores stale events', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let finishPrompt: ((value: unknown) => void) | undefined
    let activeRequestId = ''
    window.__TAURI_INTERNALS__ = {
      listen: async (_event, handler) => {
        promptEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast'],
            capabilities: completeCapabilities(),
            prompt_cancellation_available: true,
          }
        }
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        if (command === 'submit_prompt') {
          if (
            typeof args === 'object' &&
            args !== null &&
            'requestId' in args &&
            typeof args.requestId === 'string'
          ) {
            activeRequestId = args.requestId
          }
          return new Promise((resolve) => {
            finishPrompt = resolve
          })
        }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Stream this')
      getSendButton(container).click()
      await Promise.resolve()
    })
    const stopButton = Array.from(container.querySelectorAll('button')).find(
      (button) => button.textContent === 'Stop',
    )
    expect(stopButton).toBeDefined()

    await act(async () => {
      promptEventHandler?.({
        payload: { request_id: 'stale-request', kind: 'text', text: 'Wrong' },
      })
      const submitCall = promptEventHandler
      expect(submitCall).toBeDefined()
      expect(container.querySelectorAll('.message--assistant')).toHaveLength(0)
    })

    expect(activeRequestId).not.toBe('')
    await act(async () => {
      promptEventHandler?.({
        payload: { request_id: activeRequestId, kind: 'text', text: 'Hello' },
      })
      promptEventHandler?.({
        payload: { request_id: activeRequestId, kind: 'text', text: ' world' },
      })
    })

    const assistantMessages = container.querySelectorAll('.message--assistant')
    expect(assistantMessages).toHaveLength(1)
    expect(assistantMessages[0]?.textContent).toContain('Hello world')
    expect(container.textContent).not.toContain('Wrong')

    await act(async () => {
      finishPrompt?.({
        request_id: activeRequestId,
        runtime_phase: 'sleeping',
        outcome: 'completed',
      })
      await Promise.resolve()
    })
    expect(container.textContent).not.toContain('Executing prompt')
  })

  it('shows string errors rejected by the Tauri prompt command', async () => {
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast'],
            capabilities: completeCapabilities(),
            prompt_cancellation_available: true,
          }
        }
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        if (command === 'submit_prompt') {
          throw 'OpenCode server is not available'
        }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Trigger failure')
      getSendButton(container).click()
      await Promise.resolve()
    })

    expect(container.textContent).toContain('Response failed')
    expect(container.textContent).toContain('OpenCode server is not available')
  })

  it('marks retained partial output as failed when the prompt transport rejects', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let rejectPrompt: ((error: Error) => void) | undefined
    let requestId = ''
    window.__TAURI_INTERNALS__ = {
      listen: async (event, handler) => {
        if (event === 'prompt-execution-event') promptEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          return new Promise((_, reject) => { rejectPrompt = reject })
        }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Retain partial output')
      getSendButton(container).click()
      await Promise.resolve()
      promptEventHandler?.({ payload: { request_id: requestId, kind: 'text', text: 'Partial answer' } })
    })
    await act(async () => {
      rejectPrompt?.(new Error('stream disconnected'))
      await Promise.resolve()
    })

    expect(container.textContent).toContain('Partial answer')
    expect(container.querySelector('[aria-label="Instant status: failed"]')).not.toBeNull()
    expect(container.textContent).toContain('stream disconnected')
  })

  it('shows authoritative OpenCode errors from the final prompt result', async () => {
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command, args) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast'],
            capabilities: completeCapabilities(),
            prompt_cancellation_available: true,
          }
        }
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        if (
          command === 'submit_prompt' &&
          typeof args === 'object' &&
          args !== null &&
          'requestId' in args &&
          typeof args.requestId === 'string'
        ) {
          return {
            request_id: args.requestId,
            runtime_phase: 'error',
            outcome: 'error',
            error_message: 'OpenCode provider authentication failed',
          }
        }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Trigger provider failure')
      getSendButton(container).click()
      await Promise.resolve()
    })

    expect(container.textContent).toContain('Response failed')
    expect(container.textContent).toContain('OpenCode provider authentication failed')
  })

  it('cancels an active stream with Escape and leaves no empty assistant bubble', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let finishPrompt: ((value: unknown) => void) | undefined
    let activeRequestId = ''
    let cancelledRequestId = ''
    window.__TAURI_INTERNALS__ = {
      listen: async (_event, handler) => {
        promptEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'fast',
            supported_response_profiles: ['fast'],
            capabilities: completeCapabilities(),
            prompt_cancellation_available: true,
          }
        }
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        if (command === 'submit_prompt') {
          if (
            typeof args === 'object' &&
            args !== null &&
            'requestId' in args &&
            typeof args.requestId === 'string'
          ) {
            activeRequestId = args.requestId
          }
          return new Promise((resolve) => {
            finishPrompt = resolve
          })
        }
        if (command === 'cancel_prompt') {
          if (
            typeof args === 'object' &&
            args !== null &&
            'requestId' in args &&
            typeof args.requestId === 'string'
          ) {
            cancelledRequestId = args.requestId
          }
          return null
        }
        return null
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Cancel this')
      getSendButton(container).click()
      await Promise.resolve()
    })
    expect(container.textContent).toContain('Stop')

    await act(async () => {
      window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }))
      await Promise.resolve()
    })
    expect(cancelledRequestId).toBe(activeRequestId)
    expect(container.textContent).toContain('Stopping')

    await act(async () => {
      promptEventHandler?.({
        payload: {
          request_id: activeRequestId,
          kind: 'cancelled',
          runtime_phase: 'sleeping',
        },
      })
      finishPrompt?.({
        request_id: activeRequestId,
        runtime_phase: 'sleeping',
        outcome: 'cancelled',
      })
      await Promise.resolve()
    })

    expect(container.querySelectorAll('.message--assistant')).toHaveLength(0)
    expect(container.textContent).toContain('Response cancelled')
    expect(container.textContent).not.toContain('Stopping')
  })

  it('stops from the Stop button and returns to sleeping after cancellation settlement', async () => {
    let finishPrompt: ((value: unknown) => void) | undefined
    let requestId = ''
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command, args) => {
        if (command === 'get_startup_state') return readyStartupState({ prompt_cancellation_available: true })
        if (command === 'get_assistant_settings') return defaultAssistantSettings()
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          return new Promise((resolve) => { finishPrompt = resolve })
        }
        if (command === 'cancel_prompt') return null
        return null
      },
    }
    const { container } = await renderApp()
    await act(async () => { setTextAreaValue(getComposer(container), 'stop me'); getSendButton(container).click(); await Promise.resolve() })
    const stopButton = Array.from(container.querySelectorAll('button')).find((button) => button.textContent === 'Stop')
    expect(stopButton).toBeDefined()
    await act(async () => { stopButton?.click(); await Promise.resolve() })
    expect(container.textContent).toContain('Stopping')
    await act(async () => { finishPrompt?.({ request_id: requestId, runtime_phase: 'sleeping', outcome: 'cancelled' }); await Promise.resolve() })
    expect(container.textContent).not.toContain('Stopping')
    await act(async () => { await Promise.resolve(); await Promise.resolve(); })
    await act(async () => { setTextAreaValue(getComposer(container), 'another prompt'); await Promise.resolve(); await Promise.resolve(); })
    expect(getSendButton(container).disabled).toBe(false)
  })

  it('recovers a rejected Stop request and permits its later completion', async () => {
    let finishPrompt: ((value: unknown) => void) | undefined
    let requestId = ''
    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command, args) => {
        if (command === 'get_startup_state') return readyStartupState({ prompt_cancellation_available: true })
        if (command === 'get_assistant_settings') return defaultAssistantSettings()
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          return new Promise((resolve) => { finishPrompt = resolve })
        }
        if (command === 'cancel_prompt') throw new Error('cancel rejected')
        return null
      },
    }
    const { container } = await renderApp()
    await act(async () => { setTextAreaValue(getComposer(container), 'finish me'); getSendButton(container).click(); await Promise.resolve() })
    const stopButton = Array.from(container.querySelectorAll('button')).find((button) => button.textContent === 'Stop')
    await act(async () => { stopButton?.click(); await Promise.resolve() })
    await act(async () => { await Promise.resolve(); await Promise.resolve() })
    expect(container.textContent).toContain('Executing prompt')
    await act(async () => { finishPrompt?.({ request_id: requestId, runtime_phase: 'sleeping', outcome: 'completed' }); await Promise.resolve() })
    expect(container.textContent).not.toContain('Executing prompt')
    await act(async () => { setTextAreaValue(getComposer(container), 'another prompt'); await Promise.resolve(); await Promise.resolve() })
    expect(getSendButton(container).disabled).toBe(false)
  })

  it('uses a correlated stream error when the final result has no message', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    window.__TAURI_INTERNALS__ = {
      listen: async (_event, handler) => {
        promptEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        if (command === 'get_startup_state') {
          return {
            kind: 'ready',
            cue_asset_paths: {
              start_listening: 'resources/start-listening.wav',
              stop_listening: 'resources/stop-listening.wav',
            },
            runtime_phase: 'sleeping',
            voice_input_available: false,
            voice_input_error: null,
            silence_timeout_ms: 1500,
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-quality')

        expect(command).toBe('submit_prompt')
        const requestId = (args as { requestId: string }).requestId
        promptEventHandler?.({
          payload: {
            request_id: requestId,
            kind: 'error',
            message: 'Provider failed',
          },
        })
        return {
          request_id: requestId,
          runtime_phase: 'error',
          outcome: 'error',
          error_message: null,
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

    expect(container.textContent).toContain('Bad prompt')
    expect(container.textContent).toContain('Response failed')
    expect(container.textContent).toContain('Provider failed')
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
            capabilities: completeCapabilities(),
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
            capabilities: completeCapabilities(),
         }
      },
    }

    const { container } = await renderApp()

    expect(container.textContent).toContain('Model loading: Loading local Gemma model...')

    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 600))
    })

    expect(container.textContent).not.toContain('Startup ready: runtime=sleeping')
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
            capabilities: completeCapabilities(),
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

  it('keeps polling ready state until a warming capability becomes available', async () => {
    let startupCalls = 0
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command !== 'get_startup_state') throw new Error(`unexpected command: ${command}`)
        startupCalls += 1
        return readyStartupState({
          capabilities: completeCapabilities({
            qwen_prediction: startupCalls === 1 ? 'warming' : 'available',
          }),
        })
      },
    }

    await renderApp()
    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 600))
    })

    expect(startupCalls).toBe(2)
  })

  it('surfaces a capability that fails after ready state was first published', async () => {
    let startupCalls = 0
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command !== 'get_startup_state') throw new Error(`unexpected command: ${command}`)
        startupCalls += 1
        return readyStartupState({
          capabilities: completeCapabilities({
            qwen_prediction: startupCalls === 1 ? 'warming' : 'failed',
          }).map((capability) => capability['id'] === 'qwen_prediction' && startupCalls > 1
            ? { ...capability, reason: 'completion startup failed' }
            : capability),
        })
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 600))
    })

    expect(container.textContent).toContain('completion startup failed')
  })

  it('continues monitoring a capability after it becomes available', async () => {
    let startupCalls = 0
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command !== 'get_startup_state') throw new Error(`unexpected command: ${command}`)
        startupCalls += 1
        const state = startupCalls === 1 ? 'warming' : startupCalls === 2 ? 'available' : 'failed'
        return readyStartupState({
          capabilities: completeCapabilities({ qwen_prediction: state }).map((capability) =>
            capability['id'] === 'qwen_prediction' && state === 'failed'
              ? { ...capability, reason: 'completion worker stopped' }
              : capability),
        })
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 1_700))
    })

    expect(startupCalls).toBe(3)
    expect(container.textContent).toContain('completion worker stopped')
  })

  it('keeps an active prompt tracked across settled capability polling', async () => {
    let startupCalls = 0
    let submitCalls = 0
    let requestId = ''
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let finishPrompt: ((value: unknown) => void) | undefined
    window.__TAURI_INTERNALS__ = {
      listen: async (_event, handler) => {
        promptEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        if (command === 'get_startup_state') {
          startupCalls += 1
          return readyStartupState({ voice_input_available: false })
        }
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        if (command === 'submit_prompt') {
          submitCalls += 1
          requestId = (args as { requestId: string }).requestId
          return new Promise((resolve) => {
            finishPrompt = resolve
          })
        }
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Long-running prompt')
      getSendButton(container).click()
      await Promise.resolve()
    })

    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 1_100))
      setTextAreaValue(getComposer(container), 'Overlapping prompt')
    })

    expect(startupCalls).toBeGreaterThanOrEqual(2)
    expect(getSendButton(container).disabled).toBe(true)
    await act(async () => {
      getComposer(container).dispatchEvent(
        new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }),
      )
      await Promise.resolve()
    })
    expect(submitCalls).toBe(1)

    await act(async () => {
      promptEventHandler?.({
        payload: { request_id: requestId, kind: 'text', text: 'Original response' },
      })
      finishPrompt?.({
        request_id: requestId,
        runtime_phase: 'sleeping',
        outcome: 'completed',
      })
      await Promise.resolve()
    })

    expect(container.textContent).toContain('Original response')
  })

  it.each(['listening', 'processing'] as const)(
    'does not regress live %s state during capability polling',
    async (runtimePhase) => {
      let startupCalls = 0
      let submitCalls = 0
      window.__TAURI_INTERNALS__ = {
        invoke: async (command) => {
          if (command === 'get_startup_state') {
            startupCalls += 1
            return readyStartupState({
              runtime_phase: startupCalls === 1 ? runtimePhase : 'sleeping',
              voice_input_available: false,
              capabilities: completeCapabilities({
                qwen_prediction: startupCalls === 1 ? 'warming' : 'available',
              }),
            })
          }
          if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
          if (command === 'submit_prompt') {
            submitCalls += 1
            return null
          }
          throw new Error(`unexpected command: ${command}`)
        },
      }

      const { container } = await renderApp()
      await act(async () => {
        setTextAreaValue(getComposer(container), 'Typed while voice is active')
        await new Promise((resolve) => window.setTimeout(resolve, 600))
      })

      expect(startupCalls).toBe(2)
      expect(getSendButton(container).disabled).toBe(true)
      await act(async () => {
        getComposer(container).dispatchEvent(
          new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }),
        )
        await Promise.resolve()
      })
      expect(submitCalls).toBe(0)
    },
  )

  it.each(['listening', 'processing'] as const)(
    'preserves live %s state through a transient startup poll error',
    async (runtimePhase) => {
      let startupCalls = 0
      let submitCalls = 0
      window.__TAURI_INTERNALS__ = {
        invoke: async (command) => {
          if (command === 'get_startup_state') {
            startupCalls += 1
            if (startupCalls === 2) {
              return { kind: 'error', message: 'transient startup poll failure' }
            }
            return readyStartupState({
              runtime_phase: startupCalls === 1 ? runtimePhase : 'sleeping',
              voice_input_available: false,
              capabilities: completeCapabilities({
                qwen_prediction: startupCalls === 1 ? 'warming' : 'available',
              }),
            })
          }
          if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
          if (command === 'submit_prompt') {
            submitCalls += 1
            return null
          }
          throw new Error(`unexpected command: ${command}`)
        },
      }

      const { container } = await renderApp()
      await act(async () => {
        setTextAreaValue(getComposer(container), 'Typed during startup recovery')
        await new Promise((resolve) => window.setTimeout(resolve, 1_600))
      })

      expect(startupCalls).toBe(3)
      expect(getSendButton(container).disabled).toBe(true)
      await act(async () => {
        getComposer(container).dispatchEvent(
          new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }),
        )
        await Promise.resolve()
      })
      expect(submitCalls).toBe(0)
    },
  )

  it('plays the configured start-listening cue path from startup state', async () => {
    const stop = vi.fn()
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
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

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })

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
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-quality')

        if (command === 'ingest_audio_frame') {
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

    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })

    expect(playedSources).toEqual(['test-assets/configured-start.mp3'])
    nowMs = 2_000

    await act(async () => {
      await onFrame?.([0.001, -0.001, 0.001, -0.001])
      await Promise.resolve()
      await new Promise((resolve) => setTimeout(resolve, 0))
    })

    expect(playedSources).toEqual([
      'test-assets/configured-start.mp3',
      'test-assets/configured-stop.mp3',
    ])
    expect(container.textContent).not.toContain('transcription_ready:\n3200 samples captured')
  })

  it('holds a wake confidence peak for one second before showing the latest lower score', async () => {
    vi.useFakeTimers()
    const stop = vi.fn()
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    let confidence: number | null = 0.00114
    let runtimePhase: 'sleeping' | 'listening' = 'sleeping'

    class FakeAudio {
      play(): Promise<void> {
        return Promise.resolve()
      }
    }

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
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-quality')

        if (command === 'ingest_audio_frame') {
          return {
            runtime_phase: runtimePhase,
            transcription_ready_samples: null,
            transcript_text: null,
            last_activity_ms: Date.now(),
            capturing_utterance: true,
            preroll_samples: 0,
            utterance_samples: 3,
            telemetry: {
              wake_detected_ms: null,
               wake_confidence: confidence,
            },
          }
        }

        if (command === 'ingest_audio_frame') {
          return { runtime_phase: 'sleeping', last_activity_ms: null, transcription_ready_samples: null, transcript_text: null, capturing_utterance: false, preroll_samples: 0, utterance_samples: 0 }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    expect(container.textContent).not.toContain('wake: 0.00%')

    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })

    expect(container.textContent).toContain('wake: 0.11%')

    confidence = 0.8
    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })
    expect(container.textContent).toContain('wake: 80.00%')

    confidence = null
    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })
    expect(container.textContent).not.toContain('wake:')

    confidence = 0.2
    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })
    expect(container.textContent).toContain('wake: 20.00%')

    confidence = 0.3
    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
      await vi.advanceTimersByTimeAsync(999)
    })
    expect(container.textContent).toContain('wake: 30.00%')

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1)
    })
    expect(container.textContent).toContain('wake: 30.00%')

    confidence = 0.9
    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })
    expect(container.textContent).toContain('wake: 90.00%')

    confidence = 0.1
    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
      await vi.advanceTimersByTimeAsync(1_000)
    })
    expect(container.textContent).toContain('wake: 10.00%')

    runtimePhase = 'listening'
    confidence = 0.99
    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })
    expect(container.textContent).not.toContain('wake:')

    confidence = 0.88
    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })
    expect(container.textContent).not.toContain('wake:')

    runtimePhase = 'sleeping'
    confidence = 0
    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
      await Promise.resolve()
    })
    expect(container.textContent).toContain('wake: 0.00%')
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
            capabilities: completeCapabilities(),
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
    expect(container.textContent).not.toContain('live_audio:\ndefault microphone started')

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
    expect(container.textContent).not.toContain('live_audio:\ndefault microphone stopped')
  })

  it('ignores stale live frames after profile switch stops capture', async () => {
    const originalStop = vi.fn()
    const replacementStop = vi.fn()
    let originalFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    let replacementFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    let sourceCount = 0
    const invokedCommands: string[] = []
    let selectedProfile: 'fast' | 'quality' = 'fast'

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      sourceCount += 1
      if (sourceCount === 1) {
        originalFrame = options.onFrame
        return { stop: originalStop }
      }
      replacementFrame = options.onFrame
      return { stop: replacementStop }
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
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'switch_response_profile') {
          expect(args).toEqual({ profile: 'quality' })
          selectedProfile = 'quality'
          return {
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
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

        if (command === 'ingest_audio_frame') {
          return { runtime_phase: 'sleeping', last_activity_ms: null, transcription_ready_samples: null, transcript_text: null, capturing_utterance: false, preroll_samples: 0, utterance_samples: 0 }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = await getResponseProfileSelect(container)

    await act(async () => {
      setSelectValue(select, 'quality')
      await Promise.resolve()
    })

    expect(originalStop).toHaveBeenCalledTimes(1)
    expect(startLiveAudioSourceMock.mock.calls.length).toBeGreaterThan(0)

    await act(async () => {
      await originalFrame?.([0.2, -0.2, 0.2, -0.2])
      await replacementFrame?.([0.2, -0.2, 0.2, -0.2])
      await Promise.resolve()
    })

    expect(invokedCommands).toContain('switch_response_profile')
    expect(invokedCommands.filter((command) => command === 'ingest_audio_frame')).toHaveLength(1)
  })

  it('stops live audio and rejects stale frames before resetting the session', async () => {
    const stop = vi.fn()
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let resolveIngestFrame: () => void = () => undefined

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
    })

    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        invokedCommands.push(command)
        if (command === 'get_startup_state') {
          return readyStartupState({ voice_input_available: true })
        }
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        if (command === 'reset_session') return null
        if (command === 'ingest_audio_frame') {
          await new Promise<void>((resolve) => { resolveIngestFrame = resolve })
          return {
            runtime_phase: 'sleeping', transcription_ready_samples: null, transcript_text: null,
            last_activity_ms: null, capturing_utterance: false, preroll_samples: 0, utterance_samples: 0,
          }
        }
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    expect(onFrame).not.toBeNull()
    const frameHandler = onFrame as unknown as (frame: readonly number[]) => Promise<void> | void

    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })

    const framePromise = frameHandler([0.2, -0.2, 0.2, -0.2])
    await Promise.resolve()
    expect(invokedCommands).toContain('ingest_audio_frame')

    await act(async () => {
      getButtonByLabel(container, 'Reset Session').click()
      await Promise.resolve()
    })

    expect(invokedCommands).not.toContain('reset_session')
    await act(async () => {
      resolveIngestFrame()
      await framePromise
      await new Promise((resolve) => setTimeout(resolve, 20))
    })

    expect(stop).toHaveBeenCalledTimes(1)
    expect(invokedCommands.indexOf('ingest_audio_frame')).toBeLessThan(invokedCommands.indexOf('reset_session'))
    expect(invokedCommands.filter((command) => command === 'ingest_audio_frame')).toHaveLength(1)
  })

  it('correlates partial transcription to the active listening session', async () => {
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    let partialEventHandler: ((event: { payload: unknown }) => void) | undefined
    let ingestCount = 0
    class FakeAudio {
      play(): Promise<void> { return Promise.resolve() }
    }
    Object.defineProperty(globalThis, 'Audio', { configurable: true, value: FakeAudio })
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop: vi.fn() }
    })
    window.__TAURI_INTERNALS__ = {
      listen: async (event, handler) => {
        if (event === 'partial-transcription-event') partialEventHandler = handler
        return () => undefined
      },
      invoke: async (command) => {
        if (command === 'get_startup_state') return readyStartupState({ voice_input_available: true })
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        if (command === 'ingest_audio_frame') {
          ingestCount += 1
          return {
            runtime_phase: ingestCount === 2 ? 'sleeping' : 'listening',
            transcription_ready_samples: null,
            transcript_text: null,
            last_activity_ms: null,
            capturing_utterance: ingestCount !== 2,
            preroll_samples: 0,
            utterance_samples: 0,
          }
        }
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const emitPartial = async (sessionId: number, revision: number, text: string) => {
      await act(async () => {
        partialEventHandler?.({ payload: { session_id: sessionId, revision, text } })
        await Promise.resolve()
      })
    }
    await emitPartial(1, 1, 'before listening')
    expect(container.textContent).not.toContain('before listening')

    const frame = onFrame as unknown as (samples: readonly number[]) => Promise<void> | void
    await act(async () => { await frame([0.2, -0.2]); await Promise.resolve() })
    await emitPartial(1, 1, 'current partial')
    expect(container.textContent).toContain('current partial')
    await emitPartial(2, 2, 'wrong session')
    await emitPartial(1, 1, 'stale revision')
    expect(container.textContent).not.toContain('wrong session')
    expect(container.textContent).not.toContain('stale revision')

    await act(async () => { await frame([0.2, -0.2]); await Promise.resolve() })
    await emitPartial(1, 2, 'after listening')
    expect(container.textContent).not.toContain('after listening')

    await act(async () => { await frame([0.2, -0.2]); await Promise.resolve() })
    await emitPartial(1, 3, 'previous session')
    await emitPartial(2, 3, 'next session')
    expect(container.textContent).not.toContain('previous session')
    expect(container.textContent).toContain('next session')
  })

  it('invalidates a pending prompt before reset and ignores late output', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let partialEventHandler: ((event: { payload: unknown }) => void) | undefined
    let activeRequestId = ''
    let resolveReset: () => void = () => undefined
    const invokedCommands: string[] = []

    window.__TAURI_INTERNALS__ = {
      listen: async (event, handler) => {
        if (event === 'prompt-execution-event') promptEventHandler = handler
        if (event === 'partial-transcription-event') partialEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        invokedCommands.push(command)
        if (command === 'get_startup_state') return readyStartupState({ prompt_cancellation_available: true })
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-fast')
        if (command === 'submit_prompt') {
          activeRequestId = (args as { requestId: string }).requestId
          return new Promise(() => undefined)
        }
        if (command === 'cancel_prompt') return null
        if (command === 'reset_session') {
          await new Promise<void>((resolve) => { resolveReset = resolve })
          return null
        }
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Reset this prompt')
      getSendButton(container).click()
      await Promise.resolve()
    })
    expect(activeRequestId).not.toBe('')

    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })
    await act(async () => {
      getButtonByLabel(container, 'Reset Session').click()
      await Promise.resolve()
    })
    getButtonByLabel(container, 'Reset Session').click()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Blocked during reset')
      getComposer(container).dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }))
      await Promise.resolve()
    })

    expect(invokedCommands).toContain('cancel_prompt')
    expect(invokedCommands).toContain('reset_session')
    expect(invokedCommands.filter((command) => command === 'reset_session')).toHaveLength(1)
    expect(invokedCommands.filter((command) => command === 'submit_prompt')).toHaveLength(1)
    expect(invokedCommands.indexOf('cancel_prompt')).toBeLessThan(invokedCommands.indexOf('reset_session'))
    promptEventHandler?.({ payload: { request_id: activeRequestId, kind: 'text', text: 'late output' } })
    promptEventHandler?.({ payload: { request_id: activeRequestId, kind: 'error', message: 'late failure' } })
    partialEventHandler?.({ payload: { session_id: 999, revision: 1, text: 'late partial' } })
    expect(container.textContent).not.toContain('late output')
    expect(container.textContent).not.toContain('late failure')
    expect(container.textContent).not.toContain('late partial')

    await act(async () => {
      resolveReset()
      await Promise.resolve()
    })
    expect(container.textContent).not.toContain('Reset this prompt')
    expect(container.textContent).not.toContain('late output')
    expect(container.textContent).not.toContain('late failure')
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Prompt after reset')
    })
    expect(getSendButton(container).disabled).toBe(false)
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
            capabilities: completeCapabilities(),
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
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'set_assistant_settings') {
          return (args as { settings: unknown }).settings
        }

        if (command === 'set_assistant_settings') {
          return (args as { settings: unknown }).settings
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = await getResponseProfileSelect(container)

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
    expect(container.textContent).not.toContain('Reset to idle')
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
            capabilities: completeCapabilities(),
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
            capabilities: completeCapabilities(),
          }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = await getResponseProfileSelect(container)

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
            capabilities: completeCapabilities(),
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
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'set_assistant_settings') {
          return (args as { settings: unknown }).settings
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = await getResponseProfileSelect(container)

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
    const staleStop = vi.fn()
    const replacementStop = vi.fn()
    let delayedStartCount = 0
    let resolveStaleStart: (source: { stop: () => void }) => void = () => { throw new Error('Expected stale resolver') }
    let resolveReplacementStart: (source: { stop: () => void }) => void = () => { throw new Error('Expected replacement resolver') }
    let staleFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    let replacementFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let selectedProfile: 'fast' | 'quality' = 'fast'

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      delayedStartCount += 1
      if (delayedStartCount === 1) {
        staleFrame = options.onFrame
        return await new Promise((resolve) => { resolveStaleStart = resolve })
      }
      replacementFrame = options.onFrame
      return await new Promise((resolve) => { resolveReplacementStart = resolve })
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
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'switch_response_profile') {
          expect(args).toEqual({ profile: 'quality' })
          selectedProfile = 'quality'
          return {
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'ingest_audio_frame') {
          return { runtime_phase: 'sleeping', last_activity_ms: null, transcription_ready_samples: null, transcript_text: null, capturing_utterance: false, preroll_samples: 0, utterance_samples: 0 }
        }
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = await getResponseProfileSelect(container)

    await act(async () => {
      setSelectValue(select, 'quality')
      await Promise.resolve()
    })

    expect(delayedStartCount).toBe(1)

    await act(async () => {
      resolveStaleStart({ stop: staleStop })
      await Promise.resolve()
    })

    expect(invokedCommands).toContain('switch_response_profile')
    expect(invokedCommands.filter((command) => command === 'ingest_audio_frame')).toHaveLength(0)
    expect(staleStop).toHaveBeenCalledTimes(1)
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); await Promise.resolve() })
    expect(delayedStartCount).toBe(2)
    expect(container.textContent).not.toContain('live_audio:\ndefault microphone started')
    await act(async () => {
      await staleFrame?.([0.2, -0.2])
      await Promise.resolve()
    })
    expect(invokedCommands.filter((command) => command === 'ingest_audio_frame')).toHaveLength(0)
    await act(async () => {
      resolveReplacementStart({ stop: replacementStop })
      await Promise.resolve()
      await replacementFrame?.([0.2, -0.2])
      await Promise.resolve()
    })
    expect(invokedCommands.filter((command) => command === 'ingest_audio_frame')).toHaveLength(1)
  })

  it('ignores stale delayed microphone start rejection after profile switch', async () => {
    let delayedStartPending = false
    let startCount = 0
    let rejectLiveAudioSource: (error: unknown) => void = () => {
      throw new Error('Expected delayed microphone start rejector')
    }
    let replacementFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let selectedProfile: 'fast' | 'quality' = 'fast'

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      startCount += 1
      if (startCount === 1) {
        return await new Promise((_resolve, reject) => {
          delayedStartPending = true
          rejectLiveAudioSource = reject
        })
      }
      replacementFrame = options.onFrame
      return { stop: vi.fn() }
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
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'switch_response_profile') {
          expect(args).toEqual({ profile: 'quality' })
          selectedProfile = 'quality'
          return {
            selected_response_profile: 'quality',
            supported_response_profiles: ['fast', 'quality'],
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'ingest_audio_frame') {
          return { runtime_phase: 'sleeping', last_activity_ms: null, transcription_ready_samples: null, transcript_text: null, capturing_utterance: false, preroll_samples: 0, utterance_samples: 0 }
        }

        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const select = await getResponseProfileSelect(container)

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
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0))
      await Promise.resolve()
    })
    expect(startCount).toBe(2)
    await act(async () => {
      await replacementFrame?.([0.2, -0.2])
      await Promise.resolve()
    })
    expect(invokedCommands.filter((command) => command === 'ingest_audio_frame')).toHaveLength(1)
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
            capabilities: completeCapabilities(),
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

    expect(nonDiagnosticCommands(invokedCommands)).toEqual([
      'get_startup_state',
      'ingest_audio_frame',
      'ingest_audio_frame',
      'mark_silence',
    ])
    expect(container.textContent).not.toContain('transcription_ready:\n3200 samples captured')
  })

  it('submits the transcribed voice prompt after silence and returns to wake-word waiting', async () => {
    const stop = vi.fn()
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
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
      listen: async (_event, handler) => {
        promptEventHandler = handler
        return () => undefined
      },
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
            capabilities: completeCapabilities(),
          }
        }

        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-quality')

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
          expect(args).toMatchObject({ requestId: expect.any(String), prompt: 'Open the pull request' })
          promptEventHandler?.({
            payload: { request_id: (args as { requestId: string }).requestId, kind: 'text', text: 'Voice execution response' },
          })

          return {
            request_id: (args as { requestId: string }).requestId,
            outcome: 'completed',
            error_message: null,
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

    expect(nonDiagnosticCommands(invokedCommands)).toEqual([
      'get_startup_state',
      'ingest_audio_frame',
      'ingest_audio_frame',
      'mark_silence',
      'submit_prompt',
    ])
    expect(container.textContent).not.toContain('transcript:\nOpen the pull request')
    expect(container.textContent).toContain('Open the pull request')
    expect(container.textContent).toContain('Voice execution response')
    expect(container.textContent).toContain('Stop mic')
  })

  it('does not submit a voice transcript through an unavailable Instant provider', async () => {
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let nowMs = 1_000

    class FakeAudio {
      play(): Promise<void> {
        return Promise.resolve()
      }
    }

    Date.now = () => nowMs
    Object.defineProperty(globalThis, 'Audio', { configurable: true, value: FakeAudio })
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop: vi.fn() }
    })
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        invokedCommands.push(command)
        if (command === 'get_startup_state') {
          return readyStartupState({
            capabilities: completeCapabilities({ opencode: 'unavailable' }),
          })
        }
        if (command === 'get_assistant_settings') {
          return {
            instant: 'opencode-sol-high', deep: 'opencode-sol-high', review: 'opencode-sol-high',
            deep_enabled: false, review_enabled: false, prefetch: false, completion: false,
          }
        }
        if (command === 'ingest_audio_frame') {
          return {
            runtime_phase: 'listening', transcription_ready_samples: null, transcript_text: null,
            last_activity_ms: 1_000, capturing_utterance: true, preroll_samples: 4, utterance_samples: 4,
          }
        }
        if (command === 'mark_silence') {
          return {
            runtime_phase: 'processing', transcription_ready_samples: 3200,
            transcript_text: 'Do not send this transcript', last_activity_ms: null,
            capturing_utterance: false, preroll_samples: 4, utterance_samples: 0,
          }
        }
        if (command === 'submit_prompt') throw new Error('voice prompt bypassed provider gate')
        throw new Error(`unexpected command: ${command}`)
      },
    }

    await renderApp()
    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
    })
    nowMs = 3_600
    await act(async () => {
      await onFrame?.([0.001, -0.001, 0.001, -0.001])
    })

    expect(invokedCommands).toContain('mark_silence')
    expect(invokedCommands).not.toContain('submit_prompt')
  })

  it('keeps typed and voice submission fail-closed when native settings fail to load', async () => {
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let nowMs = 1_000

    class FakeAudio {
      play(): Promise<void> {
        return Promise.resolve()
      }
    }

    Date.now = () => nowMs
    Object.defineProperty(globalThis, 'Audio', { configurable: true, value: FakeAudio })
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop: vi.fn() }
    })
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        invokedCommands.push(command)
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'get_assistant_settings') throw new Error('settings are unreadable')
        if (command === 'ingest_audio_frame') {
          return {
            runtime_phase: 'listening', transcription_ready_samples: null, transcript_text: null,
            last_activity_ms: 1_000, capturing_utterance: true, preroll_samples: 4, utterance_samples: 4,
          }
        }
        if (command === 'mark_silence') {
          return {
            runtime_phase: 'processing', transcription_ready_samples: 3200,
            transcript_text: 'Blocked voice prompt', last_activity_ms: null,
            capturing_utterance: false, preroll_samples: 4, utterance_samples: 0,
          }
        }
        if (command === 'submit_prompt') throw new Error('settings failure gate was bypassed')
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Blocked typed prompt')
    })
    expect(getSendButton(container).disabled).toBe(true)
    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
    })
    nowMs = 3_600
    await act(async () => {
      await onFrame?.([0.001, -0.001, 0.001, -0.001])
    })
    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })

    expect(invokedCommands).not.toContain('submit_prompt')
    expect(invokedCommands).not.toContain('request_completion')
    expect(container.textContent).toContain('Assistant settings unavailable: settings are unreadable')
    expect(Array.from(container.querySelectorAll<HTMLSelectElement>('.settings-panel__assistant select')).every((control) => control.disabled)).toBe(true)
    expect(Array.from(container.querySelectorAll<HTMLInputElement>('.settings-panel__assistant input')).every((control) => control.disabled)).toBe(true)
  })

  it('blocks typed and voice prompts when the selected local profile is not loaded', async () => {
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    const invokedCommands: string[] = []
    let nowMs = 1_000

    class FakeAudio {
      play(): Promise<void> {
        return Promise.resolve()
      }
    }

    Date.now = () => nowMs
    Object.defineProperty(globalThis, 'Audio', { configurable: true, value: FakeAudio })
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop: vi.fn() }
    })
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        invokedCommands.push(command)
        if (command === 'get_startup_state') return readyStartupState({ selected_response_profile: 'fast' })
        if (command === 'get_assistant_settings') return defaultAssistantSettings('local-quality')
        if (command === 'ingest_audio_frame') {
          return {
            runtime_phase: 'listening', transcription_ready_samples: null, transcript_text: null,
            last_activity_ms: 1_000, capturing_utterance: true, preroll_samples: 4, utterance_samples: 4,
          }
        }
        if (command === 'mark_silence') {
          return {
            runtime_phase: 'processing', transcription_ready_samples: 3200,
            transcript_text: 'Mismatched voice prompt', last_activity_ms: null,
            capturing_utterance: false, preroll_samples: 4, utterance_samples: 0,
          }
        }
        if (command === 'submit_prompt') throw new Error('profile gate was bypassed')
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'Mismatched typed prompt')
    })
    expect(getSendButton(container).disabled).toBe(true)
    expect(container.textContent).toContain('selected local model profile is not loaded')
    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
    })
    nowMs = 3_600
    await act(async () => {
      await onFrame?.([0.001, -0.001, 0.001, -0.001])
    })

    expect(invokedCommands).not.toContain('submit_prompt')
  })

  it('uses TTS and assistant settings updated after the microphone starts', async () => {
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let resolveSettings: (settings: unknown) => void = () => undefined
    const pendingSettings = new Promise<unknown>((resolve) => { resolveSettings = resolve })
    const invokedCommands: string[] = []
    let nowMs = 1_000

    class FakeAudio {
      play(): Promise<void> {
        return Promise.resolve()
      }
    }

    class FakeAudioContext {
      destination = {} as AudioDestinationNode
      state: AudioContextState = 'running'

      createBuffer(): AudioBuffer {
        return { copyToChannel: () => {} } as unknown as AudioBuffer
      }

      createBufferSource(): AudioBufferSourceNode {
        return {
          buffer: null,
          connect: () => {},
          onended: null,
          start: function start(this: AudioBufferSourceNode) {
            this.onended?.(new Event('ended'))
          },
        } as unknown as AudioBufferSourceNode
      }

      createGain(): GainNode {
        return {
          gain: { value: 1 } as AudioParam,
          connect: () => {},
        } as unknown as GainNode
      }

      async resume(): Promise<void> {}
      async close(): Promise<void> {}
    }

    Date.now = () => nowMs
    Object.defineProperty(globalThis, 'Audio', { configurable: true, value: FakeAudio })
    Object.defineProperty(globalThis, 'AudioContext', { configurable: true, value: FakeAudioContext })
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop: vi.fn() }
    })
    window.__TAURI_INTERNALS__ = {
      listen: async (event, handler) => {
        if (event === 'prompt-execution-event') promptEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        invokedCommands.push(command)
        if (command === 'get_startup_state') return readyStartupState({ tts_enabled: false })
        if (command === 'get_assistant_settings') return pendingSettings
        if (command === 'set_tts_enabled') return { enabled: true, sample_rate_hz: 22050 }
        if (command === 'ingest_audio_frame') {
          return {
            runtime_phase: 'listening', transcription_ready_samples: null, transcript_text: null,
            last_activity_ms: 1_000, capturing_utterance: true, preroll_samples: 4, utterance_samples: 4,
          }
        }
        if (command === 'mark_silence') {
          return {
            runtime_phase: 'processing', transcription_ready_samples: 3200,
            transcript_text: 'Use current voice settings', last_activity_ms: null,
            capturing_utterance: false, preroll_samples: 4, utterance_samples: 0,
          }
        }
        if (command === 'submit_prompt') {
          const requestId = (args as { requestId: string }).requestId
          promptEventHandler?.({
            payload: { request_id: requestId, kind: 'text', text: 'Current settings response' },
          })
          return {
            request_id: requestId, outcome: 'completed', error_message: null, runtime_phase: 'sleeping',
          }
        }
        if (command === 'synthesize_local_tts') {
          return { pcm_f32: [0, 0.1], sample_rate_hz: 22050, duration_ms: 1 }
        }
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    expect(startLiveAudioSourceMock).toHaveBeenCalledTimes(1)
    await act(async () => {
      getTtsToggle(container).click()
      resolveSettings({
        instant: 'local-fast', deep: 'opencode-sol-high', review: 'opencode-sol-high',
        deep_enabled: true, review_enabled: false, prefetch: false, completion: false,
      })
      await Promise.resolve()
    })
    await act(async () => {
      await onFrame?.([0.04, -0.04, 0.04, -0.04])
    })
    nowMs = 3_600
    await act(async () => {
      await onFrame?.([0.001, -0.001, 0.001, -0.001])
      await new Promise((resolve) => setTimeout(resolve, 0))
    })

    expect(invokedCommands).toContain('synthesize_local_tts')
    expect(container.querySelector('[aria-label="Deep status: stale"]')).not.toBeNull()
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
            capabilities: completeCapabilities(),
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

    expect(nonDiagnosticCommands(invokedCommands)).toEqual([
      'get_startup_state',
      'ingest_audio_frame',
      'ingest_audio_frame',
    ])

    nowMs = 4_000

    await act(async () => {
      await onFrame?.([0.001, -0.001, 0.001, -0.001])
      await Promise.resolve()
    })

    expect(nonDiagnosticCommands(invokedCommands)).toEqual([
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
            capabilities: completeCapabilities(),
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

    expect(nonDiagnosticCommands(invokedCommands)).toEqual([
      'get_startup_state',
      'ingest_audio_frame',
      'ingest_audio_frame',
      'mark_silence',
    ])
    expect(container.textContent).not.toContain(
      'Runtime control error (mark_silence): utterance transcription failed: InvalidTranscript(EmptyText)',
    )
    expect(container.textContent).toContain('Runtime control failed')
    expect(container.textContent).toContain('Stop mic')
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
            capabilities: completeCapabilities(),
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

    expect(nonDiagnosticCommands(invokedCommands)).toEqual([
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

    expect(nonDiagnosticCommands(invokedCommands)).toEqual([
      'get_startup_state',
      'ingest_audio_frame',
      'ingest_audio_frame',
      'mark_silence',
    ])
  })

  it('hides microphone capture errors from chat without changing the backend contract', async () => {
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
            capabilities: completeCapabilities(),
        }
      },
    }

    const { container } = await renderApp()

    expect(container.textContent).toContain('Microphone unavailable')
    expect(container.textContent).not.toContain('live_audio_error:\nPermission denied')
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
            capabilities: completeCapabilities(),
        }
      },
    }

    const { container } = await renderApp()

    expect(startLiveAudioSourceMock).toHaveBeenCalledTimes(1)
    expect(container.textContent).not.toContain('live_audio:\ndefault microphone started')
    expect(container.querySelector('details.shell__manual-controls')).toBeNull()
    expect(container.textContent).not.toContain('Stop listening and process')
  })

  it('persists and restarts capture with a selected microphone', async () => {
    const stop = vi.fn()
    startLiveAudioSourceMock.mockResolvedValue({ stop })
    listAudioInputDevicesMock.mockResolvedValue([
      { deviceId: 'studio-device', label: 'Studio Microphone' },
    ])
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'get_assistant_settings') return defaultAssistantSettings()
        if (command === 'get_ui_text_size') return 'medium'
        if (command === 'get_ui_theme') return 'dark'
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })
    const select = container.querySelector<HTMLSelectElement>('#audioInputDevice')
    expect(select).not.toBeNull()
    expect(select?.textContent).toContain('Studio Microphone')

    await act(async () => {
      setSelectValue(select as HTMLSelectElement, 'studio-device')
      await Promise.resolve()
    })

    expect(window.localStorage.getItem('voxgolem.audioInputDeviceId')).toBe('studio-device')
    expect(stop).toHaveBeenCalledTimes(1)
    expect(startLiveAudioSourceMock).toHaveBeenLastCalledWith(expect.objectContaining({
      deviceId: 'studio-device',
    }))

    await act(async () => {
      setSelectValue(select as HTMLSelectElement, '')
      await Promise.resolve()
    })
    expect(window.localStorage.getItem('voxgolem.audioInputDeviceId')).toBeNull()
    expect(startLiveAudioSourceMock).toHaveBeenLastCalledWith(expect.not.objectContaining({
      deviceId: expect.anything(),
    }))

    const firstRoot = mountedRoots.pop()
    await act(async () => {
      firstRoot?.unmount()
    })
    const remounted = await renderApp()
    await act(async () => {
      getButtonByLabel(remounted.container, 'Settings').click()
      await Promise.resolve()
    })
    expect(remounted.container.querySelector<HTMLSelectElement>('#audioInputDevice')?.value).toBe('')
    expect(startLiveAudioSourceMock).toHaveBeenLastCalledWith(expect.not.objectContaining({
      deviceId: expect.anything(),
    }))
  })

  it('retains an explicit microphone when fallback capture fails', async () => {
    window.localStorage.setItem('voxgolem.audioInputDeviceId', 'stale-device')
    startLiveAudioSourceMock.mockRejectedValue(new DOMException('permission denied', 'NotAllowedError'))
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_startup_state') return readyStartupState()
        throw new Error(`unexpected command: ${command}`)
      },
    }

    await renderApp()

    expect(window.localStorage.getItem('voxgolem.audioInputDeviceId')).toBe('stale-device')
  })

  it('ignores delayed fallback metadata from a microphone session that was replaced', async () => {
    const firstStop = vi.fn()
    let firstOptions: StartLiveAudioSourceOptions | undefined
    let callCount = 0
    startLiveAudioSourceMock.mockImplementation(async (options) => {
      callCount += 1
      if (callCount === 1) {
        firstOptions = options
        return { stop: firstStop }
      }
      return new Promise(() => undefined)
    })
    window.localStorage.setItem('voxgolem.audioInputDeviceId', 'first-device')
    listAudioInputDevicesMock.mockResolvedValue([
      { deviceId: 'first-device', label: 'First Microphone' },
      { deviceId: 'second-device', label: 'Second Microphone' },
    ])
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === 'get_startup_state') return readyStartupState()
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      getButtonByLabel(container, 'Settings').click()
      await Promise.resolve()
    })
    const select = container.querySelector<HTMLSelectElement>('#audioInputDevice')
    await act(async () => {
      setSelectValue(select as HTMLSelectElement, 'second-device')
      await Promise.resolve()
    })

    const delayedFirstOptions = firstOptions
    if (delayedFirstOptions !== undefined) {
      delayedFirstOptions.onSelectedDeviceFallback?.('first-device')
    }

    expect(select?.value).toBe('second-device')
    expect(window.localStorage.getItem('voxgolem.audioInputDeviceId')).toBe('first-device')
  })

  it('does not auto-start microphone capture when wake-word capability is unavailable', async () => {
    const wakeReason = 'wake word model is unavailable for this test'
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        expect(command).toBe('get_startup_state')
        return readyStartupState({
          voice_input_available: true,
          capabilities: completeCapabilities({ wake_word: 'unavailable' }).map((capability) =>
            capability['id'] === 'wake_word' ? { ...capability, reason: wakeReason } : capability,
          ),
        })
      },
    }

    const { container } = await renderApp()
    const startMic = getControlButton(container, 'Start mic')
    const autoStop = getAutoStopToggle(container)
    const describedBy = startMic.getAttribute('aria-describedby')
    const description = describedBy === null ? null : container.querySelector(`#${describedBy}`)

    expect(startLiveAudioSourceMock).not.toHaveBeenCalled()
    expect(startMic.disabled).toBe(true)
    expect(autoStop.disabled).toBe(true)
    expect(container.textContent).toContain(wakeReason)
    expect(describedBy).not.toBeNull()
    expect(description?.textContent).toContain(wakeReason)
    expect(autoStop.getAttribute('aria-describedby')).toBe(describedBy)
  })

  it('preserves an accepted completion prefetch through typed submission', async () => {
    let completionEventHandler: ((event: { payload: unknown }) => void) | undefined
    const invocations: Array<{ command: string, args?: unknown }> = []
    window.__TAURI_INTERNALS__ = {
      listen: async (event, handler) => {
        if (event === 'completion-event') completionEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        invocations.push({ command, args })
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'get_assistant_settings') {
          return {
            instant: 'local-fast', deep: 'opencode-sol-high', review: 'opencode-sol-high',
            deep_enabled: false, review_enabled: false, prefetch: true, completion: true,
          }
        }
        if (command === 'request_completion' || command === 'clear_completion') return null
        if (command === 'submit_prompt') {
          return {
            request_id: (args as { requestId: string }).requestId,
            outcome: 'completed', error_message: null, runtime_phase: 'sleeping',
          }
        }
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    const composer = getComposer(container)
    await act(async () => {
      setTextAreaValue(composer, 'draft')
      await Promise.resolve()
    })
    await act(async () => {
      completionEventHandler?.({
        payload: {
          source: 'typed', revision: 1, voice_session_id: null, suffix: ' completion',
        },
      })
      await Promise.resolve()
    })
    await act(async () => {
      composer.dispatchEvent(new KeyboardEvent('keydown', { key: 'Tab', bubbles: true }))
      await Promise.resolve()
    })

    expect(composer.value).toBe('draft completion')
    expect(invocations.filter(({ command }) => command === 'request_completion')).toHaveLength(1)

    await act(async () => {
      getSendButton(container).click()
      await Promise.resolve()
    })

    expect(invocations.filter(({ command }) => command === 'clear_completion')).toHaveLength(0)
    expect(invocations.find(({ command }) => command === 'submit_prompt')?.args).toMatchObject({
      prompt: 'draft completion',
      source: 'typed',
    })
  })

  it('ignores typed completion events from before prompt submission', async () => {
    let completionEventHandler: ((event: { payload: unknown }) => void) | undefined
    window.__TAURI_INTERNALS__ = {
      listen: async (event, handler) => {
        if (event === 'completion-event') completionEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        if (command === 'get_startup_state') return readyStartupState()
        if (command === 'get_assistant_settings') {
          return {
            instant: 'local-fast', deep: 'opencode-sol-high', review: 'opencode-sol-high',
            deep_enabled: false, review_enabled: false, prefetch: false, completion: true,
          }
        }
        if (command === 'request_completion' || command === 'clear_completion') return null
        if (command === 'submit_prompt') {
          return {
            request_id: (args as { requestId: string }).requestId,
            outcome: 'completed', error_message: null, runtime_phase: 'sleeping',
          }
        }
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      completionEventHandler?.({
        payload: {
          source: 'typed', revision: 0, voice_session_id: null, suffix: ' UNSOLICITED_COMPLETION',
        },
      })
      await Promise.resolve()
    })
    expect(container.textContent).not.toContain('UNSOLICITED_COMPLETION')
    await act(async () => {
      setTextAreaValue(getComposer(container), 'submit before completion')
    })
    await act(async () => {
      getSendButton(container).click()
      await Promise.resolve()
    })
    await act(async () => {
      completionEventHandler?.({
        payload: {
          source: 'typed', revision: 1, voice_session_id: null, suffix: ' STALE_COMPLETION',
        },
      })
      await Promise.resolve()
    })

    expect(container.textContent).not.toContain('STALE_COMPLETION')
    expect(getComposer(container).value).toBe('')
  })

  it('does not synthesize a pending response after TTS is disabled', async () => {
    let finishPrompt: ((value: unknown) => void) | undefined
    let promptRequestId = ''
    const invoked: string[] = []

    window.__TAURI_INTERNALS__ = {
      listen: async () => () => undefined,
      invoke: async (command, args) => {
        invoked.push(command)
        if (command === 'get_startup_state') return readyStartupState({ tts_enabled: false })
        if (command === 'set_tts_enabled') return { enabled: args && (args as { enabled: boolean }).enabled }
        if (command === 'submit_prompt') {
          promptRequestId = (args as { requestId: string }).requestId
          return new Promise((resolve) => { finishPrompt = resolve })
        }
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    await act(async () => { getTtsToggle(container).click(); await Promise.resolve() })
    await act(async () => {
      setTextAreaValue(getComposer(container), 'pending response')
      getSendButton(container).click()
      await Promise.resolve()
    })
    await act(async () => { getTtsToggle(container).click(); await Promise.resolve() })
    await act(async () => {
      finishPrompt?.({ request_id: promptRequestId, outcome: 'completed', runtime_phase: 'sleeping' })
      await Promise.resolve()
    })

    expect(invoked).not.toContain('synthesize_local_tts')
  })

  it('marks a Deep-only correction as Deep corrected without requiring Review', async () => {
    let promptEventHandler: ((event: { payload: unknown }) => void) | undefined
    let finishPrompt: ((value: unknown) => void) | undefined
    let requestId = ''
    let synthesizedText = ''
    window.__TAURI_INTERNALS__ = {
      listen: async (event, handler) => {
        if (event !== 'prompt-execution-event') return () => undefined
        promptEventHandler = handler
        return () => undefined
      },
      invoke: async (command, args) => {
        if (command === 'get_startup_state') return readyStartupState({ tts_enabled: true })
        if (command === 'get_assistant_settings') return {
          instant: 'local-fast', deep: 'opencode-sol-high', review: 'opencode-sol-high',
          deep_enabled: true, review_enabled: false, prefetch: false, completion: false,
        }
        if (command === 'submit_prompt') {
          requestId = (args as { requestId: string }).requestId
          return new Promise((resolve) => { finishPrompt = resolve })
        }
        if (command === 'synthesize_local_tts') {
          synthesizedText = (args as { text: string }).text
          return { pcm_f32: [0], sample_rate_hz: 22050, duration_ms: 1 }
        }
        throw new Error(`unexpected command: ${command}`)
      },
    }

    const { container } = await renderApp()
    await act(async () => {
      setTextAreaValue(getComposer(container), 'correct this')
      getSendButton(container).click()
      await Promise.resolve()
      await Promise.resolve()
      promptEventHandler?.({ payload: { request_id: requestId, kind: 'stage', stage: 'deep', status: 'running' } })
      promptEventHandler?.({ payload: { request_id: requestId, kind: 'sources', sources: [{ url: 'https://example.com/deep', title: 'Deep source' }] } })
      promptEventHandler?.({ payload: { request_id: requestId, kind: 'correction', stage: 'deep', text: 'Corrected answer', correction: 'Correction: Deep correction' } })
      await Promise.resolve()
    })

    expect(container.textContent).toContain('Deep')
    expect(container.textContent).toContain('Corrected answer')
    expect(synthesizedText).toBe('Deep correction')
    expect(container.querySelector('[aria-label="Deep status: corrected"]')).not.toBeNull()
    expect(container.querySelector('[aria-label="Review status: corrected"]')).toBeNull()
    expect(container.querySelector('a[href="https://example.com/deep"]')).not.toBeNull()
    await act(async () => {
      finishPrompt?.({ request_id: requestId, outcome: 'cancelled', runtime_phase: 'sleeping' })
      await Promise.resolve()
    })
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
            capabilities: completeCapabilities(),
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

    expect(nonDiagnosticCommands(invokedCommands)).toEqual([
      'get_startup_state',
      'ingest_audio_frame',
      'ingest_audio_frame',
    ])
    expect(container.textContent).toContain('Stop mic')
  })

  it('hides raw runtime control rejection messages from chat', async () => {
    const stop = vi.fn()
    let onFrame: ((frame: readonly number[]) => Promise<void> | void) | null = null
    let nowMs = 1_000

    class FakeAudio {
      play(): Promise<void> {
        return Promise.resolve()
      }
    }

    Object.defineProperty(globalThis, 'Audio', {
      configurable: true,
      value: FakeAudio,
    })

    Date.now = () => nowMs

    startLiveAudioSourceMock.mockImplementation(async (options) => {
      onFrame = options.onFrame
      return { stop }
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
            capabilities: completeCapabilities(),
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

    nowMs = 3_000

    await act(async () => {
      await onFrame?.([0.001, -0.001, 0.001, -0.001])
      await Promise.resolve()
    })

    expect(container.textContent).not.toContain(
      'Runtime control error (mark_silence): utterance transcription failed: InvalidTranscript(EmptyText)',
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

function completeCapabilities(overrides: Record<string, string> = {}): Array<Record<string, unknown>> {
  return ['custom_provider', 'opencode', 'local_fast', 'local_quality', 'qwen_prediction', 'wake_word', 'vad', 'parakeet', 'tts', 'deep', 'review']
    .map((id) => ({ id, state: overrides[id] ?? 'available', reason: 'test fixture', actual_provider: null }))
}

function readyStartupState(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    kind: 'ready',
    cue_asset_paths: { start_listening: 'start.wav', stop_listening: 'stop.wav' },
    runtime_phase: 'sleeping',
    voice_input_available: true,
    voice_input_error: null,
    silence_timeout_ms: 1500,
    selected_response_profile: 'fast',
    supported_response_profiles: ['fast', 'quality'],
    tts_enabled: false,
    capabilities: completeCapabilities(),
    ...overrides,
  }
}

function defaultAssistantSettings(
  instant: 'local-fast' | 'local-quality' = 'local-fast',
): Record<string, unknown> {
  return {
    instant,
    deep: 'opencode-sol-high',
    review: 'opencode-sol-high',
    deep_enabled: false,
    review_enabled: false,
    prefetch: false,
    completion: false,
  }
}

function getComposer(container: HTMLElement): HTMLTextAreaElement {
  const composer = container.querySelector<HTMLTextAreaElement>('#promptComposer')

  if (composer === null) {
    throw new Error('Missing composer textarea')
  }

  return composer
}

function getShell(container: HTMLElement): HTMLElement {
  const shell = container.querySelector<HTMLElement>('.shell')

  if (shell === null) {
    throw new Error('Missing app shell')
  }

  return shell
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

function getButtonByLabel(container: HTMLElement, label: string): HTMLButtonElement {
  const button = container.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`)

  if (button === null) {
    throw new Error(`Missing ${label} button`)
  }

  return button
}

function getButtonByText(container: HTMLElement, text: string): HTMLButtonElement {
  const button = Array.from(container.querySelectorAll<HTMLButtonElement>('button'))
    .find((candidate) => candidate.textContent?.trim() === text)
  if (button === undefined) {
    throw new Error(`Missing ${text} button`)
  }
  return button
}

function getControlButton(
  container: HTMLElement,
  label: 'Start mic' | 'Stop mic',
): HTMLButtonElement {
  const buttons = Array.from(container.querySelectorAll<HTMLButtonElement>('button'))
  const button = buttons.find((candidate) => candidate.textContent?.trim() === label)

  if (button === undefined) {
    throw new Error(`Missing ${label} control button`)
  }

  return button
}

function getAutoStopToggle(container: HTMLElement): HTMLInputElement {
  const labels = Array.from(container.querySelectorAll<HTMLLabelElement>('label.shell__toggle'))
  const autoStopLabel = labels.find((label) => label.textContent?.includes('Auto Stop'))
  const toggle = autoStopLabel?.querySelector<HTMLInputElement>('input[type="checkbox"]') ?? null

  if (toggle === null) {
    throw new Error('Missing auto stop on silence toggle')
  }

  return toggle
}

function getTtsToggle(container: HTMLElement): HTMLInputElement {
  const toggle = container.querySelector<HTMLInputElement>('#tts-toggle')

  if (toggle === null) {
    throw new Error('Missing TTS toggle')
  }

  return toggle
}

async function getResponseProfileSelect(container: HTMLElement): Promise<HTMLSelectElement> {
  await act(async () => {
    getButtonByLabel(container, 'Settings').click()
    await Promise.resolve()
  })
  const select = container.querySelector<HTMLSelectElement>('#assistantInstantSelect')

  if (select === null) {
    throw new Error('Missing response profile select')
  }

  return select
}

function setSelectValue(select: HTMLSelectElement, value: string): void {
  if (select.id === 'assistantInstantSelect') value = value === 'quality' ? 'local-quality' : value === 'fast' ? 'local-fast' : value
  select.value = value
  select.dispatchEvent(new Event('change', { bubbles: true }))
}

function nonDiagnosticCommands(commands: readonly string[]): readonly string[] {
  return commands.filter(
    (command) =>
      command !== 'write_runtime_log' &&
      command !== 'record_frontend_runtime_diagnostic' &&
      command !== 'get_ui_text_size' &&
      command !== 'set_ui_text_size' &&
      command !== 'get_ui_theme' &&
      command !== 'set_ui_theme' &&
      command !== 'get_assistant_settings' &&
      command !== 'set_assistant_settings' &&
      command !== 'check_for_update',
  )
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

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null
}

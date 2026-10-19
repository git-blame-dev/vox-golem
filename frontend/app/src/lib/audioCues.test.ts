import { afterEach, describe, expect, it, vi } from 'vitest'
import { createBrowserCuePlayer, createNativeAudioLifecycle, createNativeCuePlayer, playCue, playCueWithFallback, prepareVoiceCues } from './audioCues'
import { DEFAULT_CUE_ASSET_PATHS } from './startupState'

const WINDOWS_CUE_PATH = 'C:\\bundle\\start-listening.wav'
const WINDOWS_CUE_FILE_URL = 'file:///C:/bundle/start-listening.wav'
const activeAdmission = (): boolean => true

afterEach(() => {
  Reflect.deleteProperty(window, '__TAURI_INTERNALS__')
  vi.unstubAllGlobals()
})

describe('prepareVoiceCues', () => {
  it('accepts native readiness without transporting PCM or WAV URLs', async () => {
    window.__TAURI_INTERNALS__ = { invoke: vi.fn(async () => ({ native_ready: true })) }
    expect(await prepareVoiceCues()).toEqual({
      startListening: 'native:start', stopListening: 'native:stop', stopListeningShort: 'native:stop-short',
    })
  })
  it('loads the two prepared local WAV URLs without changing the embedded fallback', async () => {
    const start = 'data:audio/wav;base64,AAEC'
    const stop = 'data:audio/wav;base64,AQID'
    const shortStop = 'data:audio/wav;base64,BAUG'
    window.__TAURI_INTERNALS__ = {
      invoke: vi.fn(async () => ({ start_listening: start, stop_listening: stop, stop_listening_short: shortStop })),
    }

    const prepared = await prepareVoiceCues()

    expect(prepared).toEqual({ startListening: start, stopListening: stop, stopListeningShort: shortStop })
    expect(window.__TAURI_INTERNALS__.invoke).toHaveBeenCalledWith('prepare_voice_cues')
    expect(DEFAULT_CUE_ASSET_PATHS).toEqual({
      startListening: 'resources/start-listening.wav',
      stopListening: 'resources/stop-listening.wav',
    })
  })

  it('rejects incomplete or nonlocal cue responses', async () => {
    window.__TAURI_INTERNALS__ = {
      invoke: vi.fn(async () => ({
        start_listening: 'data:audio/wav;base64,AAEC',
        stop_listening: 'https://example.test/audio.wav',
      })),
    }

    await expect(prepareVoiceCues()).rejects.toThrow('Local voice cue URL is invalid')
  })
})

describe('native cue ownership', () => {
  it('blocks a fresh delayed-frame cue synchronously while hide IPC is pending', async () => {
    let release: () => void = () => undefined
    const lifecyclePending = new Promise<void>((resolve) => { release = resolve })
    const invoke = vi.fn(async (command: string, args?: unknown) => {
      if (command === 'set_native_audio_active') return (args as { active: boolean })?.active === false ? lifecyclePending : null
      if (command === 'reserve_cue_playback_id') return 40
      return { playback_id: 40, stage: 'activated' }
    })
    window.__TAURI_INTERNALS__ = { invoke }
    const lifecycle = createNativeAudioLifecycle()
    const player = lifecycle.createCuePlayer()
    lifecycle.setActive(true)
    await Promise.resolve(); await Promise.resolve()
    lifecycle.setActive(false)
    player.stop()
    const started = await player.playNativeCue!('start_listening')
    release()
    expect(started).toBe(false)
    expect(invoke.mock.calls.map(([command]) => command)).toEqual(['set_native_audio_active', 'set_native_audio_active'])
  })

  it.each([false, true])('never replays hidden or pre-hide requests after show (replace player=%s)', async (replacePlayer) => {
    let releaseHide: () => void = () => undefined
    let reserveOld: (ticket: number) => void = () => undefined
    const hiding = new Promise<void>((resolve) => { releaseHide = resolve })
    const oldReservation = new Promise<number>((resolve) => { reserveOld = resolve })
    let reservations = 0
    const invoke = vi.fn(async (command: string, args?: unknown) => {
      if (command === 'set_native_audio_active') return (args as { active: boolean })?.active === false ? hiding : null
      if (command === 'reserve_cue_playback_id') return ++reservations === 1 ? oldReservation : 48
      if (command === 'activate_voice_cue') return { playback_id: 48, stage: 'activated' }
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke }
    const lifecycle = createNativeAudioLifecycle()
    let player = lifecycle.createCuePlayer()
    lifecycle.setActive(true)
    await Promise.resolve(); await Promise.resolve()
    const old = player.playNativeCue!('start_listening')
    const oldPlayer = player
    lifecycle.setActive(false)
    await expect(player.playNativeCue!('stop_listening')).resolves.toBe(false)
    if (replacePlayer) player = lifecycle.createCuePlayer() // StrictMode lifetime replay.
    lifecycle.setActive(true)
    await expect(player.playNativeCue!('stop_listening')).resolves.toBe(false)
    reserveOld(47)
    await expect(old).resolves.toBe(false)
    expect(reservations).toBe(1)
    expect(invoke.mock.calls.some(([command]) => command === 'activate_voice_cue')).toBe(false)
    releaseHide()
    await hiding; await Promise.resolve(); await Promise.resolve(); await Promise.resolve()
    if (replacePlayer) {
      await expect(oldPlayer.playNativeCue!('start_listening')).resolves.toBe(false)
      expect(reservations).toBe(1)
    }
    await expect(player.playNativeCue!('stop_listening')).resolves.toBe(true)
    expect(reservations).toBe(2)
    expect(invoke.mock.calls.filter(([command]) => command === 'activate_voice_cue')).toEqual([
      ['activate_voice_cue', { playbackId: 48, cueType: 'stop_listening' }],
    ])
  })
  it('sends only a typed selection and owned ticket, never constructing HTMLAudio', async () => {
    const audio = vi.fn(() => { throw new Error('WebView audio is forbidden') })
    vi.stubGlobal('Audio', audio)
    const invoke = vi.fn(async (command: string) => {
      if (command === 'reserve_cue_playback_id') return 41
      return { playback_id: 41, stage: 'activated' }
    })
    window.__TAURI_INTERNALS__ = { invoke }
    const player = createNativeCuePlayer(activeAdmission)
    await expect(playCueWithFallback('stop_listening', DEFAULT_CUE_ASSET_PATHS, DEFAULT_CUE_ASSET_PATHS, player)).resolves.toBe(true)
    expect(invoke.mock.calls).toEqual([
      ['reserve_cue_playback_id', undefined],
      ['activate_voice_cue', { playbackId: 41, cueType: 'stop_listening' }],
    ])
    expect(audio).not.toHaveBeenCalled()
  })

  it('retires a late reservation before it can activate after Stop', async () => {
    let reserve: (value: number) => void = () => undefined
    const pending = new Promise<number>((resolve) => { reserve = resolve })
    const invoke = vi.fn(async (command: string) => command === 'reserve_cue_playback_id' ? pending : null)
    window.__TAURI_INTERNALS__ = { invoke }
    const player = createNativeCuePlayer(activeAdmission)
    const playing = player.playNativeCue!('start_listening')
    player.stop()
    reserve(42)
    await expect(playing).resolves.toBe(false)
    expect(invoke.mock.calls.map(([command]) => command)).toEqual(['reserve_cue_playback_id', 'finish_cue_playback'])
    expect(invoke).toHaveBeenLastCalledWith('finish_cue_playback', { playbackId: 42 })
  })

  it('cancels blocked activation without trying renderer fallback', async () => {
    let rejectActivation: (error: Error) => void = () => undefined
    const pending = new Promise<never>((_resolve, reject) => { rejectActivation = reject })
    const invoke = vi.fn(async (command: string) => {
      if (command === 'reserve_cue_playback_id') return 43
      if (command === 'activate_voice_cue') return pending
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke }
    const player = createNativeCuePlayer(activeAdmission)
    const playing = playCueWithFallback('start_listening', DEFAULT_CUE_ASSET_PATHS, DEFAULT_CUE_ASSET_PATHS, player)
    await Promise.resolve(); await Promise.resolve(); await Promise.resolve()
    player.stop()
    rejectActivation(new Error('synthetic cancellation'))
    await expect(playing).resolves.toBe(false)
    expect(invoke).toHaveBeenCalledWith('finish_cue_playback', { playbackId: 43 })
    expect(invoke.mock.calls.filter(([command]) => command === 'activate_voice_cue')).toHaveLength(1)
  })

  it('honors speech ownership rather than interrupting speech with a cue', async () => {
    const invoke = vi.fn(async () => null)
    window.__TAURI_INTERNALS__ = { invoke }
    await expect(createNativeCuePlayer(activeAdmission).playNativeCue!('stop_listening')).resolves.toBe(false)
    expect(invoke).toHaveBeenCalledOnce()
  })

  it('treats native purpose preemption as cancellation, not a playback failure or fallback', async () => {
    const invoke = vi.fn(async (command: string) => command === 'reserve_cue_playback_id'
      ? 44 : { playback_id: 44, stage: 'cancelled' })
    window.__TAURI_INTERNALS__ = { invoke }
    const player = createNativeCuePlayer(activeAdmission)
    await expect(playCueWithFallback('stop_listening', DEFAULT_CUE_ASSET_PATHS, DEFAULT_CUE_ASSET_PATHS, player)).resolves.toBe(false)
    player.stop()
    expect(invoke.mock.calls.map(([command]) => command)).toEqual(['reserve_cue_playback_id', 'activate_voice_cue'])
  })

  it('retires a stale reservation without stopping the newer cue', async () => {
    let releaseOld: (ticket: number) => void = () => undefined
    const old = new Promise<number>((resolve) => { releaseOld = resolve })
    let reservations = 0
    const invoke = vi.fn(async (command: string) => {
      if (command === 'reserve_cue_playback_id') return ++reservations === 1 ? old : 46
      if (command === 'activate_voice_cue') return { playback_id: 46, stage: 'activated' }
      return null
    })
    window.__TAURI_INTERNALS__ = { invoke }
    const player = createNativeCuePlayer(activeAdmission)
    const stale = player.playNativeCue!('start_listening')
    await expect(player.playNativeCue!('stop_listening')).resolves.toBe(true)
    releaseOld(45)
    await expect(stale).resolves.toBe(false)
    expect(invoke).toHaveBeenLastCalledWith('finish_cue_playback', { playbackId: 45 })
    player.stop()
    expect(invoke).toHaveBeenLastCalledWith('finish_cue_playback', { playbackId: 46 })
  })

  it('serializes and coalesces foreground owner changes across delayed IPC', async () => {
    let release: () => void = () => undefined
    const first = new Promise<void>((resolve) => { release = resolve })
    const invoke = vi.fn(async () => { if (invoke.mock.calls.length === 1) await first })
    window.__TAURI_INTERNALS__ = { invoke }
    const lifecycle = createNativeAudioLifecycle()
    lifecycle.setActive(false)
    lifecycle.setActive(true)
    lifecycle.setActive(false)
    lifecycle.setActive(true)
    expect(invoke).toHaveBeenCalledOnce()
    release()
    await first; await Promise.resolve(); await Promise.resolve()
    expect(invoke.mock.calls).toEqual([
      ['set_native_audio_active', { active: false }],
      ['set_native_audio_active', { active: true }],
    ])
  })

  it('does not coalesce away hide while an earlier resume acknowledgement is pending', async () => {
    let resume: () => void = () => undefined
    const pendingResume = new Promise<void>((resolve) => { resume = resolve })
    const invoke = vi.fn(async () => { if (invoke.mock.calls.length === 1) await pendingResume })
    window.__TAURI_INTERNALS__ = { invoke }
    const lifecycle = createNativeAudioLifecycle()
    const player = lifecycle.createCuePlayer()
    lifecycle.setActive(true)
    lifecycle.setActive(false)
    lifecycle.setActive(true)
    await expect(player.playNativeCue!('start_listening')).resolves.toBe(false)
    resume()
    await pendingResume; await Promise.resolve(); await Promise.resolve(); await Promise.resolve(); await Promise.resolve()
    expect(invoke.mock.calls).toEqual([
      ['set_native_audio_active', { active: true }],
      ['set_native_audio_active', { active: false }],
      ['set_native_audio_active', { active: true }],
    ])
  })
})

describe('playCue', () => {
  it('pauses an active browser cue on demand', async () => {
    const paused = vi.fn()
    vi.stubGlobal('Audio', class {
      play(): Promise<void> { return Promise.resolve() }
      pause(): void { paused() }
    })

    const player = createBrowserCuePlayer()
    await player.play('data:audio/wav;base64,AAEC')
    player.stop()

    expect(paused).toHaveBeenCalledOnce()
  })

  it('does not play a fallback cue after an in-flight cue is stopped', async () => {
    const played: string[] = []
    vi.stubGlobal('Audio', class {
      constructor(source: string) { played.push(source) }
      play(): Promise<void> {
        return new Promise<void>((_resolve, reject) => { this.rejectPlayback = reject })
      }
      rejectPlayback: ((error: Error) => void) | null = null
      pause(): void { this.rejectPlayback?.(new Error('playback was interrupted')) }
    })
    const player = createBrowserCuePlayer()
    const preferred = { ...DEFAULT_CUE_ASSET_PATHS, stopListening: 'data:audio/wav;base64,AAEC' }

    const pending = playCueWithFallback('stop_listening', preferred, DEFAULT_CUE_ASSET_PATHS, player)
    player.stop()
    await expect(pending).resolves.toBe(false)
    expect(played).toEqual([preferred.stopListening])
  })

  it('falls back to the embedded chime when prepared voice playback fails', async () => {
    const voice = 'data:audio/wav;base64,AAEC'
    const play = vi.fn(async (source: string) => {
      if (source === voice) throw new Error('voice playback failed')
    })

    await playCueWithFallback(
      'stop_listening',
      { ...DEFAULT_CUE_ASSET_PATHS, stopListening: voice },
      DEFAULT_CUE_ASSET_PATHS,
      { play },
    )

    expect(play.mock.calls.map(([source]) => source)).toEqual([
      voice,
      DEFAULT_CUE_ASSET_PATHS.stopListening,
    ])
  })
  it('plays the configured start-listening cue', async () => {
    const play = vi.fn(async () => undefined)

    await playCue('start_listening', DEFAULT_CUE_ASSET_PATHS, { play })

    expect(play).toHaveBeenCalledWith(DEFAULT_CUE_ASSET_PATHS.startListening)
  })

  it('fails clearly when a configured cue asset path is missing', async () => {
    const play = vi.fn(async () => undefined)

    await expect(
      playCue(
        'stop_listening',
        {
          startListening: DEFAULT_CUE_ASSET_PATHS.startListening,
          stopListening: '',
        },
        { play },
      ),
    ).rejects.toThrow('Missing `stopListening` cue asset path')
  })

  it('converts configured windows filesystem paths into file urls', async () => {
    const play = vi.fn(async () => undefined)

    await playCue(
      'start_listening',
      {
        startListening: WINDOWS_CUE_PATH,
        stopListening: DEFAULT_CUE_ASSET_PATHS.stopListening,
      },
      { play },
    )

    expect(play).toHaveBeenCalledWith(WINDOWS_CUE_FILE_URL)
  })

  it('uses tauri convertFileSrc when available for local files', async () => {
    const play = vi.fn(async () => undefined)

    window.__TAURI_INTERNALS__ = {
      invoke: async () => null,
      convertFileSrc: (filePath) => `asset://localhost/${encodeURIComponent(filePath)}`,
    }

    await playCue(
      'start_listening',
      {
        startListening: WINDOWS_CUE_PATH,
        stopListening: DEFAULT_CUE_ASSET_PATHS.stopListening,
      },
      { play },
    )

    expect(play).toHaveBeenCalledWith(
      `asset://localhost/${encodeURIComponent(WINDOWS_CUE_PATH)}`,
    )
  })

  it('preserves already-url cue sources', async () => {
    const play = vi.fn(async () => undefined)

    await playCue(
      'start_listening',
      {
        startListening: WINDOWS_CUE_FILE_URL,
        stopListening: DEFAULT_CUE_ASSET_PATHS.stopListening,
      },
      { play },
    )

    expect(play).toHaveBeenCalledWith(WINDOWS_CUE_FILE_URL)
  })
})

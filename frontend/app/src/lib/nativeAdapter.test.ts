import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { invokeAndroidCommand, withoutAndroidEventListener } from './tauri'
import { NEW_VOICE_SHORTCUT_COMMANDS } from './newVoiceShortcut'
import { parseAndroidVoiceEvent, parseAndroidVoiceStatus } from './androidVoice'

afterEach(() => { delete window.__TAURI_INTERNALS__ })
const source = (path: string): string => readFileSync(resolve(process.cwd(), '../..', path), 'utf8')

describe('Android command boundary', () => {
  it.each(['credential_status', 'import_access_credential', 'retry_credential_transfer', 'assistant_pairing_status'])('rejects removed management command %s without invoking native code', async (command) => {
    const native = vi.fn(async () => null)
    window.__TAURI_INTERNALS__ = { invoke: native }
    await expect(invokeAndroidCommand(command)).rejects.toMatchObject({ code: 'unavailable' })
    expect(native).not.toHaveBeenCalled()
  })
})

describe('native shortcut adapter contract', () => {
  it('forwards precisely the six shortcut commands with camelCase payloads and no global listener', async () => {
    const native = vi.fn(async () => null)
    const listen = vi.fn()
    window.__TAURI_INTERNALS__ = withoutAndroidEventListener({ invoke: native, listen }, true)
    expect(NEW_VOICE_SHORTCUT_COMMANDS).toHaveLength(6)
    const androidSource = source('apps/android-tauri/src-tauri/src/lib.rs')
    const pluginSource = source('crates/assistant-plugin/android/src/main/java/dev/gitblame/voxgolem/assistant/phone/AssistantPlugin.kt')
    for (const name of NEW_VOICE_SHORTCUT_COMMANDS) {
      expect(androidSource).toContain(name)
      expect(pluginSource).toContain(`@Command fun ${name}(`)
      const payload = name === 'prepare_new_voice_shortcut' ? { sessionId: 'synthetic-session' } : name === 'release_new_voice_shortcut' ? { token: '77777777-7777-4777-8777-777777777777' } : undefined
      await invokeAndroidCommand(name, payload)
      expect(native).toHaveBeenLastCalledWith(name, payload)
    }
    await expect(invokeAndroidCommand('unknown_shortcut_command')).rejects.toMatchObject({ code: 'unavailable' })
    expect(native).toHaveBeenCalledTimes(6)
    expect(listen).not.toHaveBeenCalled()
  })

  it('parses frames with the actual producer field names and readiness-before-dictation ordering', () => {
    const producer = source('crates/assistant-plugin/android/src/main/java/dev/gitblame/voxgolem/assistant/phone/PhoneSpeechSession.kt')
    expect(producer.indexOf('.put("kind", "shortcut_ready")')).toBeLessThan(producer.indexOf('.put("kind", "dictation")'))
    for (const field of ['sessionId', 'requestId', 'text', 'draft', 'shortcutPhase']) expect(producer).toContain(`.put("${field}",`)
    const sessionId = 'synthetic-session'
    expect(parseAndroidVoiceEvent({ kind: 'shortcut_ready', sessionId })).toEqual({ kind: 'shortcut_ready', sessionId })
    expect(parseAndroidVoiceEvent({ kind: 'dictation', sessionId, phase: 'partial', draft: 'Synthetic recognized words', utterance: '1', error: null })).toMatchObject({ kind: 'dictation', draft: 'Synthetic recognized words' })
    expect(parseAndroidVoiceEvent({ kind: 'submit', sessionId, requestId: 'synthetic-authoritative-final', text: 'Synthetic authoritative final' })).toMatchObject({ kind: 'submit', requestId: 'synthetic-authoritative-final', text: 'Synthetic authoritative final' })
    expect(parseAndroidVoiceStatus({ kind: 'voice_status', available: true, reason: 'Synthetic local speech', mode: 'shortcut', dictation: 'listening', loop: 'idle', ttsEnabled: false, ttsAvailable: false, ttsPhase: 'idle', error: null, shortcutPhase: 'listening' })).toMatchObject({ mode: 'shortcut', shortcutPhase: 'listening' })
    expect(() => parseAndroidVoiceEvent({ kind: 'submit', session_id: sessionId, request_id: 'synthetic-invalid', text: 'Synthetic words' })).toThrow()
    expect(() => parseAndroidVoiceEvent({ kind: 'submit', sessionId, requestId: 'synthetic-empty', text: ' ' })).toThrow()
  })
})

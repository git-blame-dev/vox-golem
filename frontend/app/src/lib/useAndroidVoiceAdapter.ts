import { useCallback, useEffect, useRef, useState } from 'react'
import type { RefObject } from 'react'
import { getTauriInternals, invokeTauriCommand } from './tauri'
import { isRecord } from './wire'
import { parseAndroidVoiceEvent, parseAndroidVoiceStatus } from './androidVoice'
import type { AndroidVoiceStatus, AndroidVoiceContext } from './androidVoice'
import { NEW_VOICE_SHORTCUT_EVENT, parsePendingNewVoiceAction, pendingNewVoiceToken } from './newVoiceShortcut'
import { NativeVoiceStatusPoller } from './nativeVoiceStatusPoller'

export interface AndroidVoiceAdapterInputs {
  readonly enabled?: boolean
  readonly statusEnabled?: boolean
  readonly draft: string
  readonly disabled: boolean
  readonly generationAvailable: boolean
  readonly stopRef: RefObject<(() => void) | null>
  readonly onDraft: (text: string) => void
  readonly onSubmit: (requestId: string, text: string, context?: AndroidVoiceContext) => void
  readonly onActivity: (active: boolean) => void
  readonly onInputCancelled?: () => void
  readonly onSettled: () => void
  readonly onStatus?: (status: AndroidVoiceStatus | null, statusError: string | null) => void
  readonly shortcutReady?: boolean
  /** Flush and bind the current conversation/draft before microphone permission admission. */
  readonly onBeforeStart?: () => Promise<string>
  readonly onShortcutBeforeStart?: () => Promise<string>
  readonly onShortcutStarted?: () => void
  readonly onShortcutPrepare?: (oldDraft: string, isCurrent: () => boolean) => Promise<void>
  readonly onShortcutCancelled?: (oldDraft: string, prepared: boolean) => void
}

export function useAndroidVoiceAdapter({ enabled = true, statusEnabled = enabled, draft, disabled, generationAvailable, stopRef, onDraft, onSubmit, onActivity, onInputCancelled, onSettled, onStatus, shortcutReady = false, onBeforeStart, onShortcutBeforeStart, onShortcutStarted, onShortcutPrepare, onShortcutCancelled }: AndroidVoiceAdapterInputs) {
  const [status, setStatus] = useState<AndroidVoiceStatus | null>(null)
  const [pending, setPending] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [ownedSessionError, setOwnedSessionError] = useState<string | null>(null)
  const [statusError, setStatusError] = useState<string | null>(null)
  const [confirmationRequired, setConfirmationRequired] = useState(false)
  const sessionRef = useRef<string | null>(null)
  const statusCallbackRef = useRef(onStatus)
  const statusRevisionRef = useRef(0)
  const lifecycleEpochRef = useRef(0)
  const [statusPoller] = useState(() => new NativeVoiceStatusPoller())
  useEffect(() => { statusCallbackRef.current = onStatus }, [onStatus])
  useEffect(() => { statusCallbackRef.current?.(status, statusError) }, [status, statusError])
  const acceptStatus = useCallback((next: AndroidVoiceStatus): void => {
    statusRevisionRef.current += 1
    setStatus(next)
    setStatusError(null)
    setOwnedSessionError(sessionRef.current !== null ? next.error : null)
  }, [])
  const mounted = useRef(true)
  const ttsAvailable = useRef(false)
  const settledRef = useRef(onSettled)
  const shortcutRef = useRef<{ token: string; oldDraft: string; prepared: boolean; prepare: Promise<void> | null } | null>(null)
  const shortcutScanRef = useRef<() => Promise<void>>(async () => undefined)
  const scanPendingRef = useRef<object | null>(null)
  const startingRef = useRef(false)
  const launchEpochRef = useRef(0)
  const launchHintsRef = useRef({ shortcutReady, disabled, active: false, available: status?.available })
  const consumedTokensRef = useRef(new Set<string>())
  const cancelShortcutCallbackRef = useRef(onShortcutCancelled)
  const inputCancelledCallbackRef = useRef(onInputCancelled)
  useEffect(() => { inputCancelledCallbackRef.current = onInputCancelled }, [onInputCancelled])
  useEffect(() => { cancelShortcutCallbackRef.current = onShortcutCancelled }, [onShortcutCancelled])
  useEffect(() => { settledRef.current = onSettled }, [onSettled])
  const active = pending || (status !== null && (status.shortcutPhase === 'listening' || status.shortcutPhase === 'finishing' || status.ttsPhase === 'speaking' || ['checking', 'listening', 'finishing'].includes(status.dictation) || ['listening', 'generating', 'speaking', 'between_turns'].includes(status.loop)))
  useEffect(() => {
    if (launchHintsRef.current.shortcutReady !== shortcutReady) launchEpochRef.current += 1
    launchHintsRef.current = { shortcutReady, disabled, active, available: status?.available }
  }, [shortcutReady, disabled, active, status?.available])
  const stop = useCallback(() => {
    statusRevisionRef.current += 1
    launchEpochRef.current += 1
    setConfirmationRequired(false)
    inputCancelledCallbackRef.current?.()
    const shortcut = shortcutRef.current
    shortcutRef.current = null
    if (shortcut !== null) { void invokeTauriCommand('release_new_voice_shortcut', { token: shortcut.token }).catch(() => undefined) }
    if (shortcut !== null) cancelShortcutCallbackRef.current?.(shortcut.oldDraft, shortcut.prepared)
    sessionRef.current = null
    setOwnedSessionError(null)
    onActivity(false)
    setPending(false)
    setStatus((current) => current === null ? null : { ...current, mode: current.mode === 'shortcut' ? 'dictation' : current.mode, dictation: 'idle', loop: 'idle', ttsPhase: 'idle', shortcutPhase: null, readyForSpeech: false })
    void invokeTauriCommand('stop_assistant_voice').catch(() => undefined)
  }, [onActivity])
  useEffect(() => {
    if (!enabled) return undefined
    mounted.current = true
    lifecycleEpochRef.current += 1
    stopRef.current = stop
    return () => { mounted.current = false; startingRef.current = false; scanPendingRef.current = null; setPending(false); lifecycleEpochRef.current += 1; launchEpochRef.current += 1; inputCancelledCallbackRef.current?.(); const shortcut = shortcutRef.current; shortcutRef.current = null; if (shortcut !== null) void invokeTauriCommand('release_new_voice_shortcut', { token: shortcut.token }).catch(() => undefined); onActivity(false); stopRef.current = null; sessionRef.current = null; void invokeTauriCommand('stop_assistant_voice').catch(() => undefined) }
  }, [enabled, onActivity, stop, stopRef])
  useEffect(() => {
    if (!statusEnabled) return undefined
    return statusPoller.start({
      revision: () => statusRevisionRef.current,
      publish: (next) => {
        acceptStatus(next)
        if (next.ttsAvailable !== ttsAvailable.current) { ttsAvailable.current = next.ttsAvailable; settledRef.current() }
      },
      fail: () => setStatusError('Native speech status is unavailable'),
      hidden: stop,
    })
  }, [acceptStatus, statusEnabled, statusPoller, stop])
  useEffect(() => { onActivity(enabled && active) }, [active, enabled, onActivity])

  useEffect(() => {
    if (!enabled || !shortcutReady) return undefined
    let timer: number | null = null
    const scan = (): void => {
      if (timer !== null) window.clearTimeout(timer)
      timer = window.setTimeout(() => { timer = null; void shortcutScanRef.current() }, 0)
    }
    scan()
    window.addEventListener(NEW_VOICE_SHORTCUT_EVENT, scan)
    window.addEventListener('focus', scan)
    window.addEventListener('pageshow', scan)
    document.addEventListener('visibilitychange', scan)
    return () => { if (timer !== null) window.clearTimeout(timer); window.removeEventListener(NEW_VOICE_SHORTCUT_EVENT, scan); window.removeEventListener('focus', scan); window.removeEventListener('pageshow', scan); document.removeEventListener('visibilitychange', scan) }
  }, [enabled, shortcutReady, status?.available])

  const start = async (loopMode: boolean, launchToken?: string, completionPolicy: 'manual' | 'single_turn' | 'repeat_after_reply' = loopMode ? 'repeat_after_reply' : 'manual'): Promise<void> => {
    const releaseLaunch = (): void => { if (launchToken !== undefined) void invokeTauriCommand('release_new_voice_shortcut', { token: launchToken }).catch(() => undefined) }
    if (!enabled || startingRef.current || active || pending ||
      (onBeforeStart === undefined && (disabled || ((loopMode || completionPolicy === 'single_turn') && !generationAvailable)))) { releaseLaunch(); return }
    if (launchToken === undefined && shortcutRef.current !== null) {
      const previous = shortcutRef.current
      if (!previous.prepared) { setError('Review and send the new voice draft before starting another voice session'); return }
      void invokeTauriCommand('release_new_voice_shortcut', { token: previous.token }).catch(() => undefined)
      shortcutRef.current = null; cancelShortcutCallbackRef.current?.(previous.oldDraft, true)
    }
    const internals = getTauriInternals()
    if (internals?.createChannel === undefined) { releaseLaunch(); setError('Native voice requires Channel support'); return }
    if (sessionRef.current !== null) inputCancelledCallbackRef.current?.()
    startingRef.current = true
    const sessionId = crypto.randomUUID()
    const lifecycleEpoch = lifecycleEpochRef.current
    const ownsStart = (): boolean => mounted.current && lifecycleEpoch === lifecycleEpochRef.current && sessionRef.current === sessionId
    statusRevisionRef.current += 1
    sessionRef.current = sessionId
    const requests = new Set<string>()
    let receivedDraft = false
    let nativeAdmitted = false
    setPending(true); setError(null); setOwnedSessionError(null)
    setConfirmationRequired(false)
    let ownedDraft = draft
    const legacyShortcut = launchToken !== undefined && onBeforeStart === undefined && onShortcutBeforeStart !== undefined
    if (onBeforeStart !== undefined) {
      try {
        ownedDraft = await onBeforeStart()
        if (!ownsStart() || document.visibilityState !== 'visible') { releaseLaunch(); if (lifecycleEpoch === lifecycleEpochRef.current) startingRef.current = false; if (ownsStart()) setPending(false); return }
        if (launchToken !== undefined) shortcutRef.current = { token: launchToken, oldDraft: ownedDraft, prepared: true, prepare: Promise.resolve() }
      } catch (failure) {
        releaseLaunch()
        if (lifecycleEpoch === lifecycleEpochRef.current) startingRef.current = false
        if (mounted.current && sessionRef.current === sessionId) {
          sessionRef.current = null; setPending(false)
          setError(failure instanceof Error ? failure.message : 'Voice chat admission failed')
        }
        return
      }
    }
    if (legacyShortcut) {
      try {
        const oldDraft = await onShortcutBeforeStart?.() ?? draft
        if (!ownsStart() || document.visibilityState !== 'visible') { releaseLaunch(); if (ownsStart()) cancelShortcutCallbackRef.current?.(oldDraft, false); if (lifecycleEpoch === lifecycleEpochRef.current) startingRef.current = false; if (ownsStart()) setPending(false); return }
        shortcutRef.current = { token: launchToken, oldDraft, prepared: false, prepare: null }
      } catch (failure) {
        void invokeTauriCommand('release_new_voice_shortcut', { token: launchToken }).catch(() => undefined)
        if (ownsStart()) { setError(failure instanceof Error ? failure.message : 'New voice chat is busy'); sessionRef.current = null; startingRef.current = false; setPending(false) }
        return
      }
    }
    try {
      const updates = internals.createChannel<unknown>((payload) => {
        if (!ownsStart()) return
        try {
          const event = parseAndroidVoiceEvent(payload)
          if (event.sessionId !== sessionId) return
          if (event.kind === 'dictation' && event.phase !== 'idle' && event.draft !== '') { receivedDraft = true; onDraft(event.draft) }
          if (event.kind === 'shortcut_ready' && legacyShortcut) {
            const shortcut = shortcutRef.current
            if (shortcut !== null && shortcut.prepare === null) {
              shortcut.prepare = (async () => {
                await Promise.resolve()
                if (!mounted.current || sessionRef.current !== sessionId) throw new Error('New voice chat was interrupted; review the retained text')
                if (onShortcutPrepare === undefined) throw new Error('New voice conversation preparation is unavailable')
                await onShortcutPrepare(shortcut.oldDraft, () => mounted.current && sessionRef.current === sessionId)
                if (!mounted.current || sessionRef.current !== sessionId) throw new Error('New voice chat was interrupted; review the retained text')
                await invokeTauriCommand('prepare_new_voice_shortcut', { sessionId })
                shortcut.prepared = true
              })()
              void shortcut.prepare.catch((failure: unknown) => { if (sessionRef.current === sessionId) { setError(failure instanceof Error ? failure.message : 'New conversation could not be prepared'); stop() } })
            }
          }
          if (event.kind === 'submit' && !requests.has(event.requestId)) {
            const submit = (): void => {
              if (event.context === undefined) onSubmit(event.requestId, event.text)
              else onSubmit(event.requestId, event.text, event.context)
            }
            if (requests.size >= 256) { const oldest = requests.values().next().value; if (oldest !== undefined) requests.delete(oldest) }
            requests.add(event.requestId)
            const shortcut = shortcutRef.current
            if (shortcut !== null) {
              void (async () => {
                if (shortcut.prepare === null) throw new Error('New voice conversation was not prepared; send the draft manually')
                await shortcut.prepare
                if (mounted.current && sessionRef.current === sessionId) { submit(); void invokeTauriCommand('release_new_voice_shortcut', { token: shortcut.token }).catch(() => undefined) }
              })().catch((failure: unknown) => { if (mounted.current && sessionRef.current === sessionId) setError(failure instanceof Error ? failure.message : 'Voice draft was not sent') })
            } else submit()
          }
          if (event.kind === 'voice_status') {
            acceptStatus(event.status)
            if (!legacyShortcut && shortcutRef.current !== null && ['idle', 'error', 'paused'].includes(event.status.loop) && !['checking', 'listening', 'finishing'].includes(event.status.dictation)) {
              const token = shortcutRef.current.token
              shortcutRef.current = null
              void invokeTauriCommand('release_new_voice_shortcut', { token }).catch(() => undefined)
            }
            if (event.status.dictation === 'final' || event.status.error !== null) onSettled()
          }
          if (event.kind === 'loop' && event.error !== null) setOwnedSessionError(event.error)
          if (event.kind === 'shortcut_review') {
            onDraft(event.draft); setError(event.reason)
            const shortcut = shortcutRef.current
            if (shortcut !== null && event.draft === '' && !shortcut.prepared) stop()
          }
        } catch { setError('Native voice returned an invalid event'); stop() }
      })
      const statusRevision = statusRevisionRef.current
      const value = await invokeTauriCommand('start_assistant_voice', { sessionId, loopMode, draft: legacyShortcut ? '' : ownedDraft, updates, completionPolicy: legacyShortcut ? 'stop_send_new' : completionPolicy, ...(launchToken === undefined ? {} : { launchToken }) })
      nativeAdmitted = true
      if (mounted.current && sessionRef.current === sessionId) { const next = parseAndroidVoiceStatus(value); if (statusRevision === statusRevisionRef.current) acceptStatus(next); if (legacyShortcut && !receivedDraft) onShortcutStarted?.() }
    } catch (failure) {
      if (sessionRef.current === sessionId) {
        inputCancelledCallbackRef.current?.()
        if (nativeAdmitted) void invokeTauriCommand('stop_assistant_voice').catch(() => undefined)
        const shortcut = shortcutRef.current; shortcutRef.current = null
        if (shortcut !== null) { onShortcutCancelled?.(shortcut.oldDraft, shortcut.prepared); void invokeTauriCommand('release_new_voice_shortcut', { token: shortcut.token }).catch(() => undefined) }
        sessionRef.current = null; setError(failure instanceof Error ? failure.message : 'Native voice failed')
      }
    } finally { if (lifecycleEpoch === lifecycleEpochRef.current) { startingRef.current = false; if (mounted.current) setPending(false) } }
  }
  useEffect(() => {
    shortcutScanRef.current = async () => {
      if (scanPendingRef.current !== null || !mounted.current || !launchHintsRef.current.shortcutReady || launchHintsRef.current.available === undefined || document.visibilityState !== 'visible') return
      const scan = {}
      scanPendingRef.current = scan
      const epoch = launchEpochRef.current
      let token: string | null = null
      try {
        const payload = await invokeTauriCommand('consume_new_voice_shortcut')
        token = pendingNewVoiceToken(payload)
        const action = parsePendingNewVoiceAction(payload)
        const confirmation = isRecord(payload) ? payload['confirmationRequired'] : undefined
        if (confirmation !== undefined && typeof confirmation !== 'boolean') throw new Error('Native launch confirmation is invalid')
        if (confirmation === true) {
          if (token !== null) await invokeTauriCommand('release_new_voice_shortcut', { token })
          token = null
          if (mounted.current && epoch === launchEpochRef.current && document.visibilityState === 'visible' && !launchHintsRef.current.active) setConfirmationRequired(true)
          return
        }
        if (action.error !== null) { if (token !== null) await invokeTauriCommand('release_new_voice_shortcut', { token }); token = null; if (mounted.current) setError(action.error); return }
        if (action.token === null) return
        if (consumedTokensRef.current.has(action.token)) { token = null; return }
        consumedTokensRef.current.add(action.token)
        if (consumedTokensRef.current.size > 256) { const oldest = consumedTokensRef.current.values().next().value; if (oldest !== undefined) consumedTokensRef.current.delete(oldest) }
        const hints = launchHintsRef.current
        if (!mounted.current || epoch !== launchEpochRef.current || document.visibilityState !== 'visible' || !hints.shortcutReady || (onBeforeStart === undefined && hints.disabled) || hints.active || startingRef.current || !hints.available) {
          await invokeTauriCommand('release_new_voice_shortcut', { token: action.token }); token = null; if (mounted.current) setError('New voice chat is busy or native dictation is unavailable; existing work was preserved'); return
        }
        token = null
        await start(false, action.token, 'single_turn')
      } catch { if (token !== null) { void invokeTauriCommand('release_new_voice_shortcut', { token }).catch(() => undefined); if (mounted.current) setError('Native shortcut launch is invalid; existing work was preserved') } }
       finally { if (scanPendingRef.current === scan) scanPendingRef.current = null }
    }
  })
  const finishDictation = (): void => { void invokeTauriCommand('finish_assistant_dictation').catch(() => setError('Dictation could not be finished')) }
  const finishShortcut = (): void => { void invokeTauriCommand('stop_send_new_voice_shortcut').catch(() => setError('Voice draft was not sent; review before manual Send')) }
  const toggleSpeech = (): void => {
    const lifecycleEpoch = lifecycleEpochRef.current
    setPending(true)
    void invokeTauriCommand('set_tts_enabled', { enabled: status?.ttsEnabled !== true }).then(async () => {
      const statusRevision = statusRevisionRef.current
      const next = parseAndroidVoiceStatus(await invokeTauriCommand('assistant_voice_status'))
      if (!mounted.current || lifecycleEpoch !== lifecycleEpochRef.current) return
      if (statusRevision === statusRevisionRef.current) acceptStatus(next)
      settledRef.current()
    }).catch((failure: unknown) => { if (mounted.current && lifecycleEpoch === lifecycleEpochRef.current) setError(failure instanceof Error ? failure.message : 'Native speech preference was not saved') }).finally(() => { if (mounted.current && lifecycleEpoch === lifecycleEpochRef.current) setPending(false) })
  }
  return { status, pending, error, ownedSessionError, statusError, confirmationRequired, active: enabled && active,
    start, stop, finishDictation, finishShortcut, toggleSpeech }
}

export type AndroidVoiceAdapterView = ReturnType<typeof useAndroidVoiceAdapter>

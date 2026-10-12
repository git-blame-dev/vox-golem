import { useCallback, useEffect, useRef, useState } from 'react'
import type { KeyboardEvent } from 'react'
import { flushSync } from 'react-dom'

export const SETTINGS_DIALOG_ID = 'settings-dialog'
const SETTINGS_HISTORY_KEY = '__voxgolemSettingsOpen'
const FOCUSABLE = 'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])'

/** Owns only DOM navigation, focus and scroll locking; preferences remain session-owned. */
export function useSettingsNavigation() {
  const [settingsOpen, setSettingsOpen] = useState(() => Boolean(window.history.state?.[SETTINGS_HISTORY_KEY]))
  const settingsButtonRef = useRef<HTMLButtonElement | null>(null)
  const settingsPanelRef = useRef<HTMLElement | null>(null)
  const settingsCloseButtonRef = useRef<HTMLButtonElement | null>(null)
  const historyActive = useRef(settingsOpen)
  const [historyToken] = useState(() => `settings-${crypto.randomUUID()}`)
  const historyOwned = useRef(false)
  const traversalPending = useRef(false)
  const reopenQueued = useRef(false)
  const restoreTimeout = useRef<number | null>(null)

  const closeSettings = useCallback((fromHistory = false): void => {
    const currentState = window.history.state
    const markerIsCurrent = Boolean(currentState?.[SETTINGS_HISTORY_KEY])
    const markerIsOwned = historyOwned.current && currentState?.[SETTINGS_HISTORY_KEY] === historyToken
    const shouldTraverse = !fromHistory && historyActive.current && markerIsOwned && !traversalPending.current
    if (shouldTraverse) traversalPending.current = true
    historyActive.current = fromHistory && markerIsCurrent
    historyOwned.current = fromHistory && markerIsOwned
    setSettingsOpen(false)
    if (restoreTimeout.current !== null) window.clearTimeout(restoreTimeout.current)
    if (!reopenQueued.current) {
      restoreTimeout.current = window.setTimeout(() => {
        restoreTimeout.current = null
        if (!historyActive.current && !traversalPending.current) settingsButtonRef.current?.focus()
      }, 0)
    }
    if (shouldTraverse) window.history.back()
    else if (!fromHistory && markerIsCurrent && !markerIsOwned) {
      const nextState = { ...(currentState ?? {}) }
      delete nextState[SETTINGS_HISTORY_KEY]
      window.history.replaceState(nextState, '')
      historyActive.current = false
    }
  }, [historyToken])

  const openSettings = useCallback((): void => {
    if (traversalPending.current) { reopenQueued.current = true; return }
    if (historyActive.current) return
    window.history.pushState({ ...(window.history.state ?? {}), [SETTINGS_HISTORY_KEY]: historyToken }, '')
    historyActive.current = true
    historyOwned.current = true
    flushSync(() => setSettingsOpen(true))
  }, [historyToken])

  useEffect(() => {
    if (settingsOpen) settingsCloseButtonRef.current?.focus()
  }, [settingsOpen])

  useEffect(() => {
    const onPopState = (): void => {
      if (window.history.state?.[SETTINGS_HISTORY_KEY]) {
        traversalPending.current = false
        historyActive.current = true
        historyOwned.current = window.history.state?.[SETTINGS_HISTORY_KEY] === historyToken
        reopenQueued.current = false
        flushSync(() => setSettingsOpen(true))
        return
      }
      if (!historyActive.current && !traversalPending.current && !settingsOpen) return
      historyActive.current = false
      traversalPending.current = false
      const reopen = reopenQueued.current
      reopenQueued.current = false
      closeSettings(true)
      if (reopen) openSettings()
    }
    window.addEventListener('popstate', onPopState)
    return () => window.removeEventListener('popstate', onPopState)
  }, [closeSettings, historyToken, openSettings, settingsOpen])

  useEffect(() => {
    if (!settingsOpen) return undefined
    const properties = ['overflow-x', 'overflow-y'] as const
    const read = () => properties.map((property) => ({
      property,
      value: document.body.style.getPropertyValue(property),
      priority: document.body.style.getPropertyPriority(property),
    }))
    const original = read()
    for (const declaration of original) document.body.style.setProperty(declaration.property, 'hidden', declaration.priority)
    const written = read()
    return () => {
      for (const [index, declaration] of written.entries()) {
        const { property } = declaration
        if (document.body.style.getPropertyValue(property) !== declaration.value ||
          document.body.style.getPropertyPriority(property) !== declaration.priority) continue
        document.body.style.removeProperty(property)
        const previous = original[index]!
        if (previous.value !== '') document.body.style.setProperty(property, previous.value, previous.priority)
      }
    }
  }, [settingsOpen])

  useEffect(() => () => {
    if (restoreTimeout.current !== null) window.clearTimeout(restoreTimeout.current)
  }, [])

  const onSettingsKeyDown = useCallback((event: KeyboardEvent<HTMLDivElement>): void => {
    if (event.key === 'Escape') { event.preventDefault(); closeSettings(); return }
    if (event.key !== 'Tab') return
    const elements = Array.from(settingsPanelRef.current?.querySelectorAll<HTMLElement>(FOCUSABLE) ?? [])
    const first = elements[0]
    const last = elements.at(-1)
    if (first === undefined || last === undefined) return
    if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus() }
    else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus() }
  }, [closeSettings])

  return { settingsOpen, settingsButtonRef, settingsPanelRef, settingsCloseButtonRef, openSettings, closeSettings, onSettingsKeyDown }
}

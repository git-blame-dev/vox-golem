import { useCallback, useEffect, useRef, useState } from 'react'
import type { KeyboardEvent } from 'react'
import { flushSync } from 'react-dom'
import { readSettingsPage } from './settingsPages'
import type { SettingsPage } from './settingsPages'
export type { SettingsPage } from './settingsPages'

export const SETTINGS_DIALOG_ID = 'settings-dialog'
const SETTINGS_HISTORY_KEY = '__voxgolemSettingsOpen'
const SETTINGS_PAGE_KEY = '__voxgolemSettingsPage'
const readPage = (): SettingsPage => readSettingsPage(window.history.state?.[SETTINGS_PAGE_KEY])
const FOCUSABLE = 'button, [href], input, select, textarea, summary:not([tabindex="-1"]), [tabindex]:not([tabindex="-1"])'

function isSemanticallyFocusable(element: HTMLElement): boolean {
  if (element.matches(':disabled, input[type="hidden"]') ||
    (element.hasAttribute('tabindex') && Number(element.getAttribute('tabindex')) < 0)) return false
  for (let ancestor: HTMLElement | null = element; ancestor !== null; ancestor = ancestor.parentElement) {
    if (ancestor.hasAttribute('hidden') || ancestor.hasAttribute('inert')) return false
    if (ancestor instanceof HTMLFieldSetElement && ancestor.disabled && element.matches('button, input, select, textarea')) {
      const legend = Array.from(ancestor.children).find((child) => child.tagName === 'LEGEND')
      if (!legend?.contains(element)) return false
    }
    if (ancestor instanceof HTMLDetailsElement && !ancestor.open) {
      const summary = Array.from(ancestor.children).find((child) => child.tagName === 'SUMMARY')
      if (!summary?.contains(element)) return false
    }
  }
  return true
}

/** Owns only DOM navigation, focus and scroll locking; preferences remain session-owned. */
export function useSettingsNavigation() {
  const [settingsOpen, setSettingsOpen] = useState(() => Boolean(window.history.state?.[SETTINGS_HISTORY_KEY]))
  const [settingsPage, setSettingsPage] = useState<SettingsPage>(readPage)
  const pageRef = useRef(settingsPage)
  const pageOwnsHistory = useRef(false)
  const pageInvoker = useRef<HTMLElement | null>(null)
  const settingsButtonRef = useRef<HTMLButtonElement | null>(null)
  const settingsPanelRef = useRef<HTMLElement | null>(null)
  const settingsCloseButtonRef = useRef<HTMLButtonElement | null>(null)
  const historyActive = useRef(settingsOpen)
  const [historyToken] = useState(() => `settings-${crypto.randomUUID()}`)
  const historyOwned = useRef(false)
  const traversalPending = useRef(false)
  const reopenQueued = useRef(false)
  const closeQueued = useRef(false)
  const restoreTimeout = useRef<number | null>(null)

  const closeSettings = useCallback((fromHistory = false): void => {
    if (!fromHistory && traversalPending.current && historyActive.current) {
      closeQueued.current = true
      setSettingsOpen(false)
      return
    }
    const currentState = window.history.state
    const markerIsCurrent = Boolean(currentState?.[SETTINGS_HISTORY_KEY])
    const markerIsOwned = historyOwned.current && currentState?.[SETTINGS_HISTORY_KEY] === historyToken
    const shouldTraverse = !fromHistory && historyActive.current && markerIsOwned && !traversalPending.current
    if (shouldTraverse) traversalPending.current = true
    historyActive.current = fromHistory && markerIsCurrent
    historyOwned.current = fromHistory && markerIsOwned
    const wasDetail = pageRef.current !== 'overview' && pageOwnsHistory.current
    pageRef.current = 'overview'
    setSettingsPage('overview')
    setSettingsOpen(false)
    if (restoreTimeout.current !== null) window.clearTimeout(restoreTimeout.current)
    if (!reopenQueued.current) {
      restoreTimeout.current = window.setTimeout(() => {
        restoreTimeout.current = null
        if (!historyActive.current && !traversalPending.current) {
          const invoker = settingsButtonRef.current
          const menu = invoker?.closest('details')
          if (menu instanceof HTMLDetailsElement && !menu.open) menu.querySelector('summary')?.focus()
          else invoker?.focus()
        }
      }, 0)
    }
    if (shouldTraverse) { if (wasDetail) window.history.go(-2); else window.history.back() }
    else if (!fromHistory && markerIsCurrent && !markerIsOwned) {
      const nextState = { ...(currentState ?? {}) }
      delete nextState[SETTINGS_HISTORY_KEY]
      delete nextState[SETTINGS_PAGE_KEY]
      window.history.replaceState(nextState, '')
      historyActive.current = false
    }
  }, [historyToken])

  const openSettings = useCallback((): void => {
    if (traversalPending.current) { reopenQueued.current = true; return }
    if (historyActive.current) return
    window.history.pushState({ ...(window.history.state ?? {}), [SETTINGS_HISTORY_KEY]: historyToken, [SETTINGS_PAGE_KEY]: 'overview' }, '')
    historyActive.current = true
    historyOwned.current = true
    pageRef.current = 'overview'
    setSettingsPage('overview')
    flushSync(() => setSettingsOpen(true))
  }, [historyToken])

  const openSettingsPage = useCallback((page: Exclude<SettingsPage, 'overview'>, invoker?: HTMLElement): void => {
    if (!historyActive.current || traversalPending.current || pageRef.current !== 'overview') return
    pageInvoker.current = invoker ?? null
    const state = { ...window.history.state, [SETTINGS_PAGE_KEY]: page }
    pageOwnsHistory.current = historyOwned.current
    if (pageOwnsHistory.current) window.history.pushState(state, '')
    else window.history.replaceState(state, '')
    pageRef.current = page
    setSettingsPage(page)
  }, [])

  const backSettings = useCallback((): void => {
    if (traversalPending.current) return
    if (pageRef.current === 'overview') { closeSettings(); return }
    if (pageOwnsHistory.current) {
      traversalPending.current = true
      window.history.back()
    } else {
      window.history.replaceState({ ...window.history.state, [SETTINGS_PAGE_KEY]: 'overview' }, '')
      window.dispatchEvent(new PopStateEvent('popstate'))
    }
  }, [closeSettings])

  useEffect(() => {
    if (settingsOpen) {
      settingsPanelRef.current?.scrollTo?.(0, 0)
      settingsCloseButtonRef.current?.focus()
    }
  }, [settingsOpen, settingsPage])

  useEffect(() => {
    if (!settingsOpen) return
    const overlay = settingsPanelRef.current?.closest('.settings-overlay')
    if (!overlay?.parentElement) return
    const siblings = Array.from(overlay.parentElement.children).filter((element) => element !== overlay)
    const previous = siblings.map((element) => element.getAttribute('inert'))
    siblings.forEach((element) => element.setAttribute('inert', ''))
    return () => siblings.forEach((element, index) => {
      if (element.getAttribute('inert') !== '') return
      const value = previous[index]
      if (value == null) element.removeAttribute('inert')
      else element.setAttribute('inert', value)
    })
  }, [settingsOpen])

  useEffect(() => {
    const onPopState = (): void => {
      if (window.history.state?.[SETTINGS_HISTORY_KEY]) {
        if (closeQueued.current) {
          closeQueued.current = false
          traversalPending.current = false
          historyOwned.current = window.history.state?.[SETTINGS_HISTORY_KEY] === historyToken
          pageRef.current = readPage()
          closeSettings()
          return
        }
        const previousPage = pageRef.current
        const nextPage = readPage()
        pageRef.current = nextPage
        setSettingsPage(nextPage)
        pageOwnsHistory.current = nextPage !== 'overview' && window.history.state?.[SETTINGS_HISTORY_KEY] === historyToken
        if (previousPage !== 'overview' && nextPage === 'overview') {
          if (restoreTimeout.current !== null) window.clearTimeout(restoreTimeout.current)
          restoreTimeout.current = window.setTimeout(() => {
            restoreTimeout.current = null
            const invoker = pageInvoker.current?.isConnected ? pageInvoker.current : settingsPanelRef.current?.querySelector<HTMLElement>(`[data-settings-page="${previousPage}"]`)
            invoker?.focus()
          }, 0)
        }
        traversalPending.current = false
        historyActive.current = true
        historyOwned.current = window.history.state?.[SETTINGS_HISTORY_KEY] === historyToken
        reopenQueued.current = false
        flushSync(() => setSettingsOpen(true))
        return
      }
      closeQueued.current = false
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
    if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); backSettings(); return }
    if (event.key !== 'Tab') return
    const elements = Array.from(settingsPanelRef.current?.querySelectorAll<HTMLElement>(FOCUSABLE) ?? []).filter(isSemanticallyFocusable)
    const first = elements[0]
    const last = elements.at(-1)
    if (first === undefined || last === undefined) return
    if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus() }
    else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus() }
  }, [backSettings])

  return { settingsPage, openSettingsPage, backSettings, settingsOpen, settingsButtonRef, settingsPanelRef, settingsCloseButtonRef, openSettings, closeSettings, onSettingsKeyDown }
}

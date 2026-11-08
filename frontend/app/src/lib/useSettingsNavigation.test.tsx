import { act, useLayoutEffect } from 'react'
import { createRoot } from 'react-dom/client'
import { afterEach, expect, it, vi } from 'vitest'
import { useSettingsNavigation } from './useSettingsNavigation'

const container = document.createElement('div')
const root = createRoot(container)
let navigation: ReturnType<typeof useSettingsNavigation>
function Harness() { const value = useSettingsNavigation(); useLayoutEffect(() => { navigation = value }); return null }
function InheritedHarness() {
  const value = useSettingsNavigation()
  const { settingsButtonRef } = value
  useLayoutEffect(() => { navigation = value })
  return <details><summary>More</summary><button ref={settingsButtonRef}>Settings</button></details>
}

afterEach(() => { act(() => root.render(null)); vi.restoreAllMocks(); history.replaceState(null, '') })

it('keeps Settings open when Back returns from a detail page and restores its invoker', async () => {
  history.replaceState(null, '')
  act(() => root.render(<Harness />))
  act(() => navigation.openSettings())
  const overviewState = history.state
  const invoker = document.createElement('button'); document.body.append(invoker)
  act(() => navigation.openSettingsPage('voice', invoker))
  expect(navigation.settingsPage).toBe('voice')
  history.replaceState(overviewState, '')
  await act(async () => { window.dispatchEvent(new PopStateEvent('popstate')); await new Promise(resolve => setTimeout(resolve, 0)) })
  expect(navigation.settingsOpen).toBe(true)
  expect(navigation.settingsPage).toBe('overview')
  expect(document.activeElement).toBe(invoker)
  invoker.remove()
})

it('uses one Back step for a detail page without closing the dialog', () => {
  history.replaceState(null, '')
  act(() => root.render(<Harness />))
  act(() => navigation.openSettings())
  act(() => navigation.openSettingsPage('text'))
  const back = vi.spyOn(history, 'back').mockImplementation(() => undefined)
  act(() => navigation.backSettings())
  expect(back).toHaveBeenCalledTimes(1)
  expect(navigation.settingsOpen).toBe(true)
})

it('returns to the visible More trigger when inherited detail Settings closes inside a collapsed disclosure', async () => {
  history.replaceState({ __voxgolemSettingsOpen: 'previous-mount', __voxgolemSettingsPage: 'models' }, '')
  document.body.append(container)
  act(() => root.render(<InheritedHarness />))
  const trigger = container.querySelector('summary')!
  const focus = vi.spyOn(trigger, 'focus')
  act(() => navigation.backSettings())
  await act(async () => { navigation.closeSettings(); await new Promise(resolve => setTimeout(resolve, 0)) })
  expect(navigation.settingsOpen).toBe(false)
  expect(focus).toHaveBeenCalledTimes(1)
  expect(document.activeElement).toBe(trigger)
  container.remove()
})

it('honors a full close requested while a detail Back traversal is still pending', () => {
  history.replaceState(null, '')
  act(() => root.render(<Harness />))
  act(() => navigation.openSettings())
  const overviewState = history.state
  act(() => navigation.openSettingsPage('voice'))
  const back = vi.spyOn(history, 'back').mockImplementation(() => undefined)
  act(() => navigation.backSettings())
  act(() => navigation.closeSettings())
  history.replaceState(overviewState, '')
  act(() => window.dispatchEvent(new PopStateEvent('popstate')))
  expect(navigation.settingsOpen).toBe(false)
  expect(back).toHaveBeenCalledTimes(2)
})

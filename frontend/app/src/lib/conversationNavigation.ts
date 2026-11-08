export const CONVERSATIONS_PANEL_ID = 'conversation-navigation'

export function openConversationNavigation() {
  const panel = document.getElementById(CONVERSATIONS_PANEL_ID)
  if (panel instanceof HTMLDetailsElement) {
    panel.open = true
    panel.querySelector('select')?.focus()
  }
}

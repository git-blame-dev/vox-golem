export const SETTINGS_PAGE_TITLES = {
  overview: 'Settings',
  text: 'Text size',
  account: 'Account and connections',
  voice: 'Voice and audio',
  web: 'Web research',
  updates: 'Updates',
  advanced: 'Advanced and diagnostics',
  models: 'Model and reasoning',
} as const

export type SettingsPage = keyof typeof SETTINGS_PAGE_TITLES

export function readSettingsPage(value: unknown): SettingsPage {
  return typeof value === 'string' && Object.hasOwn(SETTINGS_PAGE_TITLES, value) ? value as SettingsPage : 'overview'
}

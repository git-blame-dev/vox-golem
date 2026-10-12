export type ControlIconState = 'ready' | 'busy' | 'listening' | 'finishing' | 'generating' | 'speaking' | 'between-turns' | 'unavailable' | 'error' | 'off'

export function ControlIcon({ kind, state }: { readonly kind: 'mic' | 'conversation' | 'speaker' | 'model' | 'send' | 'search' | 'settings' | 'finish' | 'stop' | 'stop-send'; readonly state?: ControlIconState }) {
  const paths = {
    mic: 'M9 5a3 3 0 0 1 6 0v7a3 3 0 0 1-6 0z M5 10v2a7 7 0 0 0 14 0v-2 M12 19v3 M8 22h8',
    conversation: 'M4 4h16v12H9l-5 4z M8 8h8 M8 12h6',
    speaker: 'M3 9h4l5-4v14l-5-4H3z M16 8a6 6 0 0 1 0 8 M19 5a10 10 0 0 1 0 14',
    model: 'M7 7h10v10H7z M9 2v5 M15 2v5 M9 17v5 M15 17v5 M2 9h5 M2 15h5 M17 9h5 M17 15h5',
    send: 'M3 3l18 9-18 9 4-9z M7 12h14',
    search: 'M10 3a7 7 0 1 0 0 14 7 7 0 0 0 0-14 M15 15l6 6',
    settings: 'M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8 M9 3h6l1 3 3 1 2 5-2 5-3 1-1 3H9l-1-3-3-1-2-5 2-5 3-1z',
    finish: 'M7 5a3 3 0 0 1 6 0v7a3 3 0 0 1-6 0z M3 10v2a7 7 0 0 0 14 0v-2 M10 19v3 M6 22h8 M16 5l2 2 4-5',
    stop: 'M5 5h14v14H5z',
    'stop-send': 'M2 3h6v6H2z M11 4l11 8-18 9 4-9 M8 12h14',
  }
  const marks: Record<ControlIconState, string> = {
    ready: 'M22 26l3 3 5-7',
    busy: 'M26 21a5 5 0 1 0 0 10 5 5 0 0 0 0-10 M26 23v3l2 1',
    listening: 'M22 25v3 M26 22v9 M30 24v5',
    finishing: 'M22 22h8v8h-8z M24 26l2 2 2-4',
    generating: 'M22 23h8 M24 26h6 M22 29h8',
    speaking: 'M22 25l3-3v8l-3-3z M28 23q4 3 0 6',
    'between-turns': 'M23 22v8 M29 22v8',
    unavailable: 'M22 22l8 8 M30 22l-8 8',
    error: 'M26 21l5 10H21z M26 25v2 M26 29h.01',
    off: 'M22 30l8-8',
  }
  return <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false" data-icon-kind={kind} data-icon-state={state}>
    <path d={paths[kind]} />
    {state !== undefined ? <g data-state-mark={state} transform="scale(.75)"><rect className="android-voice-controls__mark-background" x="20" y="20" width="12" height="12" rx="3" /><path d={marks[state]} /></g> : null}
  </svg>
}

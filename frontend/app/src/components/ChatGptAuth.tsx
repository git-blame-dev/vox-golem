import { Button } from '../ui/controls'
import { useEffect, useRef, useState } from 'react'
import { invokeTauriCommand } from '../lib/tauri'

type Phase = 'signed_out' | 'starting' | 'pending' | 'connected' | 'error'
type AuthState = { phase: Phase; userCode?: string }
type Action = 'start' | 'status' | 'cancel' | 'sign_out'
const phases: readonly string[] = ['signed_out', 'starting', 'pending', 'connected', 'error']
const verificationUrl = 'https://auth.openai.com/codex/device'

function parseState(value: unknown): AuthState {
  if (typeof value !== 'object' || value === null) throw new Error('Invalid auth state')
  const data = value as Record<string, unknown>
  if (typeof data['phase'] !== 'string' || !phases.includes(data['phase'])) throw new Error('Invalid phase')
  const code = data['userCode']
  if (code != null && (typeof code !== 'string' || !/^[A-Za-z0-9-]{1,32}$/.test(code))) throw new Error('Invalid code')
  if (data['verificationUrl'] != null && data['verificationUrl'] !== verificationUrl) throw new Error('Invalid verification URL')
  if (data['error'] != null && (typeof data['error'] !== 'string' || data['error'].length > 512)) throw new Error('Invalid error')
  return { phase: data['phase'] as Phase, ...(typeof code === 'string' ? { userCode: code } : {}) }
}

export function ChatGptAuth({ onChanged }: { onChanged: () => void | Promise<void> }) {
  const [state, setState] = useState<AuthState>({ phase: 'signed_out' })
  const [working, setWorking] = useState(true)
  const [message, setMessage] = useState('')
  const generation = useRef(0)
  const mounted = useRef(false)
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined)
  const changed = useRef(onChanged)
  useEffect(() => { changed.current = onChanged }, [onChanged])
  const clearPoll = () => { clearTimeout(timer.current); timer.current = undefined }

  const request = async (action: Action, epoch: number): Promise<void> => {
    const current = () => mounted.current && generation.current === epoch
    try {
      const next = parseState(await invokeTauriCommand('chatgpt_auth', { action }))
      if (!current()) return
      setState(next)
      setMessage('')
      setWorking(false)
      if (next.phase === 'starting' || next.phase === 'pending') {
        timer.current = setTimeout(() => { void request('status', epoch) }, 2000)
      }
      if (next.phase === 'connected' || (action === 'sign_out' && next.phase === 'signed_out')) {
        try { await changed.current() } catch {
          if (current()) setMessage('Could not refresh model availability. Close and reopen settings to retry.')
        }
      }
    } catch {
      if (!current()) return
      setWorking(false)
      setState({ phase: 'error' })
      setMessage('ChatGPT sign-in unavailable. Please retry.')
    }
  }

  useEffect(() => {
    mounted.current = true
    const epoch = ++generation.current
    void request('status', epoch)
    return () => { mounted.current = false; generation.current += 1; clearPoll() }
    // Hydrate only when settings mounts, not when the parent rerenders.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  const run = (action: Action) => {
    clearPoll()
    const epoch = ++generation.current
    setWorking(true)
    setMessage('')
    setState({ phase: action === 'start' ? 'starting' : state.phase })
    void request(action, epoch)
  }
  const utility = async (operation: () => Promise<unknown>, failure: string) => {
    const epoch = generation.current
    try { await operation() } catch {
      if (mounted.current && epoch === generation.current) setMessage(failure)
    }
  }
  const pending = state.phase === 'starting' || state.phase === 'pending'
  return <div className="chatgpt-auth" aria-label="ChatGPT account">
    <strong>ChatGPT</strong>
    <p role="status">{state.phase === 'connected' ? 'Connected to ChatGPT' : pending ? 'Complete sign-in in your browser.' : state.phase === 'error' ? 'ChatGPT sign-in unavailable' : 'Sign in with a device code.'}</p>
    {state.phase !== 'connected' ? <p className="settings-panel__hint">Device-code login may need enabling in your ChatGPT security settings or workspace permissions.</p> : null}
    {state.userCode && pending ? <>
      <label>Device code <input aria-label="ChatGPT device code" readOnly value={state.userCode} onFocus={(event) => event.currentTarget.select()} /></label>
      <p className="settings-panel__hint">Only enter this code if you started sign-in here. Visit auth.openai.com/codex/device in your browser.</p>
      <Button type="button" className="ui-button--utility" onClick={() => void utility(() => invokeTauriCommand('open_chatgpt_sign_in'), 'Could not open the sign-in page. Please retry.')}>Open sign-in page</Button>
      <Button type="button" className="ui-button--utility" onClick={() => void utility(() => navigator.clipboard.writeText(state.userCode!), 'Could not copy the code. Select and copy it manually.')}>Copy code</Button>
    </> : null}
    {pending ? <Button type="button" className="ui-button--utility" onClick={() => run('cancel')}>Cancel</Button> : null}
    {!pending && state.phase !== 'connected' ? <Button type="button" className="ui-button--utility" disabled={working} onClick={() => run('start')}>{state.phase === 'error' ? 'Retry' : 'Continue with ChatGPT'}</Button> : null}
    {state.phase === 'connected' || state.phase === 'error' ? <Button type="button" className="ui-button--utility" disabled={working} onClick={() => run('sign_out')}>Sign out</Button> : null}
    {message ? <p role="alert">{message}</p> : null}
  </div>
}

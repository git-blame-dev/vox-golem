import { useCallback, useEffect, useRef, useState } from 'react'
import type { JSX } from 'react'
import { invokeTauriCommand } from '../lib/tauri'

interface Settings { enabled: boolean; directory: string }
interface Clip { id: string; profile: string; model_file: string; model_revision: string; created_ms: number; confidence: number; label: 'unreviewed' | 'true_wake' | 'false_wake' }

function parseSettings(value: unknown): Settings {
  if (typeof value !== 'object' || value === null) throw new Error('Wake clip settings are invalid')
  const record = value as Record<string, unknown>
  if (typeof record['enabled'] !== 'boolean' || typeof record['directory'] !== 'string') throw new Error('Wake clip settings are invalid')
  return { enabled: record['enabled'], directory: record['directory'] }
}

function parseClips(value: unknown): Clip[] {
  if (!Array.isArray(value)) throw new Error('Wake clip list is invalid')
  return value.map((item: unknown) => {
    if (typeof item !== 'object' || item === null) throw new Error('Wake clip entry is invalid')
    const clip = item as Record<string, unknown>
    if (typeof clip['id'] !== 'string' || typeof clip['profile'] !== 'string' || typeof clip['model_file'] !== 'string' || typeof clip['model_revision'] !== 'string' || typeof clip['created_ms'] !== 'number' || typeof clip['confidence'] !== 'number' || !['unreviewed', 'true_wake', 'false_wake'].includes(String(clip['label']))) throw new Error('Wake clip entry is invalid')
    return clip as unknown as Clip
  })
}

export function WakeClipSettings(): JSX.Element {
  const [settings, setSettings] = useState<Settings | null>(null)
  const [directory, setDirectory] = useState('')
  const [clips, setClips] = useState<Clip[]>([])
  const [hasOlder, setHasOlder] = useState(false)
  const [offset, setOffset] = useState(0)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const mounted = useRef(true)
  const listRevision = useRef(0)

  const refresh = useCallback(async (page: number): Promise<Clip[] | null> => {
    const revision = ++listRevision.current
    try {
      const next = parseClips(await invokeTauriCommand('list_wake_clips', { offset: page }))
      let older = false
      if (next.length === 100) {
        try {
          older = parseClips(await invokeTauriCommand('list_wake_clips', { offset: page + 100 })).length > 0
        } catch (reason) {
          if (mounted.current && revision === listRevision.current) setError(String(reason))
        }
      }
      if (mounted.current && revision === listRevision.current) { setClips(next); setOffset(page); setHasOlder(older) }
      return next
    } catch (reason) { if (mounted.current && revision === listRevision.current) setError(String(reason)); return null }
  }, [])
  useEffect(() => {
    mounted.current = true
    void invokeTauriCommand('get_wake_clip_settings').then((value) => {
      if (!mounted.current) return
      const next = parseSettings(value)
      setSettings(next)
      setDirectory(next.directory)
      void refresh(0)
    }).catch((reason: unknown) => { if (mounted.current) setError(String(reason)) })
    return () => { mounted.current = false }
  }, [refresh])

  const save = async (enabled: boolean, path: string): Promise<void> => {
    setBusy(true)
    setError(null)
    try {
      const next = parseSettings(await invokeTauriCommand('set_wake_clip_settings', { enabled, directory: path }))
      if (mounted.current) { setSettings(next); setDirectory(next.directory); await refresh(0) }
    } catch (reason) { if (mounted.current) setError(String(reason)) }
    finally { if (mounted.current) setBusy(false) }
  }

  const label = async (clip: Clip, value: Clip['label']): Promise<void> => {
    try {
      await invokeTauriCommand('label_wake_clip', { profile: clip.profile, id: clip.id, label: value })
      if (mounted.current) setClips((current) => current.map((item) => item.id === clip.id && item.profile === clip.profile ? { ...item, label: value } : item))
    } catch (reason) { if (mounted.current) setError(String(reason)) }
  }

  const play = async (clip: Clip): Promise<void> => {
    try {
      const url = await invokeTauriCommand('play_wake_clip', { profile: clip.profile, id: clip.id })
      if (typeof url !== 'string' || !url.startsWith('data:audio/wav;base64,') || url.length > 200_000) throw new Error('Wake clip audio is invalid')
      await new Audio(url).play()
    } catch (reason) { if (mounted.current) setError(String(reason)) }
  }

  const remove = async (clip: Clip): Promise<void> => {
    try {
      await invokeTauriCommand('delete_wake_clip', { profile: clip.profile, id: clip.id })
      if (mounted.current) {
        const next = await refresh(offset)
        if (next?.length === 0 && offset > 0) await refresh(offset - 100)
      }
    } catch (reason) { if (mounted.current) setError(String(reason)) }
  }

  return <section className="settings-panel__row" aria-label="Wake clips">
    <div><strong>Wake clips</strong><p className="settings-panel__hint">Short local clips are saved when the microphone detects a wake. A folder names the detecting model, not necessarily the words spoken. No storage quota or automatic deletion.</p></div>
    {settings === null ? <p>{error ?? 'Loading wake clip settings…'}</p> : <div className="settings-panel__mic-row">
      <label><input aria-label="Save wake clips" type="checkbox" checked={settings.enabled} disabled={busy} onChange={(event) => void save(event.target.checked, settings.directory)} /> Save each detected wake</label>
      <label>Save directory<input aria-label="Wake clip directory" value={directory} onChange={(event) => setDirectory(event.target.value)} /></label>
      <button type="button" disabled={busy || directory === settings.directory} onClick={() => void save(settings.enabled, directory)}>Save directory</button>
      <button type="button" onClick={() => void refresh(offset)}>Refresh clips</button>
      <p className="settings-panel__hint">Showing up to 100 clips per page; older clips remain saved.</p>
      <div>
        <button type="button" disabled={offset === 0} onClick={() => void refresh(Math.max(0, offset - 100))}>Newer clips</button>{' '}
        <button type="button" disabled={!hasOlder} onClick={() => void refresh(offset + 100)}>Older clips</button>
      </div>
      {error !== null ? <p role="alert">{error}</p> : null}
      <ul>{clips.map((clip) => <li key={`${clip.profile}/${clip.id}`}>
        <strong>{clip.model_file}</strong> · revision {clip.model_revision} · {new Date(clip.created_ms).toLocaleString()} · score {clip.confidence.toFixed(2)}{' '}
        <select aria-label="Clip label" value={clip.label} onChange={(event) => void label(clip, event.target.value as Clip['label'])}>
          <option value="unreviewed">Unreviewed</option><option value="true_wake">True wake</option><option value="false_wake">False wake</option>
        </select>{' '}
        <button type="button" onClick={() => void play(clip)}>Play</button>{' '}
        <button type="button" onClick={() => void remove(clip)}>Delete</button>
      </li>)}</ul>
    </div>}
  </section>
}

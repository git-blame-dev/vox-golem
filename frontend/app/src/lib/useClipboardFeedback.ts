import { useCallback, useEffect, useRef, useState } from 'react'

async function writeClipboard(text: string): Promise<boolean> {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text)
      return true
    }
  } catch {
    // Restricted WebViews may still support the synchronous copy fallback.
  }
  const textarea = document.createElement('textarea')
  try {
    textarea.value = text
    textarea.setAttribute('readonly', '')
    textarea.style.position = 'fixed'
    textarea.style.opacity = '0'
    textarea.style.pointerEvents = 'none'
    document.body.appendChild(textarea)
    textarea.select()
    return typeof document.execCommand === 'function' && document.execCommand('copy')
  } catch {
    return false
  } finally {
    textarea.remove()
  }
}

/** Shared clipboard effect and short-lived success feedback for Copy actions. */
export function useClipboardFeedback() {
  const [copied, setCopied] = useState(false)
  const lifetime = useRef(0)
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null)
  useEffect(() => {
    const epoch = ++lifetime.current
    return () => {
      if (lifetime.current === epoch) lifetime.current += 1
      if (timer.current !== null) clearTimeout(timer.current)
    }
  }, [])
  const copy = useCallback(async (text: string): Promise<void> => {
    const epoch = lifetime.current
    const success = await writeClipboard(text)
    if (!success || epoch !== lifetime.current) return
    setCopied(true)
    if (timer.current !== null) clearTimeout(timer.current)
    timer.current = setTimeout(() => { timer.current = null; setCopied(false) }, 1500)
  }, [])
  return { copied, copy }
}

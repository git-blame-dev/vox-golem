/** Screen-local playback ownership. Native URL retrieval does not grant permission to play. */
export class OwnedClipPlayback {
  private generation = 0
  private live = false
  private audio: HTMLAudioElement | null = null

  activate(): void { this.live = true }
  retire(): void { this.live = false; this.replace() }

  private replace(): number {
    this.generation += 1
    if (this.audio !== null) {
      this.audio.pause()
      this.audio.removeAttribute('src')
      this.audio = null
    }
    return this.generation
  }

  async play(load: () => Promise<unknown>, fail: (reason: unknown) => void): Promise<void> {
    const generation = this.replace()
    const current = (): boolean => this.live && generation === this.generation
    try {
      const url = await load()
      if (!current()) return
      if (typeof url !== 'string' || !url.startsWith('data:audio/wav;base64,') || url.length > 200_000) throw new Error('Wake clip audio is invalid')
      const audio = new Audio(url)
      this.audio = audio
      await audio.play()
    } catch (reason) {
      if (current()) { this.replace(); fail(reason) }
    }
  }
}

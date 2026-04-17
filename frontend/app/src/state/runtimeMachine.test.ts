import { describe, expect, it } from 'vitest'
import { cueForTransition, transitionRuntimeStatus } from './runtimeMachine'

describe('transitionRuntimeStatus', () => {
  it('transitions from sleeping to listening on begin_listening', () => {
    expect(transitionRuntimeStatus('sleeping', 'begin_listening')).toBe('listening')
  })

  it('transitions from listening to processing on end_listening', () => {
    expect(transitionRuntimeStatus('listening', 'end_listening')).toBe('processing')
  })

  it('transitions typed prompt flow from sleeping to executing', () => {
    expect(transitionRuntimeStatus('sleeping', 'submit_prompt')).toBe('executing')
  })

  it('transitions to result_ready when response arrives', () => {
    expect(transitionRuntimeStatus('executing', 'response_ready')).toBe('result_ready')
  })
})

describe('cueForTransition', () => {
  it('requests start-listening cue for sleeping to listening', () => {
    expect(cueForTransition('sleeping', 'listening')).toBe('start_listening')
  })

  it('requests stop-listening cue for listening to processing', () => {
    expect(cueForTransition('listening', 'processing')).toBe('stop_listening')
  })

  it('does not request a cue for unrelated transitions', () => {
    expect(cueForTransition('processing', 'result_ready')).toBe(null)
  })
})

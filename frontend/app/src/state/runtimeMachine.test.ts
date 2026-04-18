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

  it('transitions voice prompt flow from processing to executing', () => {
    expect(transitionRuntimeStatus('processing', 'submit_prompt')).toBe('executing')
  })

  it('transitions to result_ready when response arrives', () => {
    expect(transitionRuntimeStatus('executing', 'response_ready')).toBe('result_ready')
  })

  it('transitions to error on failure', () => {
    expect(transitionRuntimeStatus('executing', 'fail')).toBe('error')
  })

  it('recovers from error back to sleeping', () => {
    expect(transitionRuntimeStatus('error', 'recover_from_error')).toBe('sleeping')
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

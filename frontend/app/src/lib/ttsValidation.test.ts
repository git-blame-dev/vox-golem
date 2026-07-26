import { describe, expect, it } from 'vitest'
import { firstNonEmptyCompletedLine, firstNonEmptyStreamingLine, isValidTtsFirstLine } from './ttsValidation'

describe('tts validation', () => {
  it('extracts the first non-empty completed line', () => {
    expect(firstNonEmptyCompletedLine('  hello there  \nsecond line')).toBe('hello there')
    expect(firstNonEmptyCompletedLine('   \nsecond line')).toBe('second line')
    expect(firstNonEmptyCompletedLine('\n\nLocal answer:\nThe result is ready.')).toBe('Local answer:')
  })

  it('validates the extracted line', () => {
    const line = firstNonEmptyCompletedLine(' hello there \nrest')
    expect(line !== null && isValidTtsFirstLine(line)).toBe(true)
  })

  it('only extracts newline-terminated lines while streaming', () => {
    expect(firstNonEmptyStreamingLine('\npartial')).toBeNull()
    expect(firstNonEmptyStreamingLine('\ncomplete\nrest')).toBe('complete')
    expect(firstNonEmptyCompletedLine('\npartial')).toBe('partial')
  })

  it('accepts ordinary provider lines without formatting-based rejection', () => {
    expect(isValidTtsFirstLine('one two three four five six seven eight nine ten eleven twelve thirteen')).toBe(true)
    expect(isValidTtsFirstLine('Answer: the requested file is ready')).toBe(true)
    expect(firstNonEmptyCompletedLine('\n\nOpenCode: The implementation is complete.\nFurther details.')).toBe('OpenCode: The implementation is complete.')
  })
})

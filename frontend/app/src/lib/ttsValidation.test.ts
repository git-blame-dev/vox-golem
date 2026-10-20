import { describe, expect, it } from 'vitest'
import { firstNonEmptyCompletedLine, firstNonEmptyStreamingLine, isValidTtsFirstLine, MAX_TTS_FIRST_PARAGRAPH_BYTES, MAX_TTS_FIRST_PARAGRAPH_WORDS } from './ttsValidation'

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
    expect(firstNonEmptyStreamingLine('complete\nrest')).toBe('complete')
    expect(firstNonEmptyStreamingLine('\ncomplete\nrest')).toBe('complete')
    expect(firstNonEmptyCompletedLine('\npartial')).toBe('partial')
  })

  it('accepts ordinary provider lines without formatting-based rejection', () => {
    expect(isValidTtsFirstLine('one two three four five six seven eight nine ten eleven twelve thirteen')).toBe(true)
    expect(isValidTtsFirstLine('Answer: the requested file is ready')).toBe(true)
    expect(firstNonEmptyCompletedLine('\n\nOpenCode: The implementation is complete.\nFurther details.')).toBe('OpenCode: The implementation is complete.')
  })

  it('extracts only the first paragraph of a multi-paragraph response', () => {
    const response = 'First paragraph answer.\n\nSecond paragraph details.'
    expect(firstNonEmptyCompletedLine(response)).toBe('First paragraph answer.')
  })

  it('allows natural 1-2 sentence paragraphs between 40 and 100 words without truncation', () => {
    const paragraph = 'This is a moderately long paragraph that has more than forty words in total, detailing several distinct architectural points about the runtime behavior and making sure it stays well under one hundred words and six hundred bytes total limit to verify proper operation.'
    const wordsCount = paragraph.split(/\s+/).length
    const bytesCount = new TextEncoder().encode(paragraph).length
    expect(wordsCount).toBeGreaterThan(40)
    expect(wordsCount).toBeLessThanOrEqual(MAX_TTS_FIRST_PARAGRAPH_WORDS)
    expect(bytesCount).toBeLessThanOrEqual(MAX_TTS_FIRST_PARAGRAPH_BYTES)
    expect(firstNonEmptyCompletedLine(paragraph)).toBe(paragraph)
  })

  it('truncates at a sentence boundary when a paragraph exceeds 100 words or 600 bytes', () => {
    const sentence1 = 'First complete sentence is here.'
    const sentence2 = 'Second complete sentence follows right after with plenty more words to push the total word count past one hundred words so that truncation at a sentence boundary is correctly triggered without clipping mid sentence improperly.'
    const filler = Array.from({ length: 90 }, () => 'word').join(' ')
    const longParagraph = `${sentence1} ${sentence2} ${filler}`
    const result = firstNonEmptyCompletedLine(longParagraph)
    expect(result).not.toBeNull()
    expect(result?.endsWith('.')).toBe(true)
    expect(result?.split(/\s+/).length).toBeLessThanOrEqual(MAX_TTS_FIRST_PARAGRAPH_WORDS)
  })

  it('streaming extraction returns null until newline arrives and returns first paragraph once newline arrives', () => {
    expect(firstNonEmptyStreamingLine('First paragraph answer.')).toBeNull()
    expect(firstNonEmptyStreamingLine('First paragraph answer.\nSecond paragraph')).toBe('First paragraph answer.')
  })
})

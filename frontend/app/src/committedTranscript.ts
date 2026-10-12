import type { Message, Turn } from './lib/appSnapshot'
import type { ChatMessage } from './types/chat'
import type { AnswerStageName, AnswerStageStatusEntry } from './components/AnswerStage'

export function canonicalMessageId(message: Pick<Message, 'id' | 'role'>): string {
  return message.id.includes(':') ? `${message.role}:${message.id}` : message.id
}

export function settleCanonicalAnswerStages(stages: readonly AnswerStageStatusEntry[], finalStage: AnswerStageName): readonly AnswerStageStatusEntry[] {
  const settled = stages.map((entry): AnswerStageStatusEntry => {
    if (entry.status !== 'running' && entry.status !== 'queued') return entry
    if (entry.stage === finalStage || (entry.stage === 'instant' && entry.status === 'running')) return { ...entry, status: 'completed' }
    return { ...entry, status: 'stale', detail: entry.detail ?? 'Not required for this answer' }
  })
  return settled.some(({ stage }) => stage === finalStage) ? settled : [...settled, { stage: finalStage, status: 'completed' }]
}

export function projectCommittedTranscript(transcript: readonly Turn[]): readonly ChatMessage[] {
  return transcript.flatMap((turn) => [
    { id: canonicalMessageId(turn.user), role: 'user' as const, content: turn.user.content.text },
    { id: canonicalMessageId(turn.answer), role: 'assistant' as const, content: turn.answer.content.text, contentKind: turn.answer.content.kind,
      answerStage: { stages: [{ stage: turn.finalStage, status: 'completed' as const }], priorVersions: [] } },
  ])
}

export function replaceCommittedTranscript(current: readonly ChatMessage[], transcript: readonly Turn[]): readonly ChatMessage[] {
  return [
    ...current.filter((message) => message.role === 'system'),
    ...projectCommittedTranscript(transcript).map((message) => {
      const previous = current.find((candidate) => candidate.id === message.id && candidate.role === message.role)
      if (previous === undefined || message.answerStage === undefined || previous.content !== message.content || (previous.contentKind ?? 'text') !== message.contentKind) return message
      const finalStage = message.answerStage.stages[0]!.stage
      return { ...message, ...(previous.customDirectTimings === undefined ? {} : { customDirectTimings: previous.customDirectTimings }),
        answerStage: { ...message.answerStage, stages: settleCanonicalAnswerStages(previous.answerStage?.stages ?? [], finalStage), priorVersions: previous.answerStage?.priorVersions ?? [], sources: previous.answerStage?.sources ?? [] } }
    }),
  ]
}

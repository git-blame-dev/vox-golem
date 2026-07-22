import type { ReactNode } from 'react'

export type AnswerStageName = 'instant' | 'deep' | 'review'
export type AnswerStageStatus =
  | 'queued'
  | 'running'
  | 'completed'
  | 'kept'
  | 'corrected'
  | 'cancelled'
  | 'failed'
  | 'stale'

export interface AnswerStageStatusEntry {
  readonly stage: AnswerStageName
  readonly status: AnswerStageStatus
  readonly detail?: string
}

export interface AnswerPriorVersion {
  readonly id: string
  readonly text: string
  readonly label?: string
}

export interface AnswerStageProps {
  readonly answer: string
  readonly stages: readonly AnswerStageStatusEntry[]
  readonly priorVersions?: readonly AnswerPriorVersion[]
  readonly className?: string
}

const stageLabels: Record<AnswerStageName, string> = {
  instant: 'Instant',
  deep: 'Deep',
  review: 'Review',
}

const statusLabels: Record<AnswerStageStatus, string> = {
  queued: 'queued',
  running: 'running',
  completed: 'completed',
  kept: 'kept',
  corrected: 'corrected',
  cancelled: 'cancelled',
  failed: 'failed',
  stale: 'stale',
}

function PlainText({ children }: { readonly children: string }): ReactNode {
  return children
}

export function AnswerStage({ answer, stages, priorVersions = [], className }: AnswerStageProps) {
  const rootClassName = className === undefined ? 'answer-stage' : `answer-stage ${className}`

  return (
    <section className={rootClassName} aria-label="Current answer">
      <h2>Current answer</h2>
      <p className="answer-stage__answer"><PlainText>{answer}</PlainText></p>

      <ul className="answer-stage__statuses" aria-label="Answer stage status">
        {stages.map(({ stage, status, detail }) => (
          <li key={stage} data-stage={stage} data-status={status}>
            <span>{stageLabels[stage]}</span>
            <span aria-label={`${stageLabels[stage]} status: ${statusLabels[status]}`}>
              {statusLabels[status]}
            </span>
            {detail === undefined ? null : <span> — <PlainText>{detail}</PlainText></span>}
          </li>
        ))}
      </ul>

      {priorVersions.length === 0 ? null : (
        <details className="answer-stage__history">
          <summary>Prior answer versions ({priorVersions.length})</summary>
          <ol>
            {priorVersions.map(({ id, text, label }) => (
              <li key={id}>
                <h3>{label ?? `Version ${id}`}</h3>
                <p><PlainText>{text}</PlainText></p>
              </li>
            ))}
          </ol>
        </details>
      )}
    </section>
  )
}

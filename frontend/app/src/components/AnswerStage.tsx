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
export interface AnswerSource { readonly url: string; readonly title: string }

export interface AnswerStageProps {
  readonly answer: string
  readonly contentKind?: import('../types/chat').MessageContentKind
  readonly stages: readonly AnswerStageStatusEntry[]
  readonly priorVersions?: readonly AnswerPriorVersion[]
  readonly sources?: readonly AnswerSource[]
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

export function AnswerStage({ answer, contentKind = 'text', stages, priorVersions = [], sources = [], className }: AnswerStageProps) {
  const rootClassName = className === undefined ? 'answer-stage' : `answer-stage ${className}`
  const stageDetails = stages.filter(({ detail }) => typeof detail === 'string' && detail.trim() !== '')
  const hasDetails = sources.length > 0 || stageDetails.length > 0 || priorVersions.length > 0

  return (
    <section className={rootClassName} aria-label={contentKind === 'refusal' ? 'Assistant refusal' : 'Current answer'} data-content-kind={contentKind}>
      <h2>{contentKind === 'refusal' ? 'Assistant refusal' : 'Current answer'}</h2>
      <p className="answer-stage__answer"><PlainText>{answer}</PlainText></p>
      <ul className="answer-stage__statuses" aria-label="Answer stage status">
         {stages.map(({ stage, status }, index) => (
          <li key={`${stage}-${status}-${index}`} data-stage={stage} data-status={status}>
            <span>{stageLabels[stage]}</span>
            <span aria-label={`${stageLabels[stage]} status: ${statusLabels[status]}`}>
              {statusLabels[status]}
            </span>
          </li>
        ))}
      </ul>

      {hasDetails ? <details className="answer-stage__details">
        <summary>Details</summary>
        {sources.length === 0 ? null : <ul className="answer-stage__sources" aria-label="Sources">
          {sources.map((source, index) => <li key={`${source.url}-${index}`}><a href={source.url} target="_blank" rel="noreferrer">{source.title}</a></li>)}
        </ul>}
         {stageDetails.length === 0 ? null : <section aria-label="Stage details">
           {stageDetails.map(({ stage, status, detail }, index) => <p key={`${stage}-${status}-${index}`}><strong>{stageLabels[stage]} ({statusLabels[status]}):</strong> <PlainText>{detail ?? ''}</PlainText></p>)}
        </section>}
        {priorVersions.length === 0 ? null : <section aria-label="Prior answer versions">
          <ol>
            {priorVersions.map(({ id, text, label }) => (
              <li key={id}>
                <h3>{label ?? `Version ${id}`}</h3>
                <p><PlainText>{text}</PlainText></p>
              </li>
            ))}
          </ol>
        </section>}
      </details> : null}
    </section>
  )
}

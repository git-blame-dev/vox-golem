import { usePresentation } from '../sessionPolicy'
import type { SessionViewProps } from '../sessionPolicy'
import { ActionMenu } from '../ui/ActionMenu'
import { Button } from '../ui/controls'
import { ControlIcon } from './ControlIcon'
import { CONVERSATIONS_PANEL_ID } from '../lib/conversationNavigation'

export function ConversationNavigation({ session, view }: SessionViewProps) {
  const { androidAssistant, androidConversationId, shortcutStaging, resetPending, promptState, androidVoiceActive,
    headPagePending, conversationPagePending, snapshotControlsFresh } = usePresentation(session, view, 'toolbar')
  if (androidAssistant === null) return null
  const disabled = shortcutStaging || resetPending || promptState !== 'idle' || androidVoiceActive || headPagePending || !snapshotControlsFresh
  return <ActionMenu label="Chats" icon="conversation" id={CONVERSATIONS_PANEL_ID}>
    <label className="shell__conversation-picker conversation-navigation__field">
      <span><ControlIcon kind="conversation" /> Conversation</span>
      <select aria-label="Conversation" value={androidConversationId} disabled={disabled}
        onChange={(event) => void view.actions.selectAndroidConversation('select_conversation', event.target.value)}>
        <option value="0" disabled>New draft</option>
        {androidAssistant.conversations.map((conversation) => <option key={conversation.id} value={conversation.id}>{conversation.title}</option>)}
      </select>
    </label>
    {androidAssistant.conversationOffset < androidAssistant.conversationCount ? <Button disabled={conversationPagePending || disabled}
      onClick={() => void view.actions.loadMoreAndroidConversations()}>{conversationPagePending ? 'Loading conversations…' : 'More conversations'}</Button> : null}
    {androidAssistant.branchChoices.length > 0 ? <label className="conversation-navigation__field">Choose continuation
      <select aria-label="Choose continuation" value="" disabled={disabled} onChange={(event) => void view.actions.selectAndroidConversation('select_conversation_head', event.target.value)}>
        <option value="" disabled>Choose continuation</option>
        {androidAssistant.branchChoices.map((head, index) => <option key={head} value={head}>Continuation {index + 1}</option>)}
      </select>
    </label> : null}
    {androidAssistant.branchOffset < androidAssistant.branchCount ? <Button disabled={disabled}
      onClick={() => void view.actions.loadMoreAndroidHeads()}>{headPagePending ? 'Loading continuations…' : 'More continuations'}</Button> : null}
  </ActionMenu>
}

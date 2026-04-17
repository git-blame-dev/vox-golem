export function shouldSubmitComposer(eventKey: string, shiftPressed: boolean): boolean {
  return eventKey === 'Enter' && !shiftPressed
}

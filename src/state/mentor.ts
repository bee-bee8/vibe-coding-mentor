import type { MentorState } from '../types/mentor';

export function createInitialMentorState(): MentorState {
  return {
    status: 'idle',
    answer: null,
    question: null,
    selectedPath: null,
    error: null,
  };
}

export function applyMentorState(
  _state: MentorState,
  next: MentorState,
): MentorState {
  return {
    status: next.status,
    answer: next.answer,
    question: next.question,
    selectedPath: next.selectedPath,
    error: next.error,
  };
}

/**
 * Command responses only acknowledge that a request was accepted.  The
 * mentor-state event is authoritative for Loading, Available, and Error so a
 * late invoke resolution cannot overwrite a newer event.
 */
export function applyMentorInvokeResult(
  state: MentorState,
  _next: MentorState,
): MentorState {
  void _next;
  return state;
}

/**
 * Hydrate from the initial command only when no newer event arrived while the
 * invoke was in flight.  Once the subscription receives an event, that event
 * is authoritative for the corresponding state boundary.
 */
export function applyMentorInitialInvokeResult(
  state: MentorState,
  next: MentorState,
  expectedEventVersion: number,
  currentEventVersion: number,
): MentorState {
  return expectedEventVersion === currentEventVersion
    ? applyMentorState(state, next)
    : state;
}

export function createMentorError(
  state: MentorState,
  error: string,
): MentorState {
  return {
    ...state,
    status: 'error',
    answer: null,
    error,
  };
}

export function resetMentorState(): MentorState {
  return createInitialMentorState();
}

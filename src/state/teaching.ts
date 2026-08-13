import type { TeachingState } from '../types/teaching';

export function createInitialTeachingState(): TeachingState {
  return { status: 'idle', answer: null, error: null };
}

export function applyTeachingState(_current: TeachingState, next: TeachingState): TeachingState {
  return { status: next.status, answer: next.answer, error: next.error };
}

/**
 * Command responses only acknowledge that a request was accepted.  The
 * teaching-state event is authoritative for Loading, Available, and Error so
 * a late invoke resolution cannot overwrite a newer event.
 */
export function applyTeachingInvokeResult(
  state: TeachingState,
  _next: TeachingState,
): TeachingState {
  void _next;
  return state;
}

/**
 * Hydrate from the initial command only when no newer event arrived while the
 * invoke was in flight.
 */
export function applyTeachingInitialInvokeResult(
  state: TeachingState,
  next: TeachingState,
  expectedEventVersion: number,
  currentEventVersion: number,
): TeachingState {
  return expectedEventVersion === currentEventVersion
    ? applyTeachingState(state, next)
    : state;
}

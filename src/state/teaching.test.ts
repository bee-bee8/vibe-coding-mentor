import { describe, expect, it } from 'vitest';
import {
  applyTeachingInitialInvokeResult,
  applyTeachingInvokeResult,
  applyTeachingState,
  createInitialTeachingState,
} from './teaching';

describe('teaching state', () => {
  it('keeps one selected level in the current answer', () => {
    const state = applyTeachingState(createInitialTeachingState(), {
      status: 'available', answer: { explanation: 'Only beginner', level: 'beginner', generation: 2 }, error: null,
    });
    expect(state.answer?.level).toBe('beginner');
    expect(state.answer?.explanation).not.toContain('intermediate');
  });

  it('keeps an event result when the invoke promise resolves with loading later', () => {
    const available = applyTeachingState(createInitialTeachingState(), {
      status: 'available',
      answer: { explanation: 'event explanation', level: 'beginner', generation: 4 },
      error: null,
    });
    expect(
      applyTeachingInvokeResult(available, {
        status: 'loading',
        answer: null,
        error: null,
      }),
    ).toEqual(available);
  });

  it('does not let initial getTeachingState overwrite a newer event', () => {
    const eventState = applyTeachingState(createInitialTeachingState(), {
      status: 'available',
      answer: { explanation: 'new event explanation', level: 'intermediate', generation: 5 },
      error: null,
    });
    const staleInvoke = {
      status: 'loading' as const,
      answer: null,
      error: null,
    };
    expect(
      applyTeachingInitialInvokeResult(eventState, staleInvoke, 2, 3),
    ).toEqual(eventState);
    expect(
      applyTeachingInitialInvokeResult(createInitialTeachingState(), staleInvoke, 2, 2),
    ).toEqual(staleInvoke);
  });
});

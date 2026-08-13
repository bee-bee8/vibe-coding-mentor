import { describe, expect, it } from 'vitest';

import {
  applyMentorInitialInvokeResult,
  applyMentorInvokeResult,
  applyMentorState,
  createInitialMentorState,
  createMentorError,
  resetMentorState,
} from './mentor';

describe('mentor state', () => {
  it('starts idle and resets at the current-change boundary', () => {
    expect(resetMentorState()).toEqual(createInitialMentorState());
  });

  it('keeps the structured answer and selected frozen path', () => {
    const next = applyMentorState(createInitialMentorState(), {
      status: 'available',
      answer: {
        answer: 'The frozen diff shows a changed function.',
        question: 'What changed?',
        selectedPath: 'src/main.ts',
        generation: 3,
      },
      question: 'What changed?',
      selectedPath: 'src/main.ts',
      error: null,
    });
    expect(next.answer?.answer).toContain('frozen diff');
    expect(next.selectedPath).toBe('src/main.ts');
  });

  it('surfaces failures without retaining a stale answer', () => {
    const next = createMentorError(
      {
        ...createInitialMentorState(),
        answer: {
          answer: 'old',
          question: 'old?',
          selectedPath: null,
          generation: 1,
        },
      },
      'Ask Mentor was cancelled',
    );
    expect(next.status).toBe('error');
    expect(next.error).toBe('Ask Mentor was cancelled');
    expect(next.answer).toBeNull();
  });

  it('keeps an event result when the invoke promise resolves with loading later', () => {
    const available = applyMentorState(createInitialMentorState(), {
      status: 'available',
      answer: {
        answer: 'event answer',
        question: 'What changed?',
        selectedPath: null,
        generation: 4,
      },
      question: 'What changed?',
      selectedPath: null,
      error: null,
    });
    expect(
      applyMentorInvokeResult(available, {
        ...createInitialMentorState(),
        status: 'loading',
        question: 'What changed?',
      }),
    ).toEqual(available);
  });

  it('does not let initial getMentorState overwrite a newer event', () => {
    const eventState = applyMentorState(createInitialMentorState(), {
      status: 'available',
      answer: {
        answer: 'new event answer',
        question: 'What changed?',
        selectedPath: null,
        generation: 5,
      },
      question: 'What changed?',
      selectedPath: null,
      error: null,
    });
    const staleInvoke = {
      ...createInitialMentorState(),
      status: 'loading' as const,
      question: 'What changed?',
    };
    expect(
      applyMentorInitialInvokeResult(eventState, staleInvoke, 2, 3),
    ).toEqual(eventState);
    expect(
      applyMentorInitialInvokeResult(createInitialMentorState(), staleInvoke, 2, 2),
    ).toEqual(staleInvoke);
  });
});

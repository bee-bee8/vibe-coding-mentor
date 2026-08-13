import { describe, expect, it } from 'vitest';

import {
  applyLearningMemoryInvokeResult,
  applyLearningMemoryState,
  createInitialLearningMemoryState,
} from './learningMemory';

const availableState = {
  revision: 2,
  relevantConcepts: ['functions'],
  analysisGeneration: 7,
  status: 'available' as const,
  records: [{
    concept: 'functions',
    timesEncountered: 2,
    status: 'learning' as const,
    lastEncountered: '2026-08-14T01:02:03.000Z',
    projectsEncountered: ['C:/project'],
  }],
  error: null,
};

describe('learning memory state', () => {
  it('keeps the five stored memory fields and user-selected status', () => {
    const state = applyLearningMemoryState(
      createInitialLearningMemoryState(['functions'], 7),
      availableState,
    );

    expect(state.records[0]).toEqual({
      concept: 'functions',
      timesEncountered: 2,
      status: 'learning',
      lastEncountered: '2026-08-14T01:02:03.000Z',
      projectsEncountered: ['C:/project'],
    });
  });

  it('does not let an older invoke result overwrite a newer event', () => {
    const eventState = applyLearningMemoryState(
      createInitialLearningMemoryState(['functions'], 7),
      {
        ...availableState,
        revision: 3,
        records: [{ ...availableState.records[0], status: 'familiar' }],
      },
    );

    expect(
      applyLearningMemoryInvokeResult(eventState, availableState, 2, 3),
    ).toEqual(eventState);
  });

  it('hydrates from an invoke result when no event arrived first', () => {
    expect(
      applyLearningMemoryInvokeResult(
        createInitialLearningMemoryState(['functions'], 7),
        availableState,
        2,
        2,
      ),
    ).toEqual(availableState);
  });

  it('hydrates the current scope after a rejected stale event', () => {
    const reset = createInitialLearningMemoryState(['functions'], 9);
    const currentScopeResult = {
      ...availableState,
      revision: 6,
      analysisGeneration: 9,
    };

    expect(
      applyLearningMemoryInvokeResult(reset, currentScopeResult, 2, 3),
    ).toEqual(currentScopeResult);
  });

  it('keeps the newer revision when events arrive in reverse order', () => {
    const newer = applyLearningMemoryState(
      createInitialLearningMemoryState(['functions'], 7),
      {
        ...availableState,
        revision: 4,
        records: [{ ...availableState.records[0], status: 'familiar' }],
      },
    );
    const older = applyLearningMemoryState(newer, {
      ...availableState,
      revision: 3,
    });

    expect(older).toBe(newer);
    expect(older.revision).toBe(4);
    expect(older.records[0].status).toBe('familiar');
  });

  it('rejects events from a stale analysis generation or concept boundary', () => {
    const current = applyLearningMemoryState(
      createInitialLearningMemoryState(['functions'], 8),
      { ...availableState, revision: 5, analysisGeneration: 8 },
    );

    expect(
      applyLearningMemoryState(current, {
        ...availableState,
        revision: 6,
        analysisGeneration: 7,
      }),
    ).toBe(current);
    expect(
      applyLearningMemoryState(current, {
        ...availableState,
        revision: 6,
        relevantConcepts: ['loops'],
        records: [],
      }),
    ).toBe(current);
  });

  it('resets the scope so old events cannot cross a change boundary', () => {
    const reset = createInitialLearningMemoryState(['functions'], 9);
    const oldEvent = {
      ...availableState,
      revision: 99,
      analysisGeneration: 8,
    };

    expect(applyLearningMemoryState(reset, oldEvent)).toBe(reset);
    expect(reset).toEqual({
      revision: 0,
      relevantConcepts: ['functions'],
      analysisGeneration: 9,
      status: 'idle',
      records: [],
      error: null,
    });
  });

  it('keeps the latest state when concurrent updates finish out of order', () => {
    const latest = applyLearningMemoryState(
      createInitialLearningMemoryState(['functions'], 7),
      {
        ...availableState,
        revision: 8,
        records: [{ ...availableState.records[0], status: 'familiar' }],
      },
    );
    const staleConcurrentResult = applyLearningMemoryState(latest, {
      ...availableState,
      revision: 7,
      records: [{ ...availableState.records[0], status: 'new' }],
    });

    expect(staleConcurrentResult).toBe(latest);
    expect(staleConcurrentResult.records[0].status).toBe('familiar');
  });
});

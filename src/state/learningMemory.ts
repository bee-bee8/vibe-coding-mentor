import type { LearningMemoryState } from '../types/learningMemory';

export function createInitialLearningMemoryState(
  relevantConcepts: readonly string[] = [],
  analysisGeneration: number | null = null,
): LearningMemoryState {
  return {
    revision: 0,
    relevantConcepts: [...relevantConcepts],
    analysisGeneration,
    status: 'idle',
    records: [],
    error: null,
  };
}

function isCurrentScope(
  current: LearningMemoryState,
  next: LearningMemoryState,
): boolean {
  return current.analysisGeneration === next.analysisGeneration
    && current.relevantConcepts.length === next.relevantConcepts.length
    && current.relevantConcepts.every(
      (concept, index) => concept === next.relevantConcepts[index],
    );
}

export function applyLearningMemoryState(
  state: LearningMemoryState,
  next: LearningMemoryState,
): LearningMemoryState {
  if (!isCurrentScope(state, next) || next.revision < state.revision) {
    return state;
  }
  return {
    revision: next.revision,
    relevantConcepts: next.relevantConcepts,
    analysisGeneration: next.analysisGeneration,
    status: next.status,
    records: next.records,
    error: next.error,
  };
}

/**
 * Events are authoritative. Revision and scope checks keep a command result
 * from replacing a newer or unrelated event; event versions only add a
 * conservative guard when both snapshots are otherwise equivalent.
 */
export function applyLearningMemoryInvokeResult(
  state: LearningMemoryState,
  next: LearningMemoryState,
  expectedEventVersion: number,
  currentEventVersion: number,
): LearningMemoryState {
  if (expectedEventVersion !== currentEventVersion && next.revision <= state.revision) {
    return state;
  }
  return applyLearningMemoryState(state, next);
}

export function createLearningMemoryError(
  state: LearningMemoryState,
  error: string,
): LearningMemoryState {
  return { ...state, status: 'error', error };
}

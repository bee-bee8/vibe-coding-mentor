import type {
  AnalysisState,
  ChangeAnalysis,
} from '../types/analysis';

export function createInitialAnalysisState(): AnalysisState {
  return {
    status: 'idle',
    analysis: null,
    error: null,
  };
}

/** Replace the cached analysis lifecycle state without inventing missing data. */
export function applyAnalysisState(
  _state: AnalysisState,
  next: AnalysisState,
): AnalysisState {
  return {
    status: next.status,
    analysis: next.analysis,
    error: next.error,
  };
}

export function applyCompletedAnalysis(
  state: AnalysisState,
  analysis: ChangeAnalysis,
): AnalysisState {
  return {
    ...state,
    status: 'available',
    analysis,
    error: null,
  };
}

export function createAnalysisError(
  state: AnalysisState,
  error: string,
): AnalysisState {
  return {
    ...state,
    status: 'error',
    error,
  };
}

/** Project switches and stop explicitly discard the current-change analysis. */
export function resetAnalysisState(): AnalysisState {
  return createInitialAnalysisState();
}

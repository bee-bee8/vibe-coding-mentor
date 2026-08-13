import type { DiffState } from '../types/diff';

export function createInitialDiffState(): DiffState {
  return {
    projectPath: null,
    source: 'none',
    fallback: false,
    files: [],
    totalLinesAdded: 0,
    totalLinesDeleted: 0,
    unknownLineCountFiles: 0,
    error: null,
  };
}

export function applyDiffState(
  _state: DiffState,
  next: DiffState,
): DiffState {
  return {
    ...next,
    files: [...next.files].sort((left, right) =>
      left.path.localeCompare(right.path),
    ),
  };
}

export function resetDiffState(
  projectPath: string | null,
  state: DiffState = createInitialDiffState(),
): DiffState {
  return {
    ...state,
    projectPath,
    source: projectPath ? 'snapshot' : 'none',
    fallback: Boolean(projectPath),
    files: [],
    totalLinesAdded: 0,
    totalLinesDeleted: 0,
    unknownLineCountFiles: 0,
    error: null,
  };
}

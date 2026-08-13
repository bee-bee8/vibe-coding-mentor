import type {
  FileChangeRecord,
  WatcherState,
  WatcherStatus,
} from '../types/watcher';
import { createInitialDiffState, resetDiffState } from './diff';

export function createInitialWatcherState(): WatcherState {
  return {
    projectPath: null,
    status: 'idle',
    records: [],
    diff: createInitialDiffState(),
    error: null,
  };
}
/** Keep one deterministic final status per relative path. */
export function upsertChange(
  records: FileChangeRecord[],
  change: FileChangeRecord,
): FileChangeRecord[] {
  const next = records.filter((record) => record.path !== change.path);
  next.push(change);
  return next.sort((left, right) => left.path.localeCompare(right.path));
}

export function applyWatcherChange(
  state: WatcherState,
  change: FileChangeRecord,
): WatcherState {
  if (!state.projectPath) {
    return state;
  }

  return {
    ...state,
    status: 'watching',
    error: null,
    records: upsertChange(state.records, change),
  };
}

export function createWatcherError(
  state: WatcherState,
  error: string,
): WatcherState {
  return {
    ...state,
    status: 'error',
    error,
  };
}

export function resetWatcherState(
  projectPath: string | null,
  status: WatcherStatus = projectPath ? 'watching' : 'idle',
): WatcherState {
  return {
    projectPath,
    status,
    records: [],
    diff: resetDiffState(projectPath),
    error: null,
  };
}

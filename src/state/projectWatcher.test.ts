import { describe, expect, it } from 'vitest';

import {
  applyWatcherChange,
  createInitialWatcherState,
  createWatcherError,
  resetWatcherState,
  upsertChange,
} from './projectWatcher';

describe('project watcher state', () => {
  it('starts empty and idle', () => {
    expect(createInitialWatcherState()).toEqual({
      projectPath: null,
      status: 'idle',
      records: [],
      error: null,
    });
  });

  it('resets records when a project changes', () => {
    const state = {
      projectPath: 'C:/old-project',
      status: 'watching' as const,
      records: [{ path: 'old.ts', status: 'modified' as const }],
      error: null,
    };

    expect(resetWatcherState('C:/new-project')).toEqual({
      projectPath: 'C:/new-project',
      status: 'watching',
      records: [],
      error: null,
    });
    expect(resetWatcherState(null)).toEqual(createInitialWatcherState());
    expect(state.records).toHaveLength(1);
  });

  it('coalesces repeated events by keeping the final status', () => {
    let records = upsertChange([], { path: 'src/a.ts', status: 'added' });
    records = upsertChange(records, { path: 'src/a.ts', status: 'modified' });
    records = upsertChange(records, { path: 'src/b.ts', status: 'deleted' });

    expect(records).toEqual([
      { path: 'src/a.ts', status: 'modified' },
      { path: 'src/b.ts', status: 'deleted' },
    ]);
  });

  it('ignores changes when no project is selected', () => {
    expect(
      applyWatcherChange(createInitialWatcherState(), {
        path: 'a.ts',
        status: 'added',
      }),
    ).toEqual(createInitialWatcherState());
  });

  it('surfaces errors without dropping the selected project', () => {
    const state = resetWatcherState('C:/project');
    expect(createWatcherError(state, 'The folder is not readable')).toEqual({
      ...state,
      status: 'error',
      error: 'The folder is not readable',
    });
  });
});

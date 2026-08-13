import { describe, expect, it } from 'vitest';

import {
  applyDiffState,
  createInitialDiffState,
  resetDiffState,
} from './diff';

describe('diff state', () => {
  it('starts with no source and known zero totals', () => {
    expect(createInitialDiffState()).toEqual({
      projectPath: null,
      source: 'none',
      fallback: false,
      files: [],
      totalLinesAdded: 0,
      totalLinesDeleted: 0,
      unknownLineCountFiles: 0,
      error: null,
    });
  });

  it('keeps unknown line totals instead of converting them to zero', () => {
    const next = applyDiffState(createInitialDiffState(), {
      projectPath: 'C:/project',
      source: 'snapshot',
      fallback: true,
      files: [
        {
          path: 'z.bin',
          status: 'modified',
          linesAdded: null,
          linesDeleted: null,
          contentStatus: 'binary',
        },
        {
          path: 'a.ts',
          status: 'added',
          linesAdded: 2,
          linesDeleted: 0,
          contentStatus: 'text',
        },
      ],
      totalLinesAdded: null,
      totalLinesDeleted: null,
      unknownLineCountFiles: 1,
      error: 'Git is unavailable; using snapshot comparison',
    });

    expect(next.files.map((file) => file.path)).toEqual(['a.ts', 'z.bin']);
    expect(next.totalLinesAdded).toBeNull();
    expect(next.totalLinesDeleted).toBeNull();
  });

  it('resets a selected project without retaining stale records', () => {
    const state = resetDiffState('C:/project');
    expect(state.projectPath).toBe('C:/project');
    expect(state.source).toBe('snapshot');
    expect(state.fallback).toBe(true);
    expect(state.files).toEqual([]);
  });
});

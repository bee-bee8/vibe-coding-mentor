import { describe, expect, it } from 'vitest';

import type { ChangeAnalysis } from '../types/analysis';
import {
  applyAnalysisState,
  applyCompletedAnalysis,
  createAnalysisError,
  createInitialAnalysisState,
  resetAnalysisState,
} from './analysis';

const analysis: ChangeAnalysis = {
  record: {
    summary: '1 file changed since the watch-start snapshot.',
    purpose: 'Unknown: task purpose was not supplied.',
    changedComponents: ['src/app.ts'],
    keyDecisions: [],
    howItWorks: 'Mentor compares frozen snapshots.',
    impact: 'Unknown: runtime and product impact was not supplied.',
    risk: 'Unknown: risk assessment was not supplied.',
    reviewPriority: 'Unknown: review priority was not supplied.',
    programmingConcepts: [],
    relevantCodeLocations: ['src/app.ts'],
  },
  metadata: {
    projectPath: 'C:/project',
    source: 'local-snapshot',
    completion: 'explicit',
    changedFileCount: 1,
    supplied: {
      task: null,
      plan: null,
      completion: null,
      tests: null,
    },
  },
};

describe('analysis state', () => {
  it('starts idle and resets on a project lifecycle boundary', () => {
    expect(createInitialAnalysisState()).toEqual({
      status: 'idle',
      analysis: null,
      error: null,
    });
    expect(resetAnalysisState()).toEqual(createInitialAnalysisState());
  });

  it('keeps one latest analysis while later watcher state changes', () => {
    const available = applyCompletedAnalysis(createInitialAnalysisState(), analysis);
    expect(available.analysis).toEqual(analysis);
    expect(applyAnalysisState(available, {
      status: 'available',
      analysis,
      error: null,
    })).toEqual(available);
  });

  it('preserves honest unknown/null values and reports lifecycle errors', () => {
    const next = createAnalysisError(
      applyCompletedAnalysis(createInitialAnalysisState(), analysis),
      'Unable to freeze current snapshot',
    );
    expect(next.analysis?.record.impact).toContain('Unknown');
    expect(next.analysis?.metadata.supplied.task).toBeNull();
    expect(next.status).toBe('error');
    expect(next.error).toBe('Unable to freeze current snapshot');
  });
});

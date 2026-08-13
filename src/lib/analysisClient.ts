import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import {
  ANALYSIS_STATE_EVENT,
  type AnalysisState,
  type ChangeAnalysis,
  type CompletionMetadata,
} from '../types/analysis';

export async function getAnalysisState(): Promise<AnalysisState> {
  return invoke<AnalysisState>('get_analysis_state');
}

/** Mark the current frozen snapshot pair complete using the local fallback. */
export async function completeChange(
  metadata?: CompletionMetadata,
): Promise<ChangeAnalysis | null> {
  return invoke<ChangeAnalysis | null>('complete_change', {
    metadata: metadata ?? null,
  });
}

export async function subscribeToAnalysis(
  onState: (state: AnalysisState) => void,
): Promise<UnlistenFn> {
  return listen<AnalysisState>(ANALYSIS_STATE_EVENT, (event) => {
    onState(event.payload);
  });
}

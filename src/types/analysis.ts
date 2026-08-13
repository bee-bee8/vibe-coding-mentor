import type { FilePreview } from './diff';

export type AnalysisStatus = 'idle' | 'available' | 'error';

export interface CompletionMetadata {
  task?: string | null;
  plan?: string[] | null;
  completion?: string | null;
  tests?: string[] | null;
}

/** The ten canonical Change Record fields owned by WORKFLOW.md. */
export interface ChangeRecord {
  summary: string;
  purpose: string;
  changedComponents: string[];
  keyDecisions: string[];
  howItWorks: string;
  impact: string;
  risk: string;
  reviewPriority: string;
  programmingConcepts: string[];
  relevantCodeLocations: string[];
}

export interface AnalysisMetadata {
  projectPath: string;
  source: string;
  completion: string;
  changedFileCount: number;
  supplied: CompletionMetadata;
}

export interface ChangeAnalysis {
  record: ChangeRecord;
  metadata: AnalysisMetadata;
  frozenFiles: FilePreview[];
}

export interface AnalysisState {
  status: AnalysisStatus;
  analysis: ChangeAnalysis | null;
  error: string | null;
}

export const ANALYSIS_STATE_EVENT = 'analysis-state';

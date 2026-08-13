export type LearningStatus = 'new' | 'learning' | 'familiar';
export type LearningMemoryStateStatus = 'idle' | 'available' | 'error';

export interface LearningMemoryRecord {
  concept: string;
  timesEncountered: number;
  status: LearningStatus;
  lastEncountered: string;
  projectsEncountered: string[];
}

export interface LearningMemoryState {
  revision: number;
  relevantConcepts: string[];
  analysisGeneration: number | null;
  status: LearningMemoryStateStatus;
  records: LearningMemoryRecord[];
  error: string | null;
}

export const LEARNING_MEMORY_STATE_EVENT = 'learning-memory-state';

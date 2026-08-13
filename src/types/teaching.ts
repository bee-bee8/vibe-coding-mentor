export type TeachingLevel = 'beginner' | 'intermediate';
export type TeachingStatus = 'idle' | 'loading' | 'available' | 'error';

export interface TeachingAnswer {
  explanation: string;
  level: TeachingLevel;
  generation: number;
}

export interface TeachingState {
  status: TeachingStatus;
  answer: TeachingAnswer | null;
  error: string | null;
}

export const TEACHING_STATE_EVENT = 'teaching-state';

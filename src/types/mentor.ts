export type MentorStatus = 'idle' | 'loading' | 'available' | 'error';

export interface MentorAnswer {
  answer: string;
  question: string;
  selectedPath: string | null;
  generation: number;
}

export interface MentorState {
  status: MentorStatus;
  answer: MentorAnswer | null;
  question: string | null;
  selectedPath: string | null;
  error: string | null;
}

export const MENTOR_STATE_EVENT = 'mentor-state';

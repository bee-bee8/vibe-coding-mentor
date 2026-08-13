import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import {
  MENTOR_STATE_EVENT,
  type MentorState,
} from '../types/mentor';

export async function getMentorState(): Promise<MentorState> {
  return invoke<MentorState>('get_mentor_state');
}

export async function askMentor(
  question: string,
  selectedPath: string | null,
): Promise<MentorState> {
  return invoke<MentorState>('ask_mentor', {
    request: {
      question,
      selectedPath,
    },
  });
}

export async function cancelMentor(): Promise<MentorState> {
  return invoke<MentorState>('cancel_mentor');
}

export async function resetMentor(): Promise<MentorState> {
  return invoke<MentorState>('reset_mentor');
}

export async function subscribeToMentor(
  onState: (state: MentorState) => void,
): Promise<UnlistenFn> {
  return listen<MentorState>(MENTOR_STATE_EVENT, (event) => onState(event.payload));
}

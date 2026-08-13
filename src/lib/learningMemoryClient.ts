import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import {
  LEARNING_MEMORY_STATE_EVENT,
  type LearningMemoryState,
  type LearningStatus,
} from '../types/learningMemory';

export async function getLearningMemoryState(): Promise<LearningMemoryState> {
  return invoke<LearningMemoryState>('get_learning_memory_state');
}

export async function getRelevantLearningMemory(
  concepts: readonly string[],
  analysisGeneration: number,
): Promise<LearningMemoryState> {
  return invoke<LearningMemoryState>('get_relevant_learning_memory', {
    request: { concepts, analysisGeneration },
  });
}

export async function updateLearningMemoryStatus(
  concept: string,
  status: LearningStatus,
  analysisGeneration: number,
): Promise<LearningMemoryState> {
  return invoke<LearningMemoryState>('update_learning_memory_status', {
    request: { concept, status, analysisGeneration },
  });
}

export async function subscribeToLearningMemory(
  onState: (state: LearningMemoryState) => void,
): Promise<UnlistenFn> {
  return listen<LearningMemoryState>(LEARNING_MEMORY_STATE_EVENT, (event) => {
    onState(event.payload);
  });
}

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { TEACHING_STATE_EVENT, type TeachingLevel, type TeachingState } from '../types/teaching';

export const getTeachingState = () => invoke<TeachingState>('get_teaching_state');
export const teachChange = (level: TeachingLevel, selectedPath: string | null) =>
  invoke<TeachingState>('teach_change', { request: { level, selectedPath } });
export const resetTeaching = () => invoke<TeachingState>('reset_teaching');
export async function subscribeToTeaching(onState: (state: TeachingState) => void): Promise<UnlistenFn> {
  return listen<TeachingState>(TEACHING_STATE_EVENT, (event) => onState(event.payload));
}

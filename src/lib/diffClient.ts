import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import {
  DIFF_STATE_EVENT,
  type DiffState,
} from '../types/diff';

export async function getDiffState(): Promise<DiffState> {
  return invoke<DiffState>('get_diff_state');
}

export async function subscribeToDiff(
  onState: (state: DiffState) => void,
): Promise<UnlistenFn> {
  return listen<DiffState>(DIFF_STATE_EVENT, (event) => onState(event.payload));
}

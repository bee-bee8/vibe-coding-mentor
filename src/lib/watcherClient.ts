import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-dialog';

import {
  WATCHER_STATE_EVENT,
  type WatcherState,
} from '../types/watcher';

export async function getWatcherState(): Promise<WatcherState> {
  return invoke<WatcherState>('get_watcher_state');
}

export async function chooseProject(): Promise<WatcherState | null> {
  const selected = await open({
    directory: true,
    multiple: false,
    title: 'Select a project folder',
  });

  if (!selected || Array.isArray(selected)) {
    return null;
  }

  return invoke<WatcherState>('start_watching', { path: selected });
}

export async function stopWatching(): Promise<WatcherState> {
  return invoke<WatcherState>('stop_watching');
}

export async function subscribeToWatcher(
  onState: (state: WatcherState) => void,
): Promise<UnlistenFn> {
  return listen<WatcherState>(WATCHER_STATE_EVENT, (event) => onState(event.payload));
}

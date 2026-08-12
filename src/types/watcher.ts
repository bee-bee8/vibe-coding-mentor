export type WatcherStatus = 'idle' | 'watching' | 'error';

export type FileChangeStatus = 'added' | 'modified' | 'deleted';

export interface FileChangeRecord {
  path: string;
  status: FileChangeStatus;
}

export interface WatcherState {
  projectPath: string | null;
  status: WatcherStatus;
  records: FileChangeRecord[];
  error: string | null;
}

export const WATCHER_CHANGE_EVENT = 'watcher-change';
export const WATCHER_STATE_EVENT = 'watcher-state';

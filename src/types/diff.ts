export type DiffSource = 'none' | 'git' | 'snapshot';

export type ContentStatus = 'text' | 'binary' | 'unavailable';

export interface DiffFileRecord {
  path: string;
  status: 'added' | 'modified' | 'deleted';
  linesAdded: number | null;
  linesDeleted: number | null;
  contentStatus: ContentStatus;
}

export interface DiffState {
  projectPath: string | null;
  source: DiffSource;
  fallback: boolean;
  files: DiffFileRecord[];
  totalLinesAdded: number | null;
  totalLinesDeleted: number | null;
  unknownLineCountFiles: number;
  error: string | null;
}

export const DIFF_STATE_EVENT = 'diff-state';

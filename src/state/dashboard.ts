import type {
  ContentStatus,
  DiffFileRecord,
  FilePreview,
} from '../types/diff';

export interface FileSelection {
  projectPath: string;
  path: string;
}

export function deriveSelectedRecord(
  files: DiffFileRecord[],
  projectPath: string | null,
  selection: FileSelection | null,
): DiffFileRecord | null {
  if (!projectPath || !selection || selection.projectPath !== projectPath) {
    return null;
  }
  return files.find((file) => file.path === selection.path) ?? null;
}

export function resetInvalidSelection(
  files: DiffFileRecord[],
  projectPath: string | null,
  selection: FileSelection | null,
): FileSelection | null {
  return deriveSelectedRecord(files, projectPath, selection) ? selection : null;
}

export function lineTotalLabel(value: number | null): string {
  return value === null ? 'Unknown' : String(value);
}

export function contentStatusLabel(status: ContentStatus): string {
  if (status === 'text') return 'Text';
  if (status === 'binary') return 'Binary';
  return 'Unavailable';
}

export type PreviewView =
  | { kind: 'empty' }
  | { kind: 'loading' }
  | { kind: 'text'; before: string | null; after: string | null }
  | { kind: 'unavailable'; contentStatus: ContentStatus }
  | { kind: 'error'; message: string };

export function derivePreviewView(
  preview: FilePreview | null,
  loading: boolean,
  error: string | null,
): PreviewView {
  if (loading) return { kind: 'loading' };
  if (error) return { kind: 'error', message: error };
  if (!preview) return { kind: 'empty' };
  if (preview.contentStatus !== 'text') {
    return { kind: 'unavailable', contentStatus: preview.contentStatus };
  }
  return {
    kind: 'text',
    before: preview.before,
    after: preview.after,
  };
}

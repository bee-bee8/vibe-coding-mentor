import { describe, expect, it } from 'vitest';

import type { DiffFileRecord, FilePreview } from '../types/diff';
import {
  contentStatusLabel,
  derivePreviewView,
  deriveSelectedRecord,
  findFrozenFilePreview,
  lineTotalLabel,
  resetInvalidSelection,
} from './dashboard';

const files: DiffFileRecord[] = [
  {
    path: 'src/app.ts',
    status: 'modified',
    linesAdded: 2,
    linesDeleted: 1,
    contentStatus: 'text',
  },
  {
    path: 'assets/logo.bin',
    status: 'added',
    linesAdded: null,
    linesDeleted: null,
    contentStatus: 'binary',
  },
];

describe('dashboard view state', () => {
  it('derives a selected record only for the active project and current change', () => {
    const selection = { projectPath: 'C:/project', path: 'src/app.ts' };
    expect(deriveSelectedRecord(files, 'C:/project', selection)).toEqual(files[0]);
    expect(deriveSelectedRecord(files, 'C:/other', selection)).toBeNull();
    expect(
      resetInvalidSelection(files, 'C:/project', {
        projectPath: 'C:/project',
        path: 'gone.ts',
      }),
    ).toBeNull();
  });

  it('keeps nullable totals visibly unknown', () => {
    expect(lineTotalLabel(4)).toBe('4');
    expect(lineTotalLabel(null)).toBe('Unknown');
    expect(contentStatusLabel('binary')).toBe('Binary');
    expect(contentStatusLabel('unavailable')).toBe('Unavailable');
  });

  it('derives text, non-preview, loading, and error states', () => {
    const preview: FilePreview = {
      path: 'src/app.ts',
      status: 'modified',
      contentStatus: 'text',
      before: 'old\n',
      after: 'new\n',
    };
    expect(derivePreviewView(preview, false, null)).toEqual({
      kind: 'text',
      before: 'old\n',
      after: 'new\n',
    });
    expect(
      derivePreviewView(
        { ...preview, status: 'added', before: null, after: 'new\n' },
        false,
        null,
      ),
    ).toEqual({ kind: 'text', before: null, after: 'new\n' });
    expect(
      derivePreviewView(
        { ...preview, contentStatus: 'binary', before: null, after: null },
        false,
        null,
      ),
    ).toEqual({ kind: 'unavailable', contentStatus: 'binary' });
    expect(derivePreviewView(null, true, null)).toEqual({ kind: 'loading' });
    expect(derivePreviewView(null, false, 'Selected file is gone')).toEqual({
      kind: 'error',
      message: 'Selected file is gone',
    });
  });

  it('selects an exact completed preview without following a later live edit', () => {
    const completed: FilePreview = {
      path: 'src/app.ts',
      status: 'modified',
      contentStatus: 'text',
      before: 'before\n',
      after: 'completed\n',
    };
    const later: FilePreview = {
      ...completed,
      before: 'completed\n',
      after: 'later\n',
    };

    // The live list may now contain a same-path edit, but the completed
    // analysis still resolves against its own frozen list.
    expect(findFrozenFilePreview([completed], 'src/app.ts')).toEqual(completed);
    expect(findFrozenFilePreview([completed], 'src/app.ts')).not.toEqual(later);
    expect(findFrozenFilePreview([completed], 'src/other.ts')).toBeNull();
  });
});

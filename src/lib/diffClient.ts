import { invoke } from '@tauri-apps/api/core';

import type { DiffState, FilePreview } from '../types/diff';

export async function getDiffState(): Promise<DiffState> {
  return invoke<DiffState>('get_diff_state');
}

export async function getFilePreview(path: string): Promise<FilePreview> {
  return invoke<FilePreview>('get_file_preview', { path });
}

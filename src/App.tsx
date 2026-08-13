import { useEffect, useState } from 'react';

import {
  completeChange,
  getAnalysisState,
  subscribeToAnalysis,
} from './lib/analysisClient';
import {
  chooseProject,
  getWatcherState,
  stopWatching,
  subscribeToWatcher,
} from './lib/watcherClient';
import { getFilePreview } from './lib/diffClient';
import {
  contentStatusLabel,
  derivePreviewView,
  deriveSelectedRecord,
  findFrozenFilePreview,
  lineTotalLabel,
  resetInvalidSelection,
  type FileSelection,
} from './state/dashboard';
import {
  createInitialWatcherState,
  createWatcherError,
} from './state/projectWatcher';
import {
  createAnalysisError,
  createInitialAnalysisState,
} from './state/analysis';
import type { AnalysisState, ChangeAnalysis } from './types/analysis';
import type { DiffFileRecord, DiffState, FilePreview } from './types/diff';
import type { WatcherState } from './types/watcher';

function statusLabel(status: WatcherState['status']): string {
  if (status === 'error') return 'Error';
  if (status === 'watching') return 'Watching';
  return 'Idle';
}

function sourceLabel(diff: DiffState): string {
  if (diff.source === 'git' && !diff.fallback) return 'Git change data';
  if (diff.source === 'snapshot' || diff.fallback) return 'Snapshot fallback';
  return 'No change source';
}

function countLabel(value: number | null, sign: '+' | '-'): string {
  return value === null ? '?' : `${sign}${value}`;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function fileMetaLabel(file: DiffFileRecord): string {
  return `${contentStatusLabel(file.contentStatus)} · ${countLabel(file.linesAdded, '+')} / ${countLabel(file.linesDeleted, '-')}`;
}

function PreviewContent({ view }: { view: ReturnType<typeof derivePreviewView> }) {
  switch (view.kind) {
    case 'loading':
      return <p className="preview-message">Loading the selected file snapshot...</p>;
    case 'error':
      return (
        <div className="preview-message preview-warning">
          <strong>Preview unavailable</strong>
          <p>{view.message}</p>
        </div>
      );
    case 'unavailable':
      return (
        <div className="preview-message preview-warning">
          <strong>No text preview</strong>
          <p>
            This {view.contentStatus === 'binary' ? 'binary file' : 'file could not be read'}
            {' '}has unknown line counts. The change remains listed for review.
          </p>
        </div>
      );
    case 'empty':
      return <p className="preview-message">Select a changed file to inspect its contents.</p>;
    case 'text':
      return (
        <div className="diff-columns">
          <div className="diff-side">
            <div className="diff-side-heading">Before (watch start)</div>
            <pre>{view.before ?? <span className="empty-side">(empty)</span>}</pre>
          </div>
          <div className="diff-side diff-side-after">
            <div className="diff-side-heading">After (current)</div>
            <pre>{view.after ?? <span className="empty-side">(empty)</span>}</pre>
          </div>
        </div>
      );
  }
  return null;
}

type EngineerExplanationProps = {
  analysis: ChangeAnalysis;
  projectPath: string | null;
  setSelection: (selection: FileSelection) => void;
};

function EngineerText({ value }: { value: string }) {
  return (
    <p>
      {value.trim() ? value : <span className="engineer-empty">Not supplied</span>}
    </p>
  );
}

function EngineerList({ items }: { items: readonly string[] | null | undefined }) {
  if (!items || items.length === 0) {
    return <p className="engineer-empty">Not supplied</p>;
  }

  return (
    <ul className="engineer-list">
      {items.map((item, index) => (
        <li key={`${index}-${item}`}>
          {item || <span className="engineer-empty">(empty)</span>}
        </li>
      ))}
    </ul>
  );
}

function EngineerExplanation({
  analysis,
  projectPath,
  setSelection,
}: EngineerExplanationProps) {
  const activeProjectPath = projectPath;
  const { record } = analysis;
  const suppliedTests = analysis.metadata?.supplied?.tests;

  return (
    <section className="panel engineer-panel" aria-label="Engineer explanation">
      <div className="panel-heading engineer-panel-heading">
        <div>
          <span className="section-label">Current explanation</span>
          <h2>Engineer</h2>
        </div>
        <div className="engineer-mode-strip" aria-label="Explanation modes">
          <span className="engineer-mode engineer-mode-active">Engineer</span>
          <span className="engineer-mode engineer-mode-inert">Beginner (future)</span>
          <span className="engineer-mode engineer-mode-inert">Intermediate (future)</span>
        </div>
      </div>

      <div className="engineer-content">
        <div className="engineer-priority">
          <span className="engineer-priority-label">Review priority</span>
          <strong className="engineer-priority-value">
            {record.reviewPriority.trim() ? record.reviewPriority : 'Not supplied'}
          </strong>
        </div>

        <div className="engineer-fields">
          <article className="engineer-field engineer-field-wide">
            <h3>Summary</h3>
            <EngineerText value={record.summary} />
          </article>
          <article className="engineer-field">
            <h3>Purpose</h3>
            <EngineerText value={record.purpose} />
          </article>
          <article className="engineer-field">
            <h3>How it works / architecture &amp; data flow</h3>
            <EngineerText value={record.howItWorks} />
          </article>
          <article className="engineer-field">
            <h3>Changed components / important files</h3>
            <EngineerList items={record.changedComponents} />
          </article>
          <article className="engineer-field">
            <h3>Key decisions</h3>
            <EngineerList items={record.keyDecisions} />
          </article>
          <article className="engineer-field">
            <h3>Impact</h3>
            <EngineerText value={record.impact} />
          </article>
          <article className="engineer-field">
            <h3>Risk</h3>
            <EngineerText value={record.risk} />
          </article>
          <article className="engineer-field">
            <h3>Programming concepts</h3>
            <EngineerList items={record.programmingConcepts} />
          </article>
          <article className="engineer-field">
            <h3>Tests</h3>
            <EngineerList items={suppliedTests} />
          </article>
        </div>

        <div className="engineer-field engineer-code-locations">
          <h3>Relevant code locations</h3>
          {record.relevantCodeLocations.length === 0 ? (
            <p className="engineer-empty">Not supplied</p>
          ) : (
            <ul className="engineer-code-reference-list">
              {record.relevantCodeLocations.map((location, index) => {
                const frozenPreview = findFrozenFilePreview(analysis.frozenFiles, location);
                const isSelectable = activeProjectPath !== null && frozenPreview !== null;
                return (
                  <li key={`${index}-${location}`}>
                    {isSelectable ? (
                      <button
                        type="button"
                        className="engineer-code-reference"
                        onClick={() => {
                          if (activeProjectPath !== null) {
                            setSelection({
                              projectPath: activeProjectPath,
                              path: location,
                              source: 'completed',
                            });
                          }
                        }}
                      >
                        <code>{location}</code>
                        <span>View frozen before / after</span>
                      </button>
                    ) : (
                      <span className="engineer-code-reference engineer-code-reference-static">
                        <code>{location}</code>
                      </span>
                    )}
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      </div>
    </section>
  );
}

export default function App() {
  const [state, setState] = useState<WatcherState>(createInitialWatcherState);
  const [isSelecting, setIsSelecting] = useState(false);
  const [selection, setSelection] = useState<FileSelection | null>(null);
  const [preview, setPreview] = useState<FilePreview | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [isPreviewLoading, setIsPreviewLoading] = useState(false);
  const [analysisState, setAnalysisState] = useState<AnalysisState>(
    createInitialAnalysisState,
  );
  const [isCompleting, setIsCompleting] = useState(false);

  useEffect(() => {
    let mounted = true;
    let removeListener: (() => void) | undefined;

    const connect = async () => {
      try {
        const nextRemoveListener = await subscribeToWatcher(
          (nextState) => mounted && setState(nextState),
        );
        if (!mounted) {
          nextRemoveListener();
          return;
        }
        removeListener = nextRemoveListener;
        const currentState = await getWatcherState();
        if (mounted) setState(currentState);
      } catch (error) {
        if (mounted) {
          setState((current) => createWatcherError(current, errorMessage(error)));
        }
      }
    };

    void connect();
    return () => {
      mounted = false;
      removeListener?.();
    };
  }, []);

  useEffect(() => {
    let mounted = true;
    let removeListener: (() => void) | undefined;

    const connect = async () => {
      try {
        const nextRemoveListener = await subscribeToAnalysis(
          (nextState) => mounted && setAnalysisState(nextState),
        );
        if (!mounted) {
          nextRemoveListener();
          return;
        }
        removeListener = nextRemoveListener;
        const currentState = await getAnalysisState();
        if (mounted) setAnalysisState(currentState);
      } catch (error) {
        if (mounted) {
          setAnalysisState((current) =>
            createAnalysisError(current, errorMessage(error)),
          );
        }
      }
    };

    void connect();
    return () => {
      mounted = false;
      removeListener?.();
    };
  }, []);

  const selectedLiveRecord =
    selection?.source === 'completed'
      ? null
      : deriveSelectedRecord(state.diff.files, state.projectPath, selection);
  const selectedFrozenPreview =
    selection?.source === 'completed' &&
    state.projectPath !== null &&
    selection.projectPath === state.projectPath
      ? findFrozenFilePreview(
          analysisState.analysis?.frozenFiles ?? [],
          selection.path,
        )
      : null;
  const selectedRecord = selectedLiveRecord ?? selectedFrozenPreview;
  const nextSelection =
    selection?.source === 'completed'
      ? selectedFrozenPreview
        ? selection
        : null
      : resetInvalidSelection(state.diff.files, state.projectPath, selection);
  if (nextSelection !== selection) {
    setSelection(nextSelection);
  }

  const selectedPath = selectedRecord?.path ?? null;
  useEffect(() => {
    let active = true;
    if (!state.projectPath || !selectedPath || selectedFrozenPreview) {
      setPreview(selectedFrozenPreview);
      setPreviewError(null);
      setIsPreviewLoading(false);
      return () => {
        active = false;
      };
    }

    setPreview(null);
    setPreviewError(null);
    setIsPreviewLoading(true);
    void getFilePreview(selectedPath)
      .then((nextPreview) => {
        if (active) setPreview(nextPreview);
      })
      .catch((error: unknown) => {
        if (active) setPreviewError(errorMessage(error));
      })
      .finally(() => {
        if (active) setIsPreviewLoading(false);
      });

    return () => {
      active = false;
    };
  }, [selectedFrozenPreview, selectedPath, state.diff.files, state.projectPath]);

  const previewView = derivePreviewView(
    selectedRecord && (selectedFrozenPreview ?? (preview?.path === selectedPath ? preview : null)),
    selectedRecord ? isPreviewLoading : false,
    selectedRecord ? previewError : null,
  );

  const selectProject = async () => {
    setIsSelecting(true);
    try {
      const selected = await chooseProject();
      if (selected) {
        setState(selected);
        setAnalysisState(createInitialAnalysisState());
      }
    } catch (error) {
      setState((current) => createWatcherError(current, errorMessage(error)));
    } finally {
      setIsSelecting(false);
    }
  };

  const clearProject = async () => {
    try {
      setState(await stopWatching());
      setAnalysisState(createInitialAnalysisState());
    } catch (error) {
      setState((current) => createWatcherError(current, errorMessage(error)));
    }
  };

  const finishChange = async () => {
    setIsCompleting(true);
    try {
      await completeChange();
    } catch (error) {
      setAnalysisState((current) =>
        createAnalysisError(current, errorMessage(error)),
      );
    } finally {
      setIsCompleting(false);
    }
  };

  return (
    <main className="app-shell">
      <header className="app-header">
        <div>
          <p className="eyebrow">Codex Mentor</p>
          <h1>Current change</h1>
          <p className="subtitle">
            Inspect the files changed since this watch session started.
          </p>
        </div>
        <span className={`status-pill status-${state.status}`}>
          <span className="status-dot" aria-hidden="true" />
          {statusLabel(state.status)}
        </span>
      </header>

      <div className="dashboard-grid">
        <section className="panel project-panel" aria-label="Project and current change">
          <div className="panel-heading">
            <div>
              <span className="section-label">Project / Current change</span>
              <h2>{state.projectPath ? 'Watching this project' : 'No project selected'}</h2>
            </div>
            <span className="source-label">{sourceLabel(state.diff)}</span>
          </div>
          <p className="project-path">
            {state.projectPath ?? 'Choose a project folder to begin.'}
          </p>
          {state.diff.error && <p className="error-message">{state.diff.error}</p>}
          {state.error && state.error !== state.diff.error && (
            <p className="error-message">{state.error}</p>
          )}
          <div className="project-actions">
            <button type="button" onClick={() => void selectProject()} disabled={isSelecting}>
              {isSelecting
                ? 'Opening...'
                : state.projectPath
                  ? 'Switch project'
                  : 'Choose project'}
            </button>
            {state.projectPath && (
              <button type="button" className="secondary" onClick={() => void clearProject()}>
                Stop watching
              </button>
            )}
            {state.projectPath && state.diff.files.length > 0 && (
              <button
                type="button"
                className="secondary"
                onClick={() => void finishChange()}
                disabled={isCompleting}
              >
                {isCompleting ? 'Completing...' : 'Complete change'}
              </button>
            )}
          </div>
          {analysisState.error && (
            <p className="error-message">{analysisState.error}</p>
          )}
        </section>

        {analysisState.analysis && (
          <EngineerExplanation
            analysis={analysisState.analysis}
            projectPath={state.projectPath}
            setSelection={setSelection}
          />
        )}

        <section className="panel files-panel" aria-label="Changed files">
          <div className="panel-heading">
            <div>
              <span className="section-label">Changed files</span>
              <h2>
                {state.diff.files.length
                  ? `${state.diff.files.length} file${state.diff.files.length === 1 ? '' : 's'}`
                  : 'No changes yet'}
              </h2>
            </div>
            <span className="hint">Select a row to inspect it</span>
          </div>
          {state.diff.files.length > 0 ? (
            <ul className="change-list">
              {state.diff.files.map((file) => {
                const isSelected = selectedRecord?.path === file.path;
                return (
                  <li key={file.path}>
                    <button
                      type="button"
                      className={`file-row${isSelected ? ' file-row-selected' : ''}`}
                      aria-pressed={isSelected}
                      onClick={() => {
                        if (state.projectPath) {
                          setSelection({ projectPath: state.projectPath, path: file.path });
                        }
                      }}
                    >
                      <span className={`change-badge change-${file.status}`}>
                        {file.status}
                      </span>
                      <code>{file.path}</code>
                      <span className="file-meta">{fileMetaLabel(file)}</span>
                    </button>
                  </li>
                );
              })}
            </ul>
          ) : (
            <div className="empty-state">
              <span className="empty-icon" aria-hidden="true">~</span>
              <p>Edits, new files, and deleted files will appear here.</p>
            </div>
          )}
        </section>

        <aside className="panel stats-panel" aria-label="Change statistics">
          <div className="panel-heading">
            <div>
              <span className="section-label">+ / - statistics</span>
              <h2>Lines changed</h2>
            </div>
          </div>
          <div className="stats-grid">
            <div className="stat-block">
              <span className="stat-label">Added</span>
              <strong className="stat-value stat-added">
                {state.diff.totalLinesAdded === null
                  ? 'Unknown'
                  : `+${state.diff.totalLinesAdded}`}
              </strong>
            </div>
            <div className="stat-block">
              <span className="stat-label">Deleted</span>
              <strong className="stat-value stat-deleted">
                {state.diff.totalLinesDeleted === null
                  ? 'Unknown'
                  : `-${state.diff.totalLinesDeleted}`}
              </strong>
            </div>
          </div>
          {state.diff.unknownLineCountFiles > 0 && (
            <p className="unknown-notice">
              {state.diff.unknownLineCountFiles} file
              {state.diff.unknownLineCountFiles === 1 ? '' : 's'} have unknown line counts.
            </p>
          )}
          <p className="stats-footnote">
            Counts compare the watch-start snapshot with current content.
            {state.diff.totalLinesAdded === null || state.diff.totalLinesDeleted === null
              ? ` Added: ${lineTotalLabel(state.diff.totalLinesAdded)}; deleted: ${lineTotalLabel(state.diff.totalLinesDeleted)}.`
              : ''}
          </p>
        </aside>

        <section className="panel preview-panel" aria-label="Selected file before and after">
          <div className="panel-heading">
            <div>
              <span className="section-label">Before / After</span>
              <h2>{selectedRecord?.path ?? 'File preview'}</h2>
            </div>
            {selectedRecord && (
              <span className="preview-meta">
                {selectedRecord.status} · {contentStatusLabel(selectedRecord.contentStatus)}
              </span>
            )}
          </div>
          {selectedLiveRecord && (
            <p className="preview-stats">
              {fileMetaLabel(selectedLiveRecord)}
            </p>
          )}
          <PreviewContent view={previewView} />
        </section>
      </div>
    </main>
  );
}

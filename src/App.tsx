import { useEffect, useState } from 'react';

import {
  applyWatcherChange,
  createInitialWatcherState,
  createWatcherError,
} from './state/projectWatcher';
import {
  chooseProject,
  getWatcherState,
  subscribeToWatcher,
  stopWatching,
} from './lib/watcherClient';
import type { WatcherState } from './types/watcher';

function statusLabel(state: WatcherState): string {
  if (state.status === 'error') return 'Error';
  if (state.status === 'watching') return 'Watching';
  return 'No project selected';
}

export default function App() {
  const [state, setState] = useState<WatcherState>(createInitialWatcherState);
  const [isSelecting, setIsSelecting] = useState(false);

  useEffect(() => {
    let mounted = true;
    let removeListeners: (() => void) | undefined;

    const connect = async () => {
      try {
        removeListeners = await subscribeToWatcher(
          (nextState) => mounted && setState(nextState),
          (change) =>
            mounted && setState((current) => applyWatcherChange(current, change)),
        );
        const currentState = await getWatcherState();
        if (mounted) setState(currentState);
      } catch (error) {
        if (mounted) {
          setState((current) =>
            createWatcherError(
              current,
              error instanceof Error ? error.message : String(error),
            ),
          );
        }
      }
    };

    void connect();
    return () => {
      mounted = false;
      removeListeners?.();
    };
  }, []);

  const selectProject = async () => {
    setIsSelecting(true);
    try {
      const selected = await chooseProject();
      if (selected) setState(selected);
    } catch (error) {
      setState((current) =>
        createWatcherError(
          current,
          error instanceof Error ? error.message : String(error),
        ),
      );
    } finally {
      setIsSelecting(false);
    }
  };

  const clearProject = async () => {
    try {
      setState(await stopWatching());
    } catch (error) {
      setState((current) =>
        createWatcherError(
          current,
          error instanceof Error ? error.message : String(error),
        ),
      );
    }
  };

  return (
    <main className="app-shell">
      <header className="app-header">
        <div>
          <p className="eyebrow">Codex Mentor</p>
          <h1>Project watcher</h1>
          <p className="subtitle">
            See file changes as they happen in the project you are working on.
          </p>
        </div>
        <span className={`status-pill status-${state.status}`}>
          <span className="status-dot" aria-hidden="true" />
          {statusLabel(state)}
        </span>
      </header>

      <section className="project-card" aria-label="Selected project">
        <div className="project-copy">
          <span className="section-label">Selected project</span>
          <strong>{state.projectPath ?? 'Choose a project folder to begin'}</strong>
          {state.error && <p className="error-message">{state.error}</p>}
        </div>
        <div className="project-actions">
          <button
            type="button"
            onClick={() => void selectProject()}
            disabled={isSelecting}
          >
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
        </div>
      </section>

      <section className="changes-card" aria-label="Changed files">
        <div className="section-heading">
          <div>
            <span className="section-label">Live changes</span>
            <h2>
              {state.records.length
                ? `${state.records.length} changed file${state.records.length === 1 ? '' : 's'}`
                : 'No changes yet'}
            </h2>
          </div>
          <span className="hint">Paths are relative to the project root</span>
        </div>

        {state.records.length > 0 ? (
          <ul className="change-list">
            {state.records.map((record) => (
              <li key={record.path}>
                <span className={`change-badge change-${record.status}`}>
                  {record.status}
                </span>
                <code>{record.path}</code>
              </li>
            ))}
          </ul>
        ) : (
          <div className="empty-state">
            <span className="empty-icon" aria-hidden="true">
              ~
            </span>
            <p>Edits, new files, and deleted files will appear here.</p>
          </div>
        )}
      </section>
    </main>
  );
}

import { useEffect, useRef, useState } from 'react';

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
import {
  applyMentorInitialInvokeResult,
  applyMentorInvokeResult,
  applyMentorState,
  createInitialMentorState,
  createMentorError,
  resetMentorState,
} from './state/mentor';
import {
  askMentor,
  cancelMentor,
  getMentorState,
  resetMentor,
  subscribeToMentor,
} from './lib/mentorClient';
import type { AnalysisState, ChangeAnalysis } from './types/analysis';
import type { DiffFileRecord, DiffState, FilePreview } from './types/diff';
import type { LearningMemoryState, LearningStatus } from './types/learningMemory';
import type { MentorState } from './types/mentor';
import type { WatcherState } from './types/watcher';
import type { TeachingLevel, TeachingState } from './types/teaching';
import {
  getLearningMemoryState,
  getRelevantLearningMemory,
  subscribeToLearningMemory,
  updateLearningMemoryStatus,
} from './lib/learningMemoryClient';
import {
  applyLearningMemoryInvokeResult,
  applyLearningMemoryState,
  createInitialLearningMemoryState,
  createLearningMemoryError,
} from './state/learningMemory';
import { getTeachingState, resetTeaching, subscribeToTeaching, teachChange } from './lib/teachingClient';
import {
  applyTeachingInitialInvokeResult,
  applyTeachingInvokeResult,
  applyTeachingState,
  createInitialTeachingState,
} from './state/teaching';

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

function conceptKey(concept: string): string {
  return concept.trim().split(/\s+/).join(' ').toLowerCase();
}

function learningStatusLabel(status: LearningStatus): string {
  return status[0].toUpperCase() + status.slice(1);
}

function uniqueConcepts(concepts: readonly string[]): string[] {
  const labels = new Map<string, string>();
  concepts.forEach((concept) => {
    const key = conceptKey(concept);
    if (key && !labels.has(key)) labels.set(key, concept.trim());
  });
  return [...labels.entries()].map(([key, label]) => `${key}|${label}`);
}

function uniqueConceptKeys(concepts: readonly string[]): string[] {
  return [...new Set(concepts.map(conceptKey).filter(Boolean))].sort();
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

type AskMentorProps = {
  analysis: ChangeAnalysis;
  state: MentorState;
  question: string;
  selectedFrozenPath: string | null;
  onQuestionChange: (question: string) => void;
  onAsk: () => void;
  onCancel: () => void;
};

function AskMentorPanel({
  analysis,
  state,
  question,
  selectedFrozenPath,
  onQuestionChange,
  onAsk,
  onCancel,
}: AskMentorProps) {
  const isLoading = state.status === 'loading';
  return (
    <section className="panel mentor-panel" aria-label="Ask Mentor">
      <div className="panel-heading">
        <div>
          <span className="section-label">Current-change Q&amp;A</span>
          <h2>Ask Mentor</h2>
        </div>
        <span className="hint">Read-only, scoped to this frozen analysis</span>
      </div>
      <div className="mentor-content">
        <p className="mentor-boundary">
          Ask about the {analysis.metadata.changedFileCount === 1 ? 'changed file' : 'changed files'}
          {' '}shown in the current Change Record. Mentor will say when the supplied evidence is not enough.
        </p>
        <p className="mentor-selection">
          <span>Selected frozen file:</span>{' '}
          {selectedFrozenPath ? <code>{selectedFrozenPath}</code> : <em>None (optional)</em>}
        </p>
        <label className="mentor-question-label" htmlFor="mentor-question">
          Your question
        </label>
        <textarea
          id="mentor-question"
          value={question}
          onChange={(event) => onQuestionChange(event.target.value)}
          placeholder="What does this changed function do?"
          rows={4}
          disabled={isLoading}
        />
        <div className="mentor-actions">
          <button
            type="button"
            onClick={onAsk}
            disabled={isLoading || !question.trim()}
          >
            {isLoading ? 'Thinking...' : 'Ask Mentor'}
          </button>
          <button
            type="button"
            className="secondary"
            onClick={onCancel}
            disabled={!isLoading}
          >
            Cancel
          </button>
        </div>
        {isLoading && <p className="mentor-status">Waiting for the read-only Codex response...</p>}
        {state.error && <p className="error-message mentor-error">{state.error}</p>}
        {state.answer && state.status === 'available' && (
          <article className="mentor-answer">
            <h3>Answer</h3>
            <p>{state.answer.answer}</p>
          </article>
        )}
      </div>
    </section>
  );
}

function TeachingPanel({
  state,
  memoryState,
  concepts,
  updatingConcept,
  onTeach,
  onStatusChange,
}: {
  state: TeachingState;
  memoryState: LearningMemoryState;
  concepts: readonly string[];
  updatingConcept: string | null;
  onTeach: (level: TeachingLevel) => void;
  onStatusChange: (concept: string, status: LearningStatus) => void;
}) {
  const isLoading = state.status === 'loading';
  const memoryConcepts = uniqueConcepts(concepts);
  return (
    <section className="panel teaching-panel" aria-label="Teaching Mode">
      <div className="panel-heading"><div><span className="section-label">Teaching Mode</span><h2>Explain this change</h2></div><span className="hint">Choose one level per explanation</span></div>
      <div className="teaching-content">
        <div className="teaching-actions" aria-label="Teaching levels">
          <button type="button" onClick={() => onTeach('beginner')} disabled={isLoading}>Beginner</button>
          <button type="button" className="secondary" onClick={() => onTeach('intermediate')} disabled={isLoading}>Intermediate</button>
        </div>
        {isLoading && <p className="mentor-status">Building the selected explanation from this frozen change...</p>}
        {state.error && <p className="error-message mentor-error">{state.error}</p>}
        <div className="learning-memory" aria-label="Learning Memory">
          <div className="learning-memory-heading">
            <div>
              <span className="section-label">Learning Memory</span>
              <strong>Adjust concept status</strong>
            </div>
            <span className="hint">Your choice controls depth</span>
          </div>
          {memoryState.status === 'idle' && (
            <p className="learning-memory-message">Loading the current Change Record concepts...</p>
          )}
          {memoryState.error && <p className="error-message mentor-error">{memoryState.error}</p>}
          {memoryConcepts.length === 0 && memoryState.status !== 'idle' && (
            <p className="learning-memory-message">No programming concepts were supplied in this Change Record.</p>
          )}
          {memoryConcepts.length > 0 && memoryState.status !== 'idle' && (
            <ul className="learning-memory-list">
              {memoryConcepts.map((entry) => {
                const [concept, label] = entry.split('|');
                const record = memoryState.records.find((item) => item.concept === concept);
                const canEdit = record !== undefined;
                const selectedStatus = record?.status ?? 'new';
                return (
                  <li key={concept} className="learning-memory-row">
                    <div className="learning-memory-concept">
                      <strong>{label}</strong>
                      <span>{record ? `${record.timesEncountered} encounter${record.timesEncountered === 1 ? '' : 's'}` : 'Not encountered yet'}</span>
                    </div>
                    <label className="learning-memory-status">
                      <span className="sr-only">Status for {label}</span>
                      <select
                        aria-label={`Status for ${label}`}
                        value={selectedStatus}
                        disabled={!canEdit || updatingConcept !== null}
                        onChange={(event) => onStatusChange(concept, event.target.value as LearningStatus)}
                      >
                        <option value="new">{learningStatusLabel('new')}</option>
                        <option value="learning">{learningStatusLabel('learning')}</option>
                        <option value="familiar">{learningStatusLabel('familiar')}</option>
                      </select>
                    </label>
                  </li>
                );
              })}
            </ul>
          )}
          {memoryConcepts.some((entry) => !memoryState.records.some((record) => record.concept === entry.split('|')[0])) && (
            <p className="learning-memory-note">New concepts are recorded only after a successful explanation.</p>
          )}
        </div>
        {state.answer && state.status === 'available' && <article className="teaching-answer"><h3>{state.answer.level} explanation</h3><p>{state.answer.explanation}</p></article>}
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
  const [mentorState, setMentorState] = useState<MentorState>(
    createInitialMentorState,
  );
  const [mentorQuestion, setMentorQuestion] = useState('');
  const [teachingState, setTeachingState] = useState<TeachingState>(createInitialTeachingState);
  const [learningMemoryState, setLearningMemoryState] = useState<LearningMemoryState>(
    createInitialLearningMemoryState,
  );
  const [updatingLearningMemoryConcept, setUpdatingLearningMemoryConcept] = useState<string | null>(null);
  const mentorRequestToken = useRef(0);
  const mentorEventVersion = useRef(0);
  const teachingRequestToken = useRef(0);
  const teachingEventVersion = useRef(0);
  const learningMemoryRequestToken = useRef(0);
  const learningMemoryEventVersion = useRef(0);
  const learningMemoryUpdatePendingRef = useRef(false);

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
    let eventVersionBeforeInvoke: number | undefined;
    const connect = async () => {
      try {
        const nextRemoveListener = await subscribeToTeaching((next) => {
          teachingEventVersion.current += 1;
          if (mounted) {
            setTeachingState((current) => applyTeachingState(current, next));
          }
        });
        if (!mounted) { nextRemoveListener(); return; }
        removeListener = nextRemoveListener;
        const invokeEventVersion = teachingEventVersion.current;
        eventVersionBeforeInvoke = invokeEventVersion;
        const current = await getTeachingState();
        if (mounted) {
          setTeachingState((state) =>
            applyTeachingInitialInvokeResult(
              state,
              current,
              invokeEventVersion,
              teachingEventVersion.current,
            ),
          );
        }
      } catch (error) {
        if (
          mounted &&
          (eventVersionBeforeInvoke === undefined ||
            eventVersionBeforeInvoke === teachingEventVersion.current)
        ) {
          setTeachingState((current) => ({ ...current, status: 'error', error: errorMessage(error) }));
        }
      }
    };
    void connect();
    return () => { mounted = false; removeListener?.(); };
  }, []);

  useEffect(() => {
    let mounted = true;
    let removeListener: (() => void) | undefined;
    let eventVersionBeforeInvoke: number | undefined;

    const connect = async () => {
      try {
        const nextRemoveListener = await subscribeToMentor(
          (nextState) => {
            mentorEventVersion.current += 1;
            if (mounted) {
              setMentorState((current) => applyMentorState(current, nextState));
            }
          },
        );
        if (!mounted) {
          nextRemoveListener();
          return;
        }
        removeListener = nextRemoveListener;
        const invokeEventVersion = mentorEventVersion.current;
        eventVersionBeforeInvoke = invokeEventVersion;
        const currentState = await getMentorState();
        if (mounted) {
          setMentorState((current) =>
            applyMentorInitialInvokeResult(
              current,
              currentState,
              invokeEventVersion,
              mentorEventVersion.current,
            ),
          );
        }
      } catch (error) {
        if (
          mounted &&
          (eventVersionBeforeInvoke === undefined ||
            eventVersionBeforeInvoke === mentorEventVersion.current)
        ) {
          setMentorState((current) => createMentorError(current, errorMessage(error)));
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
    let eventVersionBeforeInvoke: number | undefined;

    const connect = async () => {
      try {
        const nextRemoveListener = await subscribeToLearningMemory((nextState) => {
          learningMemoryEventVersion.current += 1;
          if (mounted) {
            setLearningMemoryState((current) => applyLearningMemoryState(current, nextState));
          }
        });
        if (!mounted) {
          nextRemoveListener();
          return;
        }
        removeListener = nextRemoveListener;
        const invokeEventVersion = learningMemoryEventVersion.current;
        eventVersionBeforeInvoke = invokeEventVersion;
        const currentState = await getLearningMemoryState();
        if (mounted) {
          setLearningMemoryState((current) =>
            applyLearningMemoryInvokeResult(
              current,
              currentState,
              invokeEventVersion,
              learningMemoryEventVersion.current,
            ),
          );
        }
      } catch (error) {
        if (
          mounted &&
          (eventVersionBeforeInvoke === undefined ||
            eventVersionBeforeInvoke === learningMemoryEventVersion.current)
        ) {
          setLearningMemoryState((current) => createLearningMemoryError(current, errorMessage(error)));
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

  const completionGeneration = analysisState.analysis?.metadata.completionGeneration ?? null;
  const analysisKey = analysisState.analysis
    ? `${analysisState.analysis.metadata.projectPath}:${completionGeneration}`
    : null;
  const learningMemoryConcepts = analysisState.analysis
    ? uniqueConceptKeys(analysisState.analysis.record.programmingConcepts)
    : [];
  const learningMemoryConceptsKey = analysisState.analysis
    ? JSON.stringify(learningMemoryConcepts)
    : null;

  useEffect(() => {
    mentorRequestToken.current += 1;
    teachingRequestToken.current += 1;
    setMentorState(resetMentorState());
    setTeachingState(createInitialTeachingState());
    setMentorQuestion('');
    void resetMentor().catch(() => undefined);
    void resetTeaching().catch(() => undefined);
  }, [analysisKey]);

  useEffect(() => {
    const requestToken = ++learningMemoryRequestToken.current;
    const concepts = analysisKey === null || learningMemoryConceptsKey === null
      ? []
      : (JSON.parse(learningMemoryConceptsKey) as string[]);
    learningMemoryUpdatePendingRef.current = false;
    setUpdatingLearningMemoryConcept(null);
    setLearningMemoryState(
      createInitialLearningMemoryState(concepts, completionGeneration),
    );
    if (
      analysisKey === null
      || learningMemoryConceptsKey === null
      || completionGeneration === null
    ) return;

    const invokeEventVersion = learningMemoryEventVersion.current;
    void getRelevantLearningMemory(concepts, completionGeneration)
      .then((nextState) => {
        if (requestToken === learningMemoryRequestToken.current) {
          setLearningMemoryState((current) =>
            applyLearningMemoryInvokeResult(
              current,
              nextState,
              invokeEventVersion,
              learningMemoryEventVersion.current,
            ),
          );
        }
      })
      .catch((error: unknown) => {
        if (
          requestToken === learningMemoryRequestToken.current &&
          invokeEventVersion === learningMemoryEventVersion.current
        ) {
          setLearningMemoryState((current) => createLearningMemoryError(current, errorMessage(error)));
        }
      });
  }, [analysisKey, learningMemoryConceptsKey, completionGeneration]);

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
        mentorRequestToken.current += 1;
        teachingRequestToken.current += 1;
        setState(selected);
        setAnalysisState(createInitialAnalysisState());
        setMentorState(resetMentorState());
        setTeachingState(createInitialTeachingState());
        setMentorQuestion('');
      }
    } catch (error) {
      setState((current) => createWatcherError(current, errorMessage(error)));
    } finally {
      setIsSelecting(false);
    }
  };

  const clearProject = async () => {
    try {
      mentorRequestToken.current += 1;
      teachingRequestToken.current += 1;
      setState(await stopWatching());
      setAnalysisState(createInitialAnalysisState());
      setMentorState(resetMentorState());
      setTeachingState(createInitialTeachingState());
      setMentorQuestion('');
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

  const submitMentorQuestion = async () => {
    const requestToken = ++mentorRequestToken.current;
    try {
      const next = await askMentor(mentorQuestion, selectedFrozenPreview?.path ?? null);
      setMentorState((current) => applyMentorInvokeResult(current, next));
    } catch (error) {
      setMentorState((current) => {
        if (mentorRequestToken.current !== requestToken) {
          return current;
        }
        return createMentorError(current, errorMessage(error));
      });
    }
  };

  const cancelMentorQuestion = async () => {
    const requestToken = mentorRequestToken.current;
    try {
      const next = await cancelMentor();
      setMentorState((current) => applyMentorInvokeResult(current, next));
    } catch (error) {
      setMentorState((current) => {
        if (mentorRequestToken.current !== requestToken) {
          return current;
        }
        return createMentorError(current, errorMessage(error));
      });
    }
  };

  const explainChange = async (level: TeachingLevel) => {
    const requestToken = ++teachingRequestToken.current;
    setTeachingState({ status: 'loading', answer: null, error: null });
    try {
      const next = await teachChange(level, selectedFrozenPreview?.path ?? null);
      setTeachingState((current) => applyTeachingInvokeResult(current, next));
    } catch (error) {
      setTeachingState((current) => {
        if (teachingRequestToken.current !== requestToken) {
          return current;
        }
        return { ...current, status: 'error', error: errorMessage(error) };
      });
    }
  };

  const changeLearningMemoryStatus = async (concept: string, status: LearningStatus) => {
    if (completionGeneration === null) return;
    if (learningMemoryUpdatePendingRef.current) return;
    learningMemoryUpdatePendingRef.current = true;
    const requestToken = ++learningMemoryRequestToken.current;
    const invokeEventVersion = learningMemoryEventVersion.current;
    setUpdatingLearningMemoryConcept(concept);
    try {
      const nextState = await updateLearningMemoryStatus(
        concept,
        status,
        completionGeneration,
      );
      if (requestToken === learningMemoryRequestToken.current) {
        setLearningMemoryState((current) =>
          applyLearningMemoryInvokeResult(
            current,
            nextState,
            invokeEventVersion,
            learningMemoryEventVersion.current,
          ),
        );
      }
    } catch (error) {
      if (
        requestToken === learningMemoryRequestToken.current &&
        invokeEventVersion === learningMemoryEventVersion.current
      ) {
        setLearningMemoryState((current) => createLearningMemoryError(current, errorMessage(error)));
      }
    } finally {
      if (requestToken === learningMemoryRequestToken.current) {
        learningMemoryUpdatePendingRef.current = false;
        setUpdatingLearningMemoryConcept(null);
      }
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

        {analysisState.analysis && (
          <AskMentorPanel
            analysis={analysisState.analysis}
            state={mentorState}
            question={mentorQuestion}
            selectedFrozenPath={selectedFrozenPreview?.path ?? null}
            onQuestionChange={setMentorQuestion}
            onAsk={() => void submitMentorQuestion()}
            onCancel={() => void cancelMentorQuestion()}
          />
        )}

        {analysisState.analysis && (
          <TeachingPanel
            state={teachingState}
            memoryState={learningMemoryState}
            concepts={analysisState.analysis.record.programmingConcepts}
            updatingConcept={updatingLearningMemoryConcept}
            onTeach={(level) => void explainChange(level)}
            onStatusChange={(concept, status) => void changeLearningMemoryStatus(concept, status)}
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

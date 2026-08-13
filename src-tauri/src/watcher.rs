use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use notify::event::ModifyKind;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, State};

use crate::diff::{self, DiffState, FileSnapshot};

pub const WATCHER_CHANGE_EVENT: &str = "watcher-change";
pub const WATCHER_STATE_EVENT: &str = "watcher-state";
pub const DIFF_STATE_EVENT: &str = "diff-state";

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WatcherStatus {
    Idle,
    Watching,
    Error,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FileChangeStatus {
    Added,
    Modified,
    Deleted,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct FileChangeRecord {
    pub path: String,
    pub status: FileChangeStatus,
}

#[derive(Clone, Debug, Serialize)]
pub struct WatcherState {
    #[serde(rename = "projectPath")]
    pub project_path: Option<String>,
    pub status: WatcherStatus,
    pub records: Vec<FileChangeRecord>,
    pub diff: DiffState,
    pub error: Option<String>,
}

impl Default for WatcherState {
    fn default() -> Self {
        Self {
            project_path: None,
            status: WatcherStatus::Idle,
            records: Vec::new(),
            diff: DiffState::idle(),
            error: None,
        }
    }
}

struct Runtime {
    generation: u64,
    state: WatcherState,
    baseline: Option<FileSnapshot>,
    watcher: Option<WatcherHandle>,
}

pub struct AppState {
    runtime: Arc<Mutex<Runtime>>,
    publication: Arc<Mutex<()>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            runtime: Arc::new(Mutex::new(Runtime {
                generation: 0,
                state: WatcherState::default(),
                baseline: None,
                watcher: None,
            })),
            publication: Arc::new(Mutex::new(())),
        }
    }
}

struct WatcherHandle {
    stop: Sender<()>,
    join: Option<JoinHandle<()>>,
}

impl WatcherHandle {
    fn stop_and_join(mut self) {
        let _ = self.stop.send(());
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn lock_runtime(state: &AppState) -> MutexGuard<'_, Runtime> {
    state.runtime.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn claim_start_generation(
    runtime: &Arc<Mutex<Runtime>>,
    publication: &Arc<Mutex<()>>,
) -> (Option<WatcherHandle>, u64) {
    let _publication = publication
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut runtime = runtime.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    runtime.generation = runtime.generation.wrapping_add(1);
    runtime.baseline = None;
    (runtime.watcher.take(), runtime.generation)
}

/// Prepare and publish one event while holding the publication gate.
///
/// Generation claims use the same gate, so a newer command cannot invalidate
/// the operation after the final generation check but before `publish` runs.
/// The runtime mutex is released before calling `publish` to avoid holding
/// mutable application state across Tauri event delivery.
fn publish_if_current<T, Prepare, Publish>(
    runtime: &Arc<Mutex<Runtime>>,
    publication: &Arc<Mutex<()>>,
    generation: u64,
    prepare: Prepare,
    publish: Publish,
) -> bool
where
    Prepare: FnOnce(&mut Runtime) -> Option<T>,
    Publish: FnOnce(T),
{
    let _publication = publication
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let payload = {
        let Ok(mut runtime) = runtime.lock() else {
            return false;
        };
        if runtime.generation != generation {
            return false;
        }
        prepare(&mut runtime)
    };
    let Some(payload) = payload else {
        return false;
    };
    publish(payload);
    true
}

fn superseded_start_error() -> String {
    "Project watch start was superseded by a newer command".to_string()
}

fn finish_start_error(
    app: &AppHandle,
    runtime: &Arc<Mutex<Runtime>>,
    publication: &Arc<Mutex<()>>,
    generation: u64,
    error: String,
) -> Result<WatcherState, String> {
    let published = publish_if_current(
        runtime,
        publication,
        generation,
        |runtime| {
            runtime.state.status = WatcherStatus::Error;
            runtime.state.error = Some(error.clone());
            Some(runtime.state.clone())
        },
        |state| {
            let _ = app.emit(WATCHER_STATE_EVENT, state);
        },
    );
    if published {
        Err(error)
    } else {
        Err(superseded_start_error())
    }
}

fn install_start_state(
    runtime: &Arc<Mutex<Runtime>>,
    generation: u64,
    state: WatcherState,
) -> bool {
    let Ok(mut runtime) = runtime.lock() else {
        return false;
    };
    if runtime.generation != generation {
        return false;
    }
    runtime.state = state;
    true
}

fn install_watcher(
    runtime: &Arc<Mutex<Runtime>>,
    generation: u64,
    watcher: WatcherHandle,
) -> Result<WatcherState, WatcherHandle> {
    let Ok(mut runtime) = runtime.lock() else {
        return Err(watcher);
    };
    if runtime.generation != generation {
        return Err(watcher);
    }
    runtime.watcher = Some(watcher);
    Ok(runtime.state.clone())
}

/// Validate a folder selected by the user and return its canonical path.
pub fn validate_project_root(path: impl AsRef<Path>) -> Result<PathBuf, String> {
    let path = path.as_ref();
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("Unable to open project folder: {error}"))?;

    if !canonical.is_dir() {
        return Err("The selected project path is not a folder".to_string());
    }

    Ok(canonical)
}

fn is_metadata_component(component: &OsStr) -> bool {
    // Repository and Mentor metadata describe the workspace, not a user change.
    component == OsStr::new(".git") || component == OsStr::new(".codex")
}

fn is_metadata_path(path: &Path) -> bool {
    path.components().any(|component| match component {
        Component::Normal(name) => is_metadata_component(name),
        _ => false,
    })
}

fn normalized_absolute(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|directory| directory.join(path))
            .map_err(|error| format!("Unable to resolve changed path: {error}"))
    }
}

/// Convert an event path to a stable slash-separated path relative to the root.
pub fn normalize_relative_path(root: &Path, path: &Path) -> Result<String, String> {
    let root = root
        .canonicalize()
        .map_err(|error| format!("Unable to resolve project root: {error}"))?;
    let candidate = normalized_absolute(path)?;
    let relative = candidate
        .strip_prefix(&root)
        .map_err(|_| "Changed path is outside the selected project".to_string())?;

    if relative.as_os_str().is_empty() {
        return Err("The selected project root is not a file".to_string());
    }

    // Reject lexical traversal before filtering components so an outside path
    // cannot be relabeled as an in-root path after `..` is dropped.
    if relative.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::Prefix(_) | Component::RootDir
        )
    }) {
        return Err("Changed path is outside the selected project".to_string());
    }

    let normalized = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");

    if normalized.is_empty() {
        return Err("Changed path is not a file inside the selected project".to_string());
    }

    Ok(normalized)
}

fn collect_files(root: &Path, directory: &Path, snapshot: &mut BTreeSet<String>) -> Result<(), String> {
    if is_metadata_path(directory) {
        return Ok(());
    }

    let entries = fs::read_dir(directory)
        .map_err(|error| format!("Unable to read project folder: {error}"))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("Unable to inspect project file: {error}"))?;
        let path = entry.path();
        if is_metadata_path(&path) {
            continue;
        }

        let file_type = entry
            .file_type()
            .map_err(|error| format!("Unable to inspect project file: {error}"))?;
        if file_type.is_dir() {
            collect_files(root, &path, snapshot)?;
        } else if file_type.is_file() {
            if let Ok(relative) = normalize_relative_path(root, &path) {
                snapshot.insert(relative);
            }
        }
    }
    Ok(())
}

/// Capture file names only. Existing files are the baseline, not change records.
pub fn initial_snapshot(root: &Path) -> Result<BTreeSet<String>, String> {
    let root = validate_project_root(root)?;
    let mut snapshot = BTreeSet::new();
    collect_files(&root, &root, &mut snapshot)?;
    Ok(snapshot)
}

fn path_is_file(path: &Path) -> bool {
    fs::metadata(path).map(|metadata| metadata.is_file()).unwrap_or(false)
}

fn collect_existing_files(path: &Path, files: &mut BTreeSet<PathBuf>) {
    if is_metadata_path(path) {
        return;
    }

    if path_is_file(path) {
        files.insert(path.to_path_buf());
        return;
    }

    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        collect_existing_files(&entry.path(), files);
    }
}

fn baseline_paths_under(root: &Path, relative: &str, baseline: &BTreeSet<String>, paths: &mut BTreeSet<PathBuf>) {
    let prefix = format!("{relative}/");
    for item in baseline {
        if item == relative || item.starts_with(&prefix) {
            let mut path = root.to_path_buf();
            for segment in item.split('/') {
                path.push(segment);
            }
            paths.insert(path);
        }
    }
}

/// Classify the final on-disk state against the initial baseline.
pub fn classify_path(
    root: &Path,
    baseline: &BTreeSet<String>,
    path: &Path,
) -> Option<FileChangeRecord> {
    if is_metadata_path(path) {
        return None;
    }

    let relative = normalize_relative_path(root, path).ok()?;
    let exists = path_is_file(path);
    match (baseline.contains(&relative), exists) {
        (false, true) => Some(FileChangeRecord {
            path: relative,
            status: FileChangeStatus::Added,
        }),
        (true, true) => Some(FileChangeRecord {
            path: relative,
            status: FileChangeStatus::Modified,
        }),
        (true, false) => Some(FileChangeRecord {
            path: relative,
            status: FileChangeStatus::Deleted,
        }),
        (false, false) => None,
    }
}

fn relevant_event(event: &Event) -> bool {
    matches!(
        &event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
}

fn event_candidates(root: &Path, baseline: &BTreeSet<String>, event: &Event) -> BTreeSet<PathBuf> {
    if !relevant_event(event) {
        return BTreeSet::new();
    }

    let is_create = matches!(&event.kind, EventKind::Create(_));
    let is_remove = matches!(&event.kind, EventKind::Remove(_));
    let expands_directories = is_create
        || matches!(
            &event.kind,
            EventKind::Modify(ModifyKind::Name(_))
        );
    let mut candidates = BTreeSet::new();
    for event_path in &event.paths {
        if is_metadata_path(event_path) {
            continue;
        }

        if path_is_file(event_path) {
            candidates.insert(event_path.clone());
            continue;
        }

        if event_path.is_dir() {
            // Directory creates and rename destinations can contain many new
            // files. A directory metadata event must not turn every existing
            // child into a fake modification, so do not expand other events.
            if expands_directories {
                collect_existing_files(event_path, &mut candidates);
            }
            continue;
        }

        if is_remove || !event_path.exists() {
            if let Ok(relative) = normalize_relative_path(root, event_path) {
                // Keep the event path itself so a path that was added after the
                // baseline can be cleared when it later disappears.
                candidates.insert(event_path.clone());
                baseline_paths_under(root, &relative, baseline, &mut candidates);
            }
        }
    }

    candidates
}

fn affected_paths_for_event(
    root: &Path,
    baseline: &BTreeSet<String>,
    event: &Event,
) -> BTreeSet<String> {
    event_candidates(root, baseline, event)
        .into_iter()
        .filter_map(|path| normalize_relative_path(root, &path).ok())
        .collect()
}

fn changes_for_event(root: &Path, baseline: &BTreeSet<String>, event: &Event) -> Vec<FileChangeRecord> {
    coalesce_changes(
        event_candidates(root, baseline, event)
            .iter()
            .filter_map(|path| classify_path(root, baseline, path)),
    )
}

/// Coalesce duplicate notify bursts while retaining each path's final status.
pub fn coalesce_changes(changes: impl IntoIterator<Item = FileChangeRecord>) -> Vec<FileChangeRecord> {
    let mut by_path = BTreeMap::new();
    for change in changes {
        by_path.insert(change.path.clone(), change);
    }
    by_path.into_values().collect()
}

fn update_runtime(
    runtime: &Arc<Mutex<Runtime>>,
    generation: u64,
    change: &FileChangeRecord,
) -> bool {
    let Ok(mut runtime) = runtime.lock() else {
        return false;
    };
    if runtime.generation != generation {
        return false;
    }

    runtime.state.records.retain(|record| record.path != change.path);
    runtime.state.records.push(change.clone());
    runtime.state.records.sort_by(|left, right| left.path.cmp(&right.path));
    runtime.state.status = WatcherStatus::Watching;
    runtime.state.error = None;
    true
}

fn clear_runtime_path(
    runtime: &Arc<Mutex<Runtime>>,
    generation: u64,
    path: &str,
) -> Option<WatcherState> {
    let Ok(mut runtime) = runtime.lock() else {
        return None;
    };
    if runtime.generation != generation {
        return None;
    }

    let prefix = format!("{path}/");
    let previous_len = runtime.state.records.len();
    runtime
        .state
        .records
        .retain(|record| record.path != path && !record.path.starts_with(&prefix));
    if runtime.state.records.len() == previous_len {
        return None;
    }

    runtime.state.status = WatcherStatus::Watching;
    runtime.state.error = None;
    Some(runtime.state.clone())
}

fn set_runtime_error(runtime: &Arc<Mutex<Runtime>>, generation: u64, error: String) -> Option<WatcherState> {
    let Ok(mut runtime) = runtime.lock() else {
        return None;
    };
    if runtime.generation != generation {
        return None;
    }
    runtime.state.status = WatcherStatus::Error;
    runtime.state.error = Some(error);
    Some(runtime.state.clone())
}

fn start_worker(
    root: PathBuf,
    baseline: FileSnapshot,
    app: AppHandle,
    runtime: Arc<Mutex<Runtime>>,
    publication: Arc<Mutex<()>>,
    generation: u64,
) -> Result<WatcherHandle, String> {
    let (event_sender, event_receiver) = mpsc::channel::<notify::Result<Event>>();
    let (stop_sender, stop_receiver) = mpsc::channel::<()>();
    let callback_sender = event_sender.clone();
    let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |event| {
        let _ = callback_sender.send(event);
    })
    .map_err(|error| format!("Unable to start project watcher: {error}"))?;
    watcher
        .watch(&root, RecursiveMode::Recursive)
        .map_err(|error| format!("Unable to watch project folder: {error}"))?;

    let join = thread::spawn(move || {
        run_worker(
            root,
            baseline,
            app,
            runtime,
            publication,
            generation,
            event_receiver,
            stop_receiver,
            watcher,
        );
    });

    Ok(WatcherHandle {
        stop: stop_sender,
        join: Some(join),
    })
}

#[allow(clippy::too_many_arguments)]
fn run_worker(
    root: PathBuf,
    baseline: FileSnapshot,
    app: AppHandle,
    runtime: Arc<Mutex<Runtime>>,
    publication: Arc<Mutex<()>>,
    generation: u64,
    event_receiver: Receiver<notify::Result<Event>>,
    stop_receiver: Receiver<()>,
    _watcher: RecommendedWatcher,
) {
    let baseline_paths = baseline.files.keys().cloned().collect::<BTreeSet<_>>();
    loop {
        if stop_receiver.try_recv().is_ok() {
            break;
        }

        let first = match event_receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(event)) => event,
            Ok(Err(error)) => {
                if let Some(state) = set_runtime_error(&runtime, generation, error.to_string()) {
                    let _ = publish_if_current(
                        &runtime,
                        &publication,
                        generation,
                        |_| Some(state),
                        |state| {
                            let _ = app.emit(WATCHER_STATE_EVENT, state);
                        },
                    );
                }
                continue;
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };

        let mut events = vec![first];
        let deadline = Instant::now() + Duration::from_millis(45);
        while Instant::now() < deadline {
            match event_receiver.recv_timeout(Duration::from_millis(5)) {
                Ok(Ok(event)) => events.push(event),
                Ok(Err(_)) => {}
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        let mut affected_paths = BTreeSet::new();
        let changes = coalesce_changes(
            events
                .iter()
                .inspect(|event| {
                    affected_paths.extend(affected_paths_for_event(&root, &baseline_paths, event));
                })
                .flat_map(|event| changes_for_event(&root, &baseline_paths, event)),
        );

        let changed_paths = changes
            .iter()
            .map(|change| change.path.as_str())
            .collect::<BTreeSet<_>>();
        for path in affected_paths {
            if changed_paths.contains(path.as_str()) {
                continue;
            }
            if let Some(state) = clear_runtime_path(&runtime, generation, &path) {
                let _ = publish_if_current(
                    &runtime,
                    &publication,
                    generation,
                    |_| Some(state),
                    |state| {
                        let _ = app.emit(WATCHER_STATE_EVENT, state);
                    },
                );
            }
        }
        for change in changes {
            if update_runtime(&runtime, generation, &change) {
                let _ = publish_if_current(
                    &runtime,
                    &publication,
                    generation,
                    |_| Some(change),
                    |change| {
                        let _ = app.emit(WATCHER_CHANGE_EVENT, change);
                    },
                );
            }
        }

        if let Some(state) = refresh_runtime_diff(&runtime, generation, &root, &baseline) {
            let _ = publish_if_current(
                &runtime,
                &publication,
                generation,
                |_| Some(state),
                |state| {
                    let _ = app.emit(DIFF_STATE_EVENT, state.diff.clone());
                    let _ = app.emit(WATCHER_STATE_EVENT, state);
                },
            );
        }
    }
}

fn refresh_runtime_diff(
    runtime: &Arc<Mutex<Runtime>>,
    generation: u64,
    root: &Path,
    baseline: &FileSnapshot,
) -> Option<WatcherState> {
    let state = diff::state_for_baseline(root, baseline);
    let Ok(mut runtime) = runtime.lock() else {
        return None;
    };
    if runtime.generation != generation {
        return None;
    }
    let next_records = state
        .files
        .iter()
        .map(|file| FileChangeRecord {
            path: file.path.clone(),
            status: file.status,
        })
        .collect::<Vec<_>>();
    let next_error = state.error.clone();
    if runtime.state.diff == state
        && runtime.state.records == next_records
        && runtime.state.error == next_error
    {
        return None;
    }
    runtime.state.diff = state;
    runtime.state.records = next_records;
    runtime.state.error = next_error;
    Some(runtime.state.clone())
}

#[tauri::command]
pub fn get_watcher_state(state: State<'_, AppState>) -> WatcherState {
    lock_runtime(&state).state.clone()
}

#[tauri::command]
pub fn get_diff_state(state: State<'_, AppState>) -> DiffState {
    lock_runtime(&state).state.diff.clone()
}

#[tauri::command]
pub fn get_file_preview(
    state: State<'_, AppState>,
    path: String,
) -> Result<diff::FilePreview, String> {
    let (generation, project_path, baseline) = {
        let runtime = lock_runtime(&state);
        let project_path = runtime
            .state
            .project_path
            .clone()
            .ok_or_else(|| "No project is currently being watched".to_string())?;
        let baseline = runtime
            .baseline
            .clone()
            .ok_or_else(|| "The current watch snapshot is unavailable".to_string())?;
        (runtime.generation, project_path, baseline)
    };

    let preview = diff::file_preview(Path::new(&project_path), &baseline, &path)?;
    let runtime = lock_runtime(&state);
    if runtime.generation != generation || runtime.baseline.is_none() {
        return Err("The watch session changed while reading the selected file".to_string());
    }
    Ok(preview)
}

#[tauri::command]
pub fn start_watching(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<WatcherState, String> {
    // Claim the operation before validation or snapshot capture.  A stop or
    // newer start can therefore invalidate this command while that work is in
    // progress, before it can publish any selected project state.
    let (old_watcher, generation) = claim_start_generation(&state.runtime, &state.publication);
    if let Some(old_watcher) = old_watcher {
        old_watcher.stop_and_join();
    }

    let root = match validate_project_root(&path) {
        Ok(root) => root,
        Err(error) => {
            return finish_start_error(&app, &state.runtime, &state.publication, generation, error)
        }
    };
    let baseline = match diff::capture_snapshot(&root) {
        Ok(baseline) => baseline,
        Err(error) => {
            return finish_start_error(&app, &state.runtime, &state.publication, generation, error)
        }
    };

    let initial_state = WatcherState {
        project_path: Some(root.to_string_lossy().into_owned()),
        status: WatcherStatus::Watching,
        records: Vec::new(),
        diff: diff::state_for_baseline(&root, &baseline),
        error: None,
    };
    if !install_start_state(&state.runtime, generation, initial_state) {
        // Another command superseded this start while validation or snapshot
        // capture was in progress.  Leave the newer command's state and
        // handle intact.
        return Err(superseded_start_error());
    }
    {
        let Ok(mut runtime) = state.runtime.lock() else {
            return Err("Unable to store the watch-start snapshot".to_string());
        };
        if runtime.generation != generation {
            return Err(superseded_start_error());
        }
        runtime.baseline = Some(baseline.clone());
    }

    match start_worker(
        root.clone(),
        baseline,
        app.clone(),
        state.runtime.clone(),
        state.publication.clone(),
        generation,
    ) {
        Ok(watcher) => match install_watcher(&state.runtime, generation, watcher) {
            Ok(mut current) => {
                let published = publish_if_current(
                    &state.runtime,
                    &state.publication,
                    generation,
                    |runtime| {
                        // A same-generation worker may have advanced runtime.state
                        // after install_watcher captured `current`.  Read the state
                        // again under the publication gate so start-success cannot
                        // publish an older snapshot.
                        current = runtime.state.clone();
                        Some(current.clone())
                    },
                    |current| {
                        let _ = app.emit(WATCHER_STATE_EVENT, current.clone());
                        let _ = app.emit(DIFF_STATE_EVENT, current.diff.clone());
                    },
                );
                if published {
                    Ok(current)
                } else {
                    Err(superseded_start_error())
                }
            }
            Err(stale_watcher) => {
                // The worker was created after a newer command took over.
                // Stop it before returning, without touching newer state.
                stale_watcher.stop_and_join();
                Err(superseded_start_error())
            }
        },
        Err(error) => {
            if let Ok(mut runtime) = state.runtime.lock() {
                if runtime.generation == generation {
                    runtime.baseline = None;
                }
            }
            finish_start_error(&app, &state.runtime, &state.publication, generation, error)
        }
    }
}

#[tauri::command]
pub fn stop_watching(app: AppHandle, state: State<'_, AppState>) -> WatcherState {
    let (old_watcher, generation) = claim_start_generation(&state.runtime, &state.publication);
    if let Some(old_watcher) = old_watcher {
        old_watcher.stop_and_join();
    }

    let _ = publish_if_current(
        &state.runtime,
        &state.publication,
        generation,
        |runtime| {
            runtime.state = WatcherState::default();
            Some(runtime.state.clone())
        },
        |state| {
            let _ = app.emit(WATCHER_STATE_EVENT, state);
        },
    );

    let current = {
        let runtime = lock_runtime(&state);
        runtime.state.clone()
    };
    current
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempProject(PathBuf);

    impl TempProject {
        fn new() -> Self {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be available")
                .as_nanos();
            let path = std::env::temp_dir().join(format!("codex-mentor-test-{stamp}"));
            fs::create_dir_all(&path).expect("temp project should be created");
            Self(path)
        }
    }

    impl Drop for TempProject {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn normalizes_nested_paths_and_rejects_outside_paths() {
        let project = TempProject::new();
        let nested = project.0.join("src");
        fs::create_dir_all(&nested).unwrap();
        let file = nested.join("main.ts");
        fs::write(&file, "export {};").unwrap();
        assert_eq!(normalize_relative_path(&project.0, &file).unwrap(), "src/main.ts");
        assert!(normalize_relative_path(&project.0, &std::env::temp_dir().join("outside.ts")).is_err());
        let lexical_outside = project.0.join("..").join("outside.ts");
        assert!(normalize_relative_path(&project.0, &lexical_outside).is_err());
    }

    #[test]
    fn initial_snapshot_excludes_git_and_codex_metadata() {
        let project = TempProject::new();
        fs::write(project.0.join("README.md"), "hello").unwrap();
        fs::create_dir_all(project.0.join(".git/objects")).unwrap();
        fs::write(project.0.join(".git/objects/index"), "metadata").unwrap();
        fs::create_dir_all(project.0.join(".codex")).unwrap();
        fs::write(project.0.join(".codex/session.json"), "metadata").unwrap();
        let snapshot = initial_snapshot(&project.0).unwrap();
        assert_eq!(snapshot.into_iter().collect::<Vec<_>>(), vec!["README.md"]);
    }

    #[test]
    fn classifies_added_modified_deleted_without_emitting_baseline_files() {
        let project = TempProject::new();
        let existing = project.0.join("existing.ts");
        let added = project.0.join("new.ts");
        fs::write(&existing, "one").unwrap();
        let baseline = initial_snapshot(&project.0).unwrap();
        assert!(classify_path(&project.0, &baseline, &existing).unwrap().status == FileChangeStatus::Modified);
        fs::write(&added, "new").unwrap();
        assert_eq!(classify_path(&project.0, &baseline, &added).unwrap().status, FileChangeStatus::Added);
        fs::remove_file(&existing).unwrap();
        assert_eq!(classify_path(&project.0, &baseline, &existing).unwrap().status, FileChangeStatus::Deleted);
    }

    #[test]
    fn detects_nested_adds_and_rapid_saves_as_one_final_change() {
        let project = TempProject::new();
        let existing = project.0.join("src/existing.ts");
        fs::create_dir_all(existing.parent().unwrap()).unwrap();
        fs::write(&existing, "one").unwrap();
        let baseline = initial_snapshot(&project.0).unwrap();

        let nested = project.0.join("src/nested/new.ts");
        fs::create_dir_all(nested.parent().unwrap()).unwrap();
        fs::write(&nested, "new").unwrap();
        assert_eq!(classify_path(&project.0, &baseline, &nested).unwrap().status, FileChangeStatus::Added);

        fs::write(&existing, "two").unwrap();
        fs::write(&existing, "three").unwrap();
        let rapid = coalesce_changes([
            classify_path(&project.0, &baseline, &existing).unwrap(),
            classify_path(&project.0, &baseline, &existing).unwrap(),
        ]);
        assert_eq!(rapid, vec![FileChangeRecord {
            path: "src/existing.ts".into(),
            status: FileChangeStatus::Modified,
        }]);
    }

    #[test]
    fn detects_files_in_renamed_directory_destination() {
        use notify::event::RenameMode;

        let project = TempProject::new();
        let source = project.0.join("before");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("nested.ts"), "one").unwrap();
        let baseline = initial_snapshot(&project.0).unwrap();

        let destination = project.0.join("after");
        fs::rename(&source, &destination).unwrap();
        let event = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
            .add_path(source)
            .add_path(destination);

        assert_eq!(
            changes_for_event(&project.0, &baseline, &event),
            vec![
                FileChangeRecord {
                    path: "after/nested.ts".into(),
                    status: FileChangeStatus::Added,
                },
                FileChangeRecord {
                    path: "before/nested.ts".into(),
                    status: FileChangeStatus::Deleted,
                },
            ]
        );
    }

    #[test]
    fn metadata_events_are_ignored() {
        let project = TempProject::new();
        fs::create_dir_all(project.0.join(".git")).unwrap();
        let metadata_file = project.0.join(".git/index");
        fs::write(&metadata_file, "metadata").unwrap();
        let baseline = initial_snapshot(&project.0).unwrap();
        assert!(classify_path(&project.0, &baseline, &metadata_file).is_none());
    }

    #[test]
    fn coalesces_duplicate_notify_events() {
        let changes = coalesce_changes([
            FileChangeRecord { path: "a.ts".into(), status: FileChangeStatus::Added },
            FileChangeRecord { path: "a.ts".into(), status: FileChangeStatus::Modified },
            FileChangeRecord { path: "b.ts".into(), status: FileChangeStatus::Deleted },
        ]);
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].path, "a.ts");
        assert_eq!(changes[0].status, FileChangeStatus::Modified);
    }

    #[test]
    fn invalid_root_is_reported() {
        let missing = std::env::temp_dir().join("codex-mentor-no-such-project");
        assert!(validate_project_root(missing).is_err());
    }

    #[test]
    fn start_claims_generation_before_slow_snapshot_and_rejects_stale_install() {
        let runtime = Arc::new(Mutex::new(Runtime {
            generation: 0,
            state: WatcherState::default(),
            baseline: None,
            watcher: None,
        }));
        let publication = Arc::new(Mutex::new(()));
        let (capture_started_sender, capture_started_receiver) = mpsc::sync_channel(0);
        let (release_capture_sender, release_capture_receiver) = mpsc::sync_channel(0);
        let operation_runtime = runtime.clone();
        let operation_publication = publication.clone();

        let operation = std::thread::spawn(move || {
            let (old_watcher, generation) =
                claim_start_generation(&operation_runtime, &operation_publication);
            assert!(old_watcher.is_none());
            capture_started_sender.send(generation).unwrap();
            release_capture_receiver.recv().unwrap();

            install_start_state(
                &operation_runtime,
                generation,
                WatcherState {
                    project_path: Some("stale-project".into()),
                    status: WatcherStatus::Watching,
                    records: Vec::new(),
                    diff: DiffState::idle(),
                    error: None,
                },
            )
        });

        let first_generation = capture_started_receiver.recv().unwrap();
        let (old_watcher, second_generation) = claim_start_generation(&runtime, &publication);
        assert!(old_watcher.is_none());
        assert_eq!(second_generation, first_generation.wrapping_add(1));
        release_capture_sender.send(()).unwrap();

        assert!(!operation.join().unwrap());
        let runtime = runtime.lock().unwrap();
        let state = &runtime.state;
        assert_eq!(runtime.generation, second_generation);
        assert!(state.project_path.is_none());
        assert_eq!(state.status, WatcherStatus::Idle);
        assert!(state.records.is_empty());
    }

    #[test]
    fn stale_start_success_event_is_suppressed_after_newer_claim() {
        let runtime = Arc::new(Mutex::new(Runtime {
            generation: 0,
            state: WatcherState::default(),
            baseline: None,
            watcher: None,
        }));
        let publication = Arc::new(Mutex::new(()));
        let (_, stale_generation) = claim_start_generation(&runtime, &publication);
        let (_, current_generation) = claim_start_generation(&runtime, &publication);
        let mut events = Vec::new();

        let published = publish_if_current(
            &runtime,
            &publication,
            stale_generation,
            |_| Some("start-success"),
            |event| events.push(event),
        );

        assert!(!published);
        assert!(events.is_empty());
        assert_eq!(runtime.lock().unwrap().generation, current_generation);
    }

    #[test]
    fn a_new_generation_invalidates_the_previous_preview_baseline() {
        let runtime = Arc::new(Mutex::new(Runtime {
            generation: 3,
            state: WatcherState::default(),
            baseline: Some(FileSnapshot::default()),
            watcher: None,
        }));
        let publication = Arc::new(Mutex::new(()));

        let (_, generation) = claim_start_generation(&runtime, &publication);

        let runtime = runtime.lock().unwrap();
        assert_eq!(runtime.generation, generation);
        assert!(runtime.baseline.is_none());
    }

    #[test]
    fn start_success_publication_uses_latest_same_generation_worker_state() {
        let project = TempProject::new();
        let file = project.0.join("main.ts");
        fs::write(&file, "one\n").unwrap();
        let baseline = diff::capture_snapshot(&project.0).unwrap();
        let runtime = Arc::new(Mutex::new(Runtime {
            generation: 1,
            state: WatcherState {
                project_path: Some(project.0.to_string_lossy().into_owned()),
                status: WatcherStatus::Watching,
                records: Vec::new(),
                diff: diff::state_for_baseline(&project.0, &baseline),
                error: None,
            },
            baseline: None,
            watcher: None,
        }));
        let publication = Arc::new(Mutex::new(()));
        let (stop, _receiver) = mpsc::channel();
        let captured = install_watcher(&runtime, 1, WatcherHandle { stop, join: None }).unwrap();

        // Model a same-generation worker update between installation and the
        // start-success publication.
        fs::write(&file, "two\n").unwrap();
        assert!(refresh_runtime_diff(&runtime, 1, &project.0, &baseline).is_some());
        let latest = runtime.lock().unwrap().state.clone();

        let mut current = captured;
        let mut emitted = None;
        let published = publish_if_current(
            &runtime,
            &publication,
            1,
            |runtime| {
                current = runtime.state.clone();
                Some(current.clone())
            },
            |state| emitted = Some(state),
        );

        assert!(published);
        let emitted = emitted.expect("start-success state should be emitted");
        assert_eq!(current.records, latest.records);
        assert_eq!(current.diff, latest.diff);
        assert_eq!(emitted.records, latest.records);
        assert_eq!(emitted.diff, latest.diff);
    }

    #[test]
    fn stale_start_error_event_is_suppressed_after_newer_claim() {
        let runtime = Arc::new(Mutex::new(Runtime {
            generation: 0,
            state: WatcherState::default(),
            baseline: None,
            watcher: None,
        }));
        let publication = Arc::new(Mutex::new(()));
        let (_, stale_generation) = claim_start_generation(&runtime, &publication);
        let (_, current_generation) = claim_start_generation(&runtime, &publication);
        let mut events = Vec::new();

        let published = publish_if_current(
            &runtime,
            &publication,
            stale_generation,
            |runtime| {
                runtime.state.status = WatcherStatus::Error;
                Some("start-error")
            },
            |event| events.push(event),
        );

        assert!(!published);
        assert!(events.is_empty());
        assert_eq!(runtime.lock().unwrap().generation, current_generation);
        assert_eq!(runtime.lock().unwrap().state.status, WatcherStatus::Idle);
    }

    #[test]
    fn stale_stop_event_is_suppressed_after_newer_claim() {
        let runtime = Arc::new(Mutex::new(Runtime {
            generation: 0,
            state: WatcherState {
                project_path: Some("current".into()),
                status: WatcherStatus::Watching,
                records: Vec::new(),
                diff: DiffState::idle(),
                error: None,
            },
            baseline: None,
            watcher: None,
        }));
        let publication = Arc::new(Mutex::new(()));
        let (_, stale_generation) = claim_start_generation(&runtime, &publication);
        let (_, current_generation) = claim_start_generation(&runtime, &publication);
        let mut events = Vec::new();

        let published = publish_if_current(
            &runtime,
            &publication,
            stale_generation,
            |runtime| {
                runtime.state = WatcherState::default();
                Some("stop")
            },
            |event| events.push(event),
        );

        assert!(!published);
        assert!(events.is_empty());
        assert_eq!(runtime.lock().unwrap().generation, current_generation);
        assert_eq!(runtime.lock().unwrap().state.project_path.as_deref(), Some("current"));
    }

    #[test]
    fn stale_generation_cannot_update_a_switched_project() {
        let runtime = Arc::new(Mutex::new(Runtime {
            generation: 2,
            state: WatcherState {
                project_path: Some("new".into()),
                status: WatcherStatus::Watching,
                records: Vec::new(),
                diff: DiffState::idle(),
                error: None,
            },
            baseline: None,
            watcher: None,
        }));
        let stale = FileChangeRecord {
            path: "old.ts".into(),
            status: FileChangeStatus::Modified,
        };
        assert!(!update_runtime(&runtime, 1, &stale));
        assert!(runtime.lock().unwrap().state.records.is_empty());
    }

    #[test]
    fn stale_generation_cannot_publish_a_diff_for_a_switched_project() {
        let project = TempProject::new();
        let file = project.0.join("main.ts");
        fs::write(&file, "one\n").unwrap();
        let baseline = diff::capture_snapshot(&project.0).unwrap();
        fs::write(&file, "two\n").unwrap();
        let runtime = Arc::new(Mutex::new(Runtime {
            generation: 2,
            state: WatcherState {
                project_path: Some("new".into()),
                status: WatcherStatus::Watching,
                records: Vec::new(),
                diff: DiffState::idle(),
                error: None,
            },
            baseline: None,
            watcher: None,
        }));
        assert!(refresh_runtime_diff(&runtime, 1, &project.0, &baseline).is_none());
        assert_eq!(runtime.lock().unwrap().state.diff, DiffState::idle());
    }

    #[test]
    fn stale_generation_cannot_install_start_state_or_watcher() {
        let runtime = Arc::new(Mutex::new(Runtime {
            generation: 2,
            state: WatcherState {
                project_path: Some("new".into()),
                status: WatcherStatus::Watching,
                records: Vec::new(),
                diff: DiffState::idle(),
                error: None,
            },
            baseline: None,
            watcher: None,
        }));
        let stale_state = WatcherState {
            project_path: Some("old".into()),
            status: WatcherStatus::Watching,
            records: Vec::new(),
            diff: DiffState::idle(),
            error: None,
        };
        assert!(!install_start_state(&runtime, 1, stale_state));
        assert_eq!(runtime.lock().unwrap().state.project_path.as_deref(), Some("new"));

        let (stop, _receiver) = mpsc::channel();
        let stale_watcher = WatcherHandle {
            stop,
            join: None,
        };
        assert!(install_watcher(&runtime, 1, stale_watcher).is_err());
        assert!(runtime.lock().unwrap().watcher.is_none());
    }

    #[test]
    fn diff_refresh_clears_a_spurious_record_when_content_matches_baseline() {
        let project = TempProject::new();
        let file = project.0.join("main.ts");
        fs::write(&file, "one\n").unwrap();
        let baseline = diff::capture_snapshot(&project.0).unwrap();
        let runtime = Arc::new(Mutex::new(Runtime {
            generation: 1,
            state: WatcherState {
                project_path: Some(project.0.to_string_lossy().into_owned()),
                status: WatcherStatus::Watching,
                records: vec![FileChangeRecord {
                    path: "main.ts".into(),
                    status: FileChangeStatus::Modified,
                }],
                diff: diff::state_for_baseline(&project.0, &baseline),
                error: None,
            },
            baseline: None,
            watcher: None,
        }));
        assert!(refresh_runtime_diff(&runtime, 1, &project.0, &baseline).is_some());
        assert!(runtime.lock().unwrap().state.records.is_empty());
    }

    #[test]
    fn clears_added_record_when_a_later_delete_returns_path_to_baseline() {
        use notify::event::{CreateKind, RemoveKind};

        let project = TempProject::new();
        let baseline = initial_snapshot(&project.0).unwrap();
        let new_file = project.0.join("new.ts");
        let runtime = Arc::new(Mutex::new(Runtime {
            generation: 1,
            state: WatcherState {
                project_path: Some(project.0.to_string_lossy().into_owned()),
                status: WatcherStatus::Watching,
                records: Vec::new(),
                diff: DiffState::idle(),
                error: None,
            },
            baseline: None,
            watcher: None,
        }));

        fs::write(&new_file, "new").unwrap();
        let create_event = Event::new(EventKind::Create(CreateKind::File)).add_path(new_file.clone());
        let added = changes_for_event(&project.0, &baseline, &create_event);
        assert_eq!(added, vec![FileChangeRecord {
            path: "new.ts".into(),
            status: FileChangeStatus::Added,
        }]);
        for change in &added {
            assert!(update_runtime(&runtime, 1, change));
        }

        fs::remove_file(&new_file).unwrap();
        let remove_event = Event::new(EventKind::Remove(RemoveKind::File)).add_path(new_file);
        assert!(changes_for_event(&project.0, &baseline, &remove_event).is_empty());
        let affected = affected_paths_for_event(&project.0, &baseline, &remove_event);
        assert_eq!(affected.into_iter().collect::<Vec<_>>(), vec!["new.ts"]);
        assert!(clear_runtime_path(&runtime, 1, "new.ts").is_some());
        assert!(runtime.lock().unwrap().state.records.is_empty());
    }
}

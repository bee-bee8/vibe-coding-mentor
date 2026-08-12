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

pub const WATCHER_CHANGE_EVENT: &str = "watcher-change";
pub const WATCHER_STATE_EVENT: &str = "watcher-state";

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WatcherStatus {
    Idle,
    Watching,
    Error,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
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
    pub error: Option<String>,
}

impl Default for WatcherState {
    fn default() -> Self {
        Self {
            project_path: None,
            status: WatcherStatus::Idle,
            records: Vec::new(),
            error: None,
        }
    }
}

struct Runtime {
    generation: u64,
    state: WatcherState,
    watcher: Option<WatcherHandle>,
}

pub struct AppState {
    runtime: Arc<Mutex<Runtime>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            runtime: Arc::new(Mutex::new(Runtime {
                generation: 0,
                state: WatcherState::default(),
                watcher: None,
            })),
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
    baseline: BTreeSet<String>,
    app: AppHandle,
    runtime: Arc<Mutex<Runtime>>,
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
    baseline: BTreeSet<String>,
    app: AppHandle,
    runtime: Arc<Mutex<Runtime>>,
    generation: u64,
    event_receiver: Receiver<notify::Result<Event>>,
    stop_receiver: Receiver<()>,
    _watcher: RecommendedWatcher,
) {
    loop {
        if stop_receiver.try_recv().is_ok() {
            break;
        }

        let first = match event_receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(event)) => event,
            Ok(Err(error)) => {
                if let Some(state) = set_runtime_error(&runtime, generation, error.to_string()) {
                    let _ = app.emit(WATCHER_STATE_EVENT, state);
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
                    affected_paths.extend(affected_paths_for_event(&root, &baseline, event));
                })
                .flat_map(|event| changes_for_event(&root, &baseline, event)),
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
                let _ = app.emit(WATCHER_STATE_EVENT, state);
            }
        }
        for change in changes {
            if update_runtime(&runtime, generation, &change) {
                let _ = app.emit(WATCHER_CHANGE_EVENT, change);
            }
        }
    }
}

#[tauri::command]
pub fn get_watcher_state(state: State<'_, AppState>) -> WatcherState {
    lock_runtime(&state).state.clone()
}

#[tauri::command]
pub fn start_watching(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<WatcherState, String> {
    let root = validate_project_root(&path)?;
    let baseline = initial_snapshot(&root)?;

    let (old_watcher, generation) = {
        let mut runtime = lock_runtime(&state);
        runtime.generation = runtime.generation.wrapping_add(1);
        (runtime.watcher.take(), runtime.generation)
    };
    if let Some(old_watcher) = old_watcher {
        old_watcher.stop_and_join();
    }

    {
        let mut runtime = lock_runtime(&state);
        runtime.state = WatcherState {
            project_path: Some(root.to_string_lossy().into_owned()),
            status: WatcherStatus::Watching,
            records: Vec::new(),
            error: None,
        };
    }

    match start_worker(root.clone(), baseline, app.clone(), state.runtime.clone(), generation) {
        Ok(watcher) => {
            let current = {
                let mut runtime = lock_runtime(&state);
                runtime.watcher = Some(watcher);
                runtime.state.clone()
            };
            let _ = app.emit(WATCHER_STATE_EVENT, current.clone());
            Ok(current)
        }
        Err(error) => {
            let current = {
                let mut runtime = lock_runtime(&state);
                runtime.state.status = WatcherStatus::Error;
                runtime.state.error = Some(error.clone());
                runtime.state.clone()
            };
            let _ = app.emit(WATCHER_STATE_EVENT, current);
            Err(error)
        }
    }
}

#[tauri::command]
pub fn stop_watching(app: AppHandle, state: State<'_, AppState>) -> WatcherState {
    let (old_watcher, generation) = {
        let mut runtime = lock_runtime(&state);
        runtime.generation = runtime.generation.wrapping_add(1);
        (runtime.watcher.take(), runtime.generation)
    };
    if let Some(old_watcher) = old_watcher {
        old_watcher.stop_and_join();
    }

    let current = {
        let mut runtime = lock_runtime(&state);
        if runtime.generation == generation {
            runtime.state = WatcherState::default();
        }
        runtime.state.clone()
    };
    let _ = app.emit(WATCHER_STATE_EVENT, current.clone());
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
    fn stale_generation_cannot_update_a_switched_project() {
        let runtime = Arc::new(Mutex::new(Runtime {
            generation: 2,
            state: WatcherState {
                project_path: Some("new".into()),
                status: WatcherStatus::Watching,
                records: Vec::new(),
                error: None,
            },
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
                error: None,
            },
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

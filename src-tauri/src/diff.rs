use crate::watcher::FileChangeStatus;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

/// A file entry captured at a point in time.
///
/// `Unavailable` is deliberately different from a missing map entry.  A map
/// entry means that the file existed, but Mentor could not read its contents;
/// this lets line counts stay unknown instead of being reported as zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnapshotEntry {
    Content(Vec<u8>),
    Unavailable,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileSnapshot {
    pub files: BTreeMap<String, SnapshotEntry>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DiffSource {
    None,
    Git,
    Snapshot,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ContentStatus {
    Text,
    Binary,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileDiffRecord {
    pub path: String,
    pub status: FileChangeStatus,
    pub lines_added: Option<u64>,
    pub lines_deleted: Option<u64>,
    pub content_status: ContentStatus,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiffState {
    pub project_path: Option<String>,
    pub source: DiffSource,
    pub fallback: bool,
    pub files: Vec<FileDiffRecord>,
    pub total_lines_added: Option<u64>,
    pub total_lines_deleted: Option<u64>,
    pub unknown_line_count_files: usize,
    pub error: Option<String>,
}

impl DiffState {
    pub fn idle() -> Self {
        Self {
            project_path: None,
            source: DiffSource::None,
            fallback: false,
            files: Vec::new(),
            total_lines_added: Some(0),
            total_lines_deleted: Some(0),
            unknown_line_count_files: 0,
            error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitError {
    NotRepository,
    CommandUnavailable(String),
    CommandFailed(String),
    InvalidOutput(String),
}

impl fmt::Display for GitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRepository => write!(formatter, "The selected folder is not a Git repository"),
            Self::CommandUnavailable(error) => write!(formatter, "Git is unavailable: {error}"),
            Self::CommandFailed(error) => write!(formatter, "Git could not read the current changes: {error}"),
            Self::InvalidOutput(error) => write!(formatter, "Git returned unreadable change data: {error}"),
        }
    }
}

fn metadata_component(component: &Component<'_>) -> bool {
    matches!(component, Component::Normal(name) if name == std::ffi::OsStr::new(".git") || name == std::ffi::OsStr::new(".codex"))
}

fn is_metadata_path(path: &Path) -> bool {
    path.components().any(|component| metadata_component(&component))
}

fn canonical_root(root: &Path) -> Result<PathBuf, String> {
    let root = root
        .canonicalize()
        .map_err(|error| format!("Unable to open project folder: {error}"))?;
    if !root.is_dir() {
        return Err("The selected project path is not a folder".to_string());
    }
    Ok(root)
}

fn relative_path(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    if relative.as_os_str().is_empty() || is_metadata_path(relative) {
        return None;
    }
    let result = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    (!result.is_empty()).then_some(result)
}

fn collect_snapshot(root: &Path, directory: &Path, snapshot: &mut FileSnapshot) -> Result<(), String> {
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
            collect_snapshot(root, &path, snapshot)?;
        } else if file_type.is_file() {
            let Some(relative) = relative_path(root, &path) else {
                continue;
            };
            let entry = match fs::read(&path) {
                Ok(content) => SnapshotEntry::Content(content),
                Err(_) => SnapshotEntry::Unavailable,
            };
            snapshot.files.insert(relative, entry);
        }
    }
    Ok(())
}

/// Capture file contents under `root`, excluding Mentor and Git metadata.
pub fn capture_snapshot(root: &Path) -> Result<FileSnapshot, String> {
    let root = canonical_root(root)?;
    let mut snapshot = FileSnapshot::default();
    collect_snapshot(&root, &root, &mut snapshot)?;
    Ok(snapshot)
}

fn text_lines(content: &[u8]) -> Result<Vec<String>, ContentStatus> {
    let text = std::str::from_utf8(content).map_err(|_| ContentStatus::Binary)?;
    if content.contains(&0) {
        return Err(ContentStatus::Binary);
    }
    if text.is_empty() {
        return Ok(Vec::new());
    }
    Ok(text
        .split_inclusive('\n')
        .map(|line| line.strip_suffix('\n').unwrap_or(line).strip_suffix('\r').unwrap_or_else(|| line.strip_suffix('\n').unwrap_or(line)).to_string())
        .collect())
}

fn line_count(content: &[u8]) -> Result<u64, ContentStatus> {
    Ok(text_lines(content)?.len() as u64)
}

/// Count line additions and deletions using a deterministic LCS comparison.
/// CRLF and LF are treated as the same line ending, and a missing final
/// newline does not create a phantom line.
pub fn count_line_changes(before: &[u8], after: &[u8]) -> Result<(u64, u64), ContentStatus> {
    let before = text_lines(before)?;
    let after = text_lines(after)?;
    let mut previous = vec![0u64; after.len() + 1];
    for before_line in &before {
        let mut current = vec![0u64; after.len() + 1];
        for (after_index, after_line) in after.iter().enumerate() {
            current[after_index + 1] = if before_line == after_line {
                previous[after_index] + 1
            } else {
                current[after_index].max(previous[after_index + 1])
            };
        }
        previous = current;
    }
    let common = previous[after.len()];
    Ok((after.len() as u64 - common, before.len() as u64 - common))
}

fn content_status(entry: Option<&SnapshotEntry>) -> ContentStatus {
    match entry {
        Some(SnapshotEntry::Unavailable) => ContentStatus::Unavailable,
        Some(SnapshotEntry::Content(content)) => match text_lines(content) {
            Ok(_) => ContentStatus::Text,
            Err(status) => status,
        },
        None => ContentStatus::Text,
    }
}

fn file_diff(
    path: String,
    status: FileChangeStatus,
    before: Option<&SnapshotEntry>,
    after: Option<&SnapshotEntry>,
) -> FileDiffRecord {
    let status_before = content_status(before);
    let status_after = content_status(after);
    let content_status = if status_before == ContentStatus::Unavailable || status_after == ContentStatus::Unavailable {
        ContentStatus::Unavailable
    } else if status_before == ContentStatus::Binary || status_after == ContentStatus::Binary {
        ContentStatus::Binary
    } else {
        ContentStatus::Text
    };

    let (mut lines_added, mut lines_deleted) = match (status, before, after) {
        (FileChangeStatus::Added, _, Some(SnapshotEntry::Content(after))) => {
            (line_count(after).ok(), Some(0))
        }
        (FileChangeStatus::Deleted, Some(SnapshotEntry::Content(before)), _) => {
            (Some(0), line_count(before).ok())
        }
        (FileChangeStatus::Modified, Some(SnapshotEntry::Content(before)), Some(SnapshotEntry::Content(after))) => {
            match count_line_changes(before, after) {
                Ok((added, deleted)) => (Some(added), Some(deleted)),
                Err(_) => (None, None),
            }
        }
        _ => (None, None),
    };
    if content_status != ContentStatus::Text {
        lines_added = None;
        lines_deleted = None;
    }

    FileDiffRecord {
        path,
        status,
        lines_added,
        lines_deleted,
        content_status,
    }
}

fn finalize_state(
    project_path: Option<String>,
    source: DiffSource,
    fallback: bool,
    mut files: Vec<FileDiffRecord>,
    error: Option<String>,
) -> DiffState {
    files.sort_by(|left, right| left.path.cmp(&right.path));
    let unknown_line_count_files = files
        .iter()
        .filter(|file| file.lines_added.is_none() || file.lines_deleted.is_none())
        .count();
    let total_lines_added = files
        .iter()
        .map(|file| file.lines_added)
        .try_fold(0u64, |total, value| value.map(|value| total + value));
    let total_lines_deleted = files
        .iter()
        .map(|file| file.lines_deleted)
        .try_fold(0u64, |total, value| value.map(|value| total + value));
    DiffState {
        project_path,
        source,
        fallback,
        files,
        total_lines_added,
        total_lines_deleted,
        unknown_line_count_files,
        error,
    }
}

/// Compare two snapshots.  A path is only reported when its final content
/// differs from the selected project's watch-start content.
pub fn diff_snapshots(
    project_path: Option<String>,
    before: &FileSnapshot,
    after: &FileSnapshot,
) -> DiffState {
    let paths = before
        .files
        .keys()
        .chain(after.files.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let files = paths
        .into_iter()
        .filter_map(|path| {
            let before_entry = before.files.get(&path);
            let after_entry = after.files.get(&path);
            let status = match (before_entry, after_entry) {
                (None, Some(_)) => FileChangeStatus::Added,
                (Some(_), None) => FileChangeStatus::Deleted,
                (Some(SnapshotEntry::Content(before)), Some(SnapshotEntry::Content(after))) if before == after => return None,
                (Some(SnapshotEntry::Unavailable), Some(SnapshotEntry::Unavailable)) => return None,
                (Some(_), Some(_)) => FileChangeStatus::Modified,
                (None, None) => return None,
            };
            Some(file_diff(path, status, before_entry, after_entry))
        })
        .collect();
    finalize_state(project_path, DiffSource::Snapshot, true, files, None)
}

fn command_output(root: &Path, arguments: &[&str]) -> Result<Vec<u8>, GitError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("-c")
        .arg("color.ui=false")
        .args(arguments)
        .output()
        .map_err(|error| GitError::CommandUnavailable(error.to_string()))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(GitError::CommandFailed(if message.is_empty() {
            format!("git exited with {}", output.status)
        } else {
            message
        }));
    }
    Ok(output.stdout)
}

fn parse_status_paths(output: &[u8]) -> Result<Vec<(String, Option<String>, [u8; 2])>, GitError> {
    let mut records = Vec::new();
    let mut segments = output.split(|byte| *byte == 0).filter(|segment| !segment.is_empty());
    while let Some(segment) = segments.next() {
        if segment.len() < 4 {
            return Err(GitError::InvalidOutput("status record is too short".to_string()));
        }
        let code = [segment[0], segment[1]];
        let path = String::from_utf8_lossy(&segment[3..]).into_owned();
        let rename = code[0] == b'R' || code[0] == b'C';
        let (old_path, destination) = if rename {
            // Porcelain `-z` puts the destination in the first record and
            // the original path in the following NUL-delimited record.
            let old_path = segments
                .next()
                .ok_or_else(|| GitError::InvalidOutput("rename record has no original path".to_string()))?;
            (
                String::from_utf8_lossy(old_path).into_owned(),
                Some(path),
            )
        } else {
            (path, None)
        };
        records.push((old_path, destination, code));
    }
    Ok(records)
}

fn normalize_git_path(repo_root: &Path, selected_root: &Path, path: &str) -> Option<String> {
    let candidate = repo_root.join(path);
    let canonical_candidate = if candidate.exists() {
        candidate.canonicalize().ok()?
    } else {
        candidate
    };
    let relative = canonical_candidate.strip_prefix(selected_root).ok()?;
    // A missing Git path cannot be canonicalized.  Reject lexical traversal
    // before filtering components so `../outside` can never be relabeled as
    // an in-root path.
    if relative
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return None;
    }
    if relative.as_os_str().is_empty() || is_metadata_path(relative) {
        return None;
    }
    Some(relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/"))
}

fn read_head_file(repo_root: &Path, path: &str) -> Option<SnapshotEntry> {
    let spec = format!("HEAD:{path}");
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["show", spec.as_str()])
        .output()
        .ok()?;
    if output.status.success() {
        Some(SnapshotEntry::Content(output.stdout))
    } else {
        None
    }
}

fn read_worktree_file(path: &Path) -> Option<SnapshotEntry> {
    if !path.exists() {
        return None;
    }
    Some(match fs::read(path) {
        Ok(content) => SnapshotEntry::Content(content),
        Err(_) => SnapshotEntry::Unavailable,
    })
}

fn git_root(root: &Path) -> Result<PathBuf, GitError> {
    let output = command_output(root, &["rev-parse", "--show-toplevel"])?;
    let text = String::from_utf8(output)
        .map_err(|_| GitError::InvalidOutput("repository root is not valid UTF-8".to_string()))?;
    let path = PathBuf::from(text.trim());
    if !path.is_dir() {
        return Err(GitError::NotRepository);
    }
    path.canonicalize().map_err(|error| GitError::InvalidOutput(error.to_string()))
}

/// Read the current tracked, staged, deleted, renamed, and untracked changes
/// through Git's NUL-delimited machine-readable status output.
pub fn git_diff(root: &Path) -> Result<DiffState, GitError> {
    let selected_root = canonical_root(root).map_err(|_| GitError::NotRepository)?;
    let repo_root = git_root(&selected_root)?;
    let status = command_output(&selected_root, &["status", "--porcelain=v1", "-z", "--untracked-files=all"])?;
    let records = parse_status_paths(&status)?;
    let mut files = Vec::new();

    for (old_path, destination, code) in records {
        if code == [b'!', b'!'] {
            continue;
        }
        if code == [b'?', b'?'] {
            let Some(path) = normalize_git_path(&repo_root, &selected_root, &old_path) else {
                continue;
            };
            let after = read_worktree_file(&selected_root.join(path.replace('/', &std::path::MAIN_SEPARATOR.to_string())));
            files.push(file_diff(path, FileChangeStatus::Added, None, after.as_ref()));
            continue;
        }

        if let Some(destination) = destination {
            if let Some(old_relative) = normalize_git_path(&repo_root, &selected_root, &old_path) {
                let before = read_head_file(&repo_root, &old_path);
                files.push(file_diff(old_relative, FileChangeStatus::Deleted, before.as_ref(), None));
            }
            if let Some(new_relative) = normalize_git_path(&repo_root, &selected_root, &destination) {
                let after_path = selected_root.join(new_relative.replace('/', &std::path::MAIN_SEPARATOR.to_string()));
                let after = read_worktree_file(&after_path);
                files.push(file_diff(new_relative, FileChangeStatus::Added, None, after.as_ref()));
            }
            continue;
        }

        let Some(relative) = normalize_git_path(&repo_root, &selected_root, &old_path) else {
            continue;
        };
        let before = read_head_file(&repo_root, &old_path);
        let after_path = selected_root.join(relative.replace('/', &std::path::MAIN_SEPARATOR.to_string()));
        let after = read_worktree_file(&after_path);
        let status = if code[0] == b'D' || code[1] == b'D' {
            FileChangeStatus::Deleted
        } else if code[0] == b'A' || code[1] == b'A' {
            FileChangeStatus::Added
        } else {
            FileChangeStatus::Modified
        };
        files.push(file_diff(relative, status, before.as_ref(), after.as_ref()));
    }

    Ok(finalize_state(
        Some(selected_root.to_string_lossy().into_owned()),
        DiffSource::Git,
        false,
        files,
        None,
    ))
}

/// Prefer Git when it can describe the repository, but retain the deterministic
/// watch-start snapshot when Git is unavailable or when a path is not part of
/// Git's selected working tree.
pub fn state_for_baseline(root: &Path, baseline: &FileSnapshot) -> DiffState {
    let selected_root = canonical_root(root).ok();
    let Some(selected_root) = selected_root else {
        return diff_snapshots(None, baseline, &FileSnapshot::default());
    };
    let Ok(after) = capture_snapshot(&selected_root) else {
        return finalize_state(
            Some(selected_root.to_string_lossy().into_owned()),
            DiffSource::Snapshot,
            true,
            Vec::new(),
            Some("The current project snapshot could not be read".to_string()),
        );
    };
    let snapshot_state = diff_snapshots(Some(selected_root.to_string_lossy().into_owned()), baseline, &after);
    match git_diff(&selected_root) {
        Ok(git_state) => {
            // Git describes HEAD -> worktree, while the live change is
            // watch-start -> current.  Keep Git as the source marker, but
            // make the snapshot records authoritative for statuses, counts,
            // and path inclusion (including ignored/untracked files Git omits).
            finalize_state(
                git_state.project_path,
                DiffSource::Git,
                false,
                snapshot_state.files,
                None,
            )
        }
        Err(error) => DiffState {
            error: Some(format!("{error}; using snapshot comparison")),
            ..snapshot_state
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempProject(PathBuf);

    impl TempProject {
        fn new() -> Self {
            let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let path = std::env::temp_dir().join(format!("codex-mentor-diff-{stamp}"));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn git(&self, args: &[&str]) {
            let status = Command::new("git").arg("-C").arg(&self.0).args(args).status().unwrap();
            assert!(status.success(), "git {:?} failed", args);
        }
    }

    impl Drop for TempProject {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn snapshot(entries: &[(&str, SnapshotEntry)]) -> FileSnapshot {
        FileSnapshot {
            files: entries.iter().map(|(path, entry)| ((*path).to_string(), entry.clone())).collect(),
        }
    }

    #[test]
    fn excludes_git_and_codex_from_snapshots() {
        let project = TempProject::new();
        fs::write(project.0.join("README.md"), "hello").unwrap();
        fs::create_dir_all(project.0.join(".git/objects")).unwrap();
        fs::write(project.0.join(".git/objects/index"), "metadata").unwrap();
        fs::create_dir_all(project.0.join(".codex")).unwrap();
        fs::write(project.0.join(".codex/session.json"), "metadata").unwrap();
        assert_eq!(
            capture_snapshot(&project.0)
                .unwrap()
                .files
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["README.md".to_string()]
        );
    }

    #[test]
    fn classifies_add_modify_delete_and_reverted_changes() {
        let before = snapshot(&[("a.ts", SnapshotEntry::Content(b"one\n".to_vec()))]);
        let changed = snapshot(&[
            ("a.ts", SnapshotEntry::Content(b"two\n".to_vec())),
            ("nested/new.ts", SnapshotEntry::Content(b"new\n".to_vec())),
        ]);
        let state = diff_snapshots(None, &before, &changed);
        assert_eq!(
            state
                .files
                .iter()
                .map(|file| (file.path.as_str(), file.status))
                .collect::<Vec<_>>(),
            vec![
                ("a.ts", FileChangeStatus::Modified),
                ("nested/new.ts", FileChangeStatus::Added),
            ]
        );
        assert!(diff_snapshots(None, &before, &before).files.is_empty());
        let deleted = snapshot(&[]);
        assert_eq!(diff_snapshots(None, &before, &deleted).files[0].status, FileChangeStatus::Deleted);
    }

    #[test]
    fn counts_empty_crlf_and_missing_final_newline() {
        assert_eq!(count_line_changes(b"", b"").unwrap(), (0, 0));
        assert_eq!(line_count(b"").unwrap(), 0);
        assert_eq!(line_count(b"a").unwrap(), 1);
        assert_eq!(count_line_changes(b"", b"a").unwrap(), (1, 0));
        assert_eq!(count_line_changes(b"a", b"").unwrap(), (0, 1));
        assert_eq!(count_line_changes(b"a\r\nb\r\n", b"a\nb\n").unwrap(), (0, 0));
        assert_eq!(count_line_changes(b"a\n", b"a\nb").unwrap(), (1, 0));
    }

    #[test]
    fn binary_and_invalid_utf8_line_counts_are_unknown() {
        assert!(count_line_changes(b"a\0b", b"a\0c").is_err());
        assert!(count_line_changes(&[0xff], b"text").is_err());
        let before = snapshot(&[("binary", SnapshotEntry::Content(vec![0, 1]))]);
        let after = snapshot(&[("binary", SnapshotEntry::Content(vec![0, 2]))]);
        let file = &diff_snapshots(None, &before, &after).files[0];
        assert_eq!(file.lines_added, None);
        assert_eq!(file.lines_deleted, None);
    }

    #[test]
    fn git_reports_staged_unstaged_untracked_deleted_renamed_and_nested() {
        let project = TempProject::new();
        project.git(&["init", "-q"]);
        project.git(&["config", "user.email", "codex@example.test"]);
        project.git(&["config", "user.name", "Codex"]);
        fs::create_dir_all(project.0.join("src/nested")).unwrap();
        fs::write(project.0.join("src/main.ts"), "one\n").unwrap();
        fs::write(project.0.join("delete.ts"), "gone\n").unwrap();
        project.git(&["add", "."]);
        project.git(&["commit", "-qm", "initial"]);
        fs::write(project.0.join("src/main.ts"), "one\ntwo\n").unwrap();
        fs::write(project.0.join("untracked.ts"), "new\n").unwrap();
        fs::remove_file(project.0.join("delete.ts")).unwrap();
        fs::rename(project.0.join("src/main.ts"), project.0.join("src/nested/renamed.ts")).unwrap();
        let state = git_diff(&project.0).unwrap();
        let paths = state.files.iter().map(|file| file.path.as_str()).collect::<BTreeSet<_>>();
        assert!(paths.contains("src/main.ts"));
        assert!(paths.contains("src/nested/renamed.ts"));
        assert!(paths.contains("delete.ts"));
        assert!(paths.contains("untracked.ts"));
        assert_eq!(
            state
                .files
                .iter()
                .find(|file| file.path == "src/main.ts")
                .map(|file| file.status),
            Some(FileChangeStatus::Deleted)
        );
        assert_eq!(
            state
                .files
                .iter()
                .find(|file| file.path == "src/nested/renamed.ts")
                .map(|file| file.status),
            Some(FileChangeStatus::Added)
        );
    }

    #[test]
    fn non_git_directory_returns_a_clear_fallback() {
        let project = TempProject::new();
        fs::write(project.0.join("a.ts"), "one\n").unwrap();
        let baseline = capture_snapshot(&project.0).unwrap();
        fs::write(project.0.join("a.ts"), "two\n").unwrap();
        let state = state_for_baseline(&project.0, &baseline);
        assert_eq!(state.source, DiffSource::Snapshot);
        assert!(state.fallback);
        assert!(state.error.as_deref().unwrap_or_default().contains("snapshot"));
    }

    #[test]
    fn git_failure_is_reported_as_snapshot_fallback() {
        let project = TempProject::new();
        fs::write(project.0.join("a.ts"), "one\n").unwrap();
        fs::write(project.0.join(".git"), "not a repository").unwrap();
        let baseline = capture_snapshot(&project.0).unwrap();
        fs::write(project.0.join("a.ts"), "two\n").unwrap();
        let state = state_for_baseline(&project.0, &baseline);
        assert_eq!(state.source, DiffSource::Snapshot);
        assert!(state.fallback);
        assert!(state.error.is_some());
        assert_eq!(state.files[0].path, "a.ts");
    }

    #[test]
    fn ignored_snapshot_changes_are_retained_when_git_omits_them() {
        let project = TempProject::new();
        project.git(&["init", "-q"]);
        project.git(&["config", "user.email", "codex@example.test"]);
        project.git(&["config", "user.name", "Codex"]);
        fs::write(project.0.join(".gitignore"), "ignored.txt\n").unwrap();
        project.git(&["add", ".gitignore"]);
        project.git(&["commit", "-qm", "initial"]);

        let baseline = capture_snapshot(&project.0).unwrap();
        fs::write(project.0.join("ignored.txt"), "new\n").unwrap();

        let state = state_for_baseline(&project.0, &baseline);
        assert_eq!(state.source, DiffSource::Git);
        assert!(!state.fallback);
        assert_eq!(state.error, None);
        assert_eq!(state.files.len(), 1);
        assert_eq!(state.files[0].path, "ignored.txt");
        assert_eq!(state.files[0].status, FileChangeStatus::Added);
        assert_eq!(state.files[0].lines_added, Some(1));
        assert_eq!(state.files[0].lines_deleted, Some(0));
    }

    #[test]
    fn watch_start_baseline_wins_over_preexisting_git_changes() {
        let project = TempProject::new();
        project.git(&["init", "-q"]);
        project.git(&["config", "user.email", "codex@example.test"]);
        project.git(&["config", "user.name", "Codex"]);
        fs::write(project.0.join("tracked.ts"), "head\n").unwrap();
        project.git(&["add", "tracked.ts"]);
        project.git(&["commit", "-qm", "initial"]);

        // These edits pre-date the watch and therefore belong in its
        // baseline, even though Git still compares them to HEAD.
        fs::write(project.0.join("tracked.ts"), "head\nbefore\n").unwrap();
        fs::write(project.0.join("untracked.ts"), "before\n").unwrap();
        let baseline = capture_snapshot(&project.0).unwrap();

        fs::write(project.0.join("tracked.ts"), "head\nbefore\nafter\n").unwrap();
        fs::write(project.0.join("untracked.ts"), "before\nafter\n").unwrap();

        let state = state_for_baseline(&project.0, &baseline);
        let tracked = state.files.iter().find(|file| file.path == "tracked.ts").unwrap();
        assert_eq!(tracked.status, FileChangeStatus::Modified);
        assert_eq!(tracked.lines_added, Some(1));
        assert_eq!(tracked.lines_deleted, Some(0));
        let untracked = state.files.iter().find(|file| file.path == "untracked.ts").unwrap();
        assert_eq!(untracked.status, FileChangeStatus::Modified);
        assert_eq!(untracked.lines_added, Some(1));
        assert_eq!(untracked.lines_deleted, Some(0));
    }

    #[test]
    fn normalize_git_path_rejects_lexical_parent_traversal() {
        let project = TempProject::new();
        assert!(normalize_git_path(&project.0, &project.0, "../outside.ts").is_none());
    }
}

use crate::diff::{self, ContentStatus, DiffState, FileSnapshot, SnapshotEntry};
use crate::watcher::FileChangeStatus;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Component, Path};

pub const ANALYSIS_STATE_EVENT: &str = "analysis-state";

/// The fields in this type are the canonical Change Record fields from the
/// workflow specification.  Keep lifecycle and source details in
/// [`ChangeAnalysis::metadata`] so explanation modes can reuse this record
/// without adding mode-specific analysis fields.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChangeRecord {
    pub summary: String,
    pub purpose: String,
    pub changed_components: Vec<String>,
    pub key_decisions: Vec<String>,
    pub how_it_works: String,
    pub impact: String,
    pub risk: String,
    pub review_priority: String,
    pub programming_concepts: Vec<String>,
    pub relevant_code_locations: Vec<String>,
}

/// Optional, explicitly supplied completion information.  The local fallback
/// never invents this data; absent values remain `null` in the serialized
/// analysis and unsupported core prose remains honest `Unknown` text.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CompletionMetadata {
    pub task: Option<String>,
    pub plan: Option<Vec<String>>,
    pub completion: Option<String>,
    pub tests: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisMetadata {
    pub project_path: String,
    pub source: String,
    pub completion: String,
    pub changed_file_count: usize,
    pub supplied: CompletionMetadata,
}

/// Source included in the scoped context for one changed file.  Binary and
/// unavailable entries retain status metadata but deliberately carry no bytes.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScopedFileContext {
    pub path: String,
    pub status: FileChangeStatus,
    pub content_status: ContentStatus,
    pub before: Option<String>,
    pub after: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisContext {
    pub project_path: String,
    pub diff: DiffState,
    pub files: Vec<ScopedFileContext>,
    pub supplied: CompletionMetadata,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChangeAnalysis {
    pub record: ChangeRecord,
    pub metadata: AnalysisMetadata,
    /// The completed change's own before/after records.  These remain frozen
    /// after the watcher rotates to a new live baseline.
    pub frozen_files: Vec<ScopedFileContext>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AnalysisStatus {
    Idle,
    Available,
    Error,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisState {
    pub status: AnalysisStatus,
    pub analysis: Option<ChangeAnalysis>,
    pub error: Option<String>,
}

impl Default for AnalysisState {
    fn default() -> Self {
        Self {
            status: AnalysisStatus::Idle,
            analysis: None,
            error: None,
        }
    }
}

fn is_scoped_relative_path(path: &str) -> bool {
    let path = Path::new(path);
    if path.is_absolute() {
        return false;
    }
    let mut has_component = false;
    for component in path.components() {
        match component {
            Component::Normal(name) => {
                has_component = true;
                if name == ".git" || name == ".codex" {
                    return false;
                }
            }
            Component::CurDir | Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return false;
            }
        }
    }
    has_component
}

fn scoped_snapshot(snapshot: &FileSnapshot) -> FileSnapshot {
    FileSnapshot {
        files: snapshot
            .files
            .iter()
            .filter(|(path, _)| is_scoped_relative_path(path))
            .map(|(path, entry)| (path.clone(), entry.clone()))
            .collect::<BTreeMap<_, _>>(),
    }
}

fn text_entry(entry: Option<&SnapshotEntry>) -> Option<String> {
    let Some(SnapshotEntry::Content(content)) = entry else {
        return None;
    };
    if content.contains(&0) {
        return None;
    }
    String::from_utf8(content.clone()).ok()
}

fn context_files(
    diff_state: &DiffState,
    before: &FileSnapshot,
    after: &FileSnapshot,
) -> Vec<ScopedFileContext> {
    diff_state
        .files
        .iter()
        .filter(|file| is_scoped_relative_path(&file.path))
        .map(|file| {
            let before_entry = before.files.get(&file.path);
            let after_entry = after.files.get(&file.path);
            let (before_text, after_text) = if file.content_status == ContentStatus::Text {
                (text_entry(before_entry), text_entry(after_entry))
            } else {
                // Binary and unavailable source is intentionally metadata-only.
                (None, None)
            };
            ScopedFileContext {
                path: file.path.clone(),
                status: file.status,
                content_status: file.content_status.clone(),
                before: before_text,
                after: after_text,
            }
        })
        .collect()
}

/// Build the only context permitted for a local Change Analysis.
///
/// Snapshots are filtered to changed-file paths and metadata directories before
/// diffing.  This prevents unrelated repository files, `.git`, and `.codex`
/// content from entering the analysis even when a caller supplies a hand-built
/// snapshot in tests.
pub fn build_context(
    project_path: &Path,
    before: &FileSnapshot,
    after: &FileSnapshot,
    supplied: CompletionMetadata,
) -> Result<Option<AnalysisContext>, String> {
    let project_path = project_path.to_string_lossy().into_owned();
    let before = scoped_snapshot(before);
    let after = scoped_snapshot(after);
    let diff = diff::diff_snapshots(Some(project_path.clone()), &before, &after);
    if diff.files.is_empty() {
        return Ok(None);
    }
    let files = context_files(&diff, &before, &after);
    if files.is_empty() {
        return Ok(None);
    }
    Ok(Some(AnalysisContext {
        project_path,
        diff,
        files,
        supplied,
    }))
}

fn unknown(text: &str) -> String {
    format!("Unknown: {text} was not supplied.")
}

/// Produce deterministic prose from the scoped context.  This is deliberately
/// conservative: only file facts and explicitly supplied metadata become
/// claims; product/runtime intent is reported as unknown when absent.
pub fn record_from_context(context: &AnalysisContext) -> ChangeRecord {
    let files = &context.files;
    let file_count = files.len();
    let noun = if file_count == 1 { "file" } else { "files" };
    let mut risk_files = files
        .iter()
        .filter(|file| {
            file.status == FileChangeStatus::Deleted
                || file.content_status != ContentStatus::Text
        })
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    risk_files.sort();

    let purpose = context
        .supplied
        .task
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| unknown("task purpose"));
    let key_decisions = context
        .supplied
        .plan
        .clone()
        .unwrap_or_default()
        .into_iter()
        .filter(|value| !value.trim().is_empty())
        .collect::<Vec<_>>();
    let risk = if risk_files.is_empty() {
        unknown("risk assessment")
    } else {
        format!("Review deleted, binary, or unavailable content: {}.", risk_files.join(", "))
    };
    let review_priority = if risk_files.is_empty() {
        unknown("review priority")
    } else {
        "high".to_string()
    };

    ChangeRecord {
        summary: format!("{file_count} {noun} changed since the watch-start snapshot."),
        purpose,
        changed_components: files.iter().map(|file| file.path.clone()).collect(),
        key_decisions,
        how_it_works: "Mentor compares the frozen watch-start snapshot with the frozen current snapshot for each changed file.".to_string(),
        impact: unknown("runtime and product impact"),
        risk,
        review_priority,
        programming_concepts: Vec::new(),
        relevant_code_locations: files.iter().map(|file| file.path.clone()).collect(),
    }
}

/// Build a reusable canonical analysis from two already-frozen snapshots.
/// `None` means the frozen state has no change and must not emit an analysis.
pub fn build_analysis(
    project_path: &Path,
    before: &FileSnapshot,
    after: &FileSnapshot,
    supplied: CompletionMetadata,
) -> Result<Option<ChangeAnalysis>, String> {
    let Some(context) = build_context(project_path, before, after, supplied.clone())? else {
        return Ok(None);
    };
    let record = record_from_context(&context);
    Ok(Some(ChangeAnalysis {
        record,
        metadata: AnalysisMetadata {
            project_path: context.project_path,
            source: "local-snapshot".to_string(),
            completion: "explicit".to_string(),
            changed_file_count: context.files.len(),
            supplied,
        },
        frozen_files: context.files,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn snapshot(entries: &[(&str, SnapshotEntry)]) -> FileSnapshot {
        FileSnapshot {
            files: entries
                .iter()
                .map(|(path, entry)| ((*path).to_string(), entry.clone()))
                .collect(),
        }
    }

    #[test]
    fn scopes_changed_files_and_excludes_metadata_and_unrelated_files() {
        let before = snapshot(&[
            ("src/app.ts", SnapshotEntry::Content(b"old\n".to_vec())),
            ("README.md", SnapshotEntry::Content(b"same\n".to_vec())),
            (".git/config", SnapshotEntry::Content(b"secret\n".to_vec())),
        ]);
        let after = snapshot(&[
            ("src/app.ts", SnapshotEntry::Content(b"new\n".to_vec())),
            ("README.md", SnapshotEntry::Content(b"same\n".to_vec())),
            ("notes.txt", SnapshotEntry::Content(b"new file\n".to_vec())),
            (".codex/session.json", SnapshotEntry::Content(b"hidden\n".to_vec())),
        ]);

        let context = build_context(
            Path::new("C:/project"),
            &before,
            &after,
            CompletionMetadata::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            context.files.iter().map(|file| file.path.as_str()).collect::<Vec<_>>(),
            vec!["notes.txt", "src/app.ts"]
        );
        assert!(!context
            .files
            .iter()
            .any(|file| file.path.starts_with(".")));
    }

    #[test]
    fn text_is_included_but_binary_and_unavailable_are_metadata_only() {
        let before = snapshot(&[
            ("text.ts", SnapshotEntry::Content(b"old\n".to_vec())),
            ("binary.bin", SnapshotEntry::Content(vec![0, 1, 2])),
            ("locked.txt", SnapshotEntry::Unavailable),
        ]);
        let after = snapshot(&[
            ("text.ts", SnapshotEntry::Content(b"new\n".to_vec())),
            ("binary.bin", SnapshotEntry::Content(vec![0, 1, 3])),
            ("locked.txt", SnapshotEntry::Content(b"now readable\n".to_vec())),
        ]);
        let context = build_context(
            Path::new("C:/project"),
            &before,
            &after,
            CompletionMetadata::default(),
        )
        .unwrap()
        .unwrap();
        let text = context.files.iter().find(|file| file.path == "text.ts").unwrap();
        assert_eq!(text.before.as_deref(), Some("old\n"));
        assert_eq!(text.after.as_deref(), Some("new\n"));
        for path in ["binary.bin", "locked.txt"] {
            let file = context.files.iter().find(|file| file.path == path).unwrap();
            assert!(file.before.is_none());
            assert!(file.after.is_none());
        }
    }

    #[test]
    fn deterministic_fields_are_stable_and_unknowns_are_honest() {
        let before = snapshot(&[("src/app.ts", SnapshotEntry::Content(b"old\n".to_vec()))]);
        let after = snapshot(&[("src/app.ts", SnapshotEntry::Content(b"new\n".to_vec()))]);
        let first = build_analysis(
            Path::new("C:/project"),
            &before,
            &after,
            CompletionMetadata::default(),
        )
        .unwrap()
        .unwrap();
        let second = build_analysis(
            Path::new("C:/project"),
            &before,
            &after,
            CompletionMetadata::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(first.record.changed_components, vec!["src/app.ts"]);
        assert_eq!(first.record.impact, "Unknown: runtime and product impact was not supplied.");
        assert!(first.record.programming_concepts.is_empty());
    }

    #[test]
    fn empty_diff_returns_no_analysis() {
        let before = snapshot(&[("same.ts", SnapshotEntry::Content(b"same\n".to_vec()))]);
        let result = build_analysis(
            &PathBuf::from("C:/project"),
            &before,
            &before,
            CompletionMetadata::default(),
        )
        .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn rotating_the_baseline_makes_repeated_completion_idempotent() {
        let initial = snapshot(&[("app.ts", SnapshotEntry::Content(b"one\n".to_vec()))]);
        let completed_snapshot = snapshot(&[("app.ts", SnapshotEntry::Content(b"two\n".to_vec()))]);
        let next_change = snapshot(&[("app.ts", SnapshotEntry::Content(b"three\n".to_vec()))]);

        let first = build_analysis(
            Path::new("C:/project"),
            &initial,
            &completed_snapshot,
            CompletionMetadata::default(),
        )
        .unwrap()
        .unwrap();
        assert!(build_analysis(
            Path::new("C:/project"),
            &completed_snapshot,
            &completed_snapshot,
            CompletionMetadata::default(),
        )
        .unwrap()
        .is_none());

        let second = build_analysis(
            Path::new("C:/project"),
            &completed_snapshot,
            &next_change,
            CompletionMetadata::default(),
        )
        .unwrap()
        .unwrap();
        assert_ne!(first, second);
        assert_eq!(second.record.changed_components, vec!["app.ts"]);
    }

    #[test]
    fn completed_analysis_keeps_frozen_preview_when_the_same_path_changes_again() {
        let before = snapshot(&[(
            "app.ts",
            SnapshotEntry::Content(b"before\n".to_vec()),
        )]);
        let completed = snapshot(&[(
            "app.ts",
            SnapshotEntry::Content(b"completed\n".to_vec()),
        )]);
        let later = snapshot(&[(
            "app.ts",
            SnapshotEntry::Content(b"later\n".to_vec()),
        )]);

        let first = build_analysis(
            Path::new("C:/project"),
            &before,
            &completed,
            CompletionMetadata::default(),
        )
        .unwrap()
        .unwrap();
        let second = build_analysis(
            Path::new("C:/project"),
            &completed,
            &later,
            CompletionMetadata::default(),
        )
        .unwrap()
        .unwrap();

        let first_file = &first.frozen_files[0];
        assert_eq!(first_file.path, "app.ts");
        assert_eq!(first_file.before.as_deref(), Some("before\n"));
        assert_eq!(first_file.after.as_deref(), Some("completed\n"));
        assert_eq!(second.frozen_files[0].after.as_deref(), Some("later\n"));
        assert_ne!(first.frozen_files, second.frozen_files);
    }
}

use crate::diff::{self, ContentStatus, DiffState, FileSnapshot, SnapshotEntry};
use crate::watcher::FileChangeStatus;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
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
    /// Monotonic watcher generation at the completed-change boundary.  This
    /// remains distinct even when two sequential changes have identical
    /// snapshots and metadata.
    pub completion_generation: u64,
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

const CONCEPT_MAP: &str = include_str!("../../teaching-source/concept-map.json");

#[derive(Deserialize)]
struct ConceptMap {
    concepts: Vec<ConceptDefinition>,
}

#[derive(Deserialize)]
struct ConceptDefinition {
    id: String,
    #[serde(default)]
    prerequisites: Vec<String>,
}

#[derive(Clone, Copy)]
struct LexToken<'a> {
    text: &'a str,
    start: usize,
}

#[derive(Clone, Copy)]
struct FunctionDeclaration {
    is_async: bool,
    body_start: Option<usize>,
    body_end: Option<usize>,
}

#[derive(Default)]
struct CodeFeatures {
    functions: bool,
    api: bool,
    async_await: bool,
}

fn push_masked_space(masked: &mut String, character: char) {
    for _ in 0..character.len_utf8() {
        masked.push(' ');
    }
}

fn mask_non_code(text: &str) -> String {
    let mut masked = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut quote = None;
    let mut line_comment = false;
    let mut block_comment_depth = 0;

    while let Some(character) = chars.next() {
        if line_comment {
            if character == '\n' {
                line_comment = false;
                masked.push(character);
            } else if character == '\r' {
                masked.push(character);
            } else {
                push_masked_space(&mut masked, character);
            }
            continue;
        }
        if block_comment_depth > 0 {
            if character == '/' && chars.peek() == Some(&'*') {
                chars.next();
                masked.push_str("  ");
                block_comment_depth += 1;
            } else if character == '*' && chars.peek() == Some(&'/') {
                chars.next();
                masked.push_str("  ");
                block_comment_depth -= 1;
            } else if matches!(character, '\n' | '\r') {
                masked.push(character);
            } else {
                push_masked_space(&mut masked, character);
            }
            continue;
        }
        if let Some(expected) = quote {
            if character == '\\' {
                push_masked_space(&mut masked, character);
                if let Some(escaped) = chars.next() {
                    if matches!(escaped, '\n' | '\r') {
                        masked.push(escaped);
                    } else {
                        push_masked_space(&mut masked, escaped);
                    }
                }
            } else if character == expected {
                push_masked_space(&mut masked, character);
                quote = None;
            } else if matches!(character, '\n' | '\r') {
                masked.push(character);
            } else {
                push_masked_space(&mut masked, character);
            }
            continue;
        }

        if character == '/' && chars.peek() == Some(&'/') {
            chars.next();
            masked.push_str("  ");
            line_comment = true;
        } else if character == '/' && chars.peek() == Some(&'*') {
            chars.next();
            masked.push_str("  ");
            block_comment_depth = 1;
        } else if character == '#' && chars.peek() != Some(&'[') {
            push_masked_space(&mut masked, character);
            line_comment = true;
        } else if matches!(character, '"' | '\'' | '`') {
            push_masked_space(&mut masked, character);
            quote = Some(character);
        } else if character.is_ascii() {
            masked.push(character);
        } else {
            // Keep the masked input valid UTF-8 and make non-ASCII prose a
            // separator instead of slicing it as if it were one byte.
            push_masked_space(&mut masked, character);
        }
    }
    masked
}

fn lex_tokens(text: &str) -> Vec<LexToken<'_>> {
    let mut tokens = Vec::new();
    let mut characters = text.char_indices().peekable();
    while let Some((start, character)) = characters.next() {
        if character.is_ascii_alphabetic() || matches!(character, '_' | '$') {
            let end = loop {
                let Some(&(index, next)) = characters.peek() else {
                    break text.len();
                };
                if next.is_ascii_alphanumeric() || matches!(next, '_' | '$') {
                    characters.next();
                } else {
                    break index;
                }
            };
            tokens.push(LexToken {
                text: &text[start..end],
                start,
            });
        } else if !character.is_whitespace() {
            let end = start + character.len_utf8();
            tokens.push(LexToken {
                text: &text[start..end],
                start,
            });
        }
    }
    tokens
}

fn line_prefix_is_structural(text: &str, start: usize) -> bool {
    let line_start = text[..start].rfind('\n').map_or(0, |index| index + 1);
    let tokens = lex_tokens(&text[line_start..start]);
    let mut index = 0;
    while index < tokens.len() {
        if matches!(
            tokens[index].text,
            "pub" | "public" | "export" | "async" | "unsafe"
        ) {
            index += 1;
        } else if index + 7 <= tokens.len()
            && tokens[index..index + 7]
                .iter()
                .map(|token| token.text)
                .eq(["#", "[", "tauri", ":", ":", "command", "]"])
        {
            index += 7;
        } else {
            return false;
        }
    }
    true
}

fn directly_qualified(tokens: &[LexToken<'_>], declaration: usize) -> bool {
    if declaration >= 7
        && tokens[declaration - 7..declaration]
            .iter()
            .map(|token| token.text)
            .eq(["#", "[", "tauri", ":", ":", "command", "]"])
    {
        return true;
    }
    let mut index = declaration;
    while index > 0 && matches!(tokens[index - 1].text, "async" | "pub" | "public" | "export") {
        index -= 1;
    }
    index < declaration
        && tokens[index..declaration]
            .iter()
            .any(|token| matches!(token.text, "pub" | "public" | "export"))
}

fn find_matching(tokens: &[LexToken<'_>], opening: usize, open: &str, close: &str) -> Option<usize> {
    let mut depth = 0;
    for index in opening..tokens.len() {
        match tokens[index].text {
            token if token == open => depth += 1,
            token if token == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

fn is_identifier(token: &str) -> bool {
    let mut characters = token.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || matches!(first, '_' | '$'))
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '$')
        })
}

fn declaration_boundary(
    text: &str,
    tokens: &[LexToken<'_>],
    start: usize,
    expression_body: bool,
) -> Option<(Option<usize>, Option<usize>)> {
    for index in start..tokens.len() {
        match tokens[index].text {
            "{" => {
                if start > 0 {
                    let previous = &tokens[start - 1];
                    let between = &text[previous.start + previous.text.len()..tokens[index].start];
                    if !between.chars().all(|character| character.is_whitespace()) {
                        return None;
                    }
                }
                let close = find_matching(tokens, index, "{", "}")?;
                return Some((Some(index + 1), Some(close)));
            }
            ";" => {
                if start > 0
                    && text[tokens[start - 1].start..tokens[index].start].contains('\n')
                {
                    return None;
                }
                return Some(if expression_body {
                    (Some(start), Some(index))
                } else {
                    (None, None)
                });
            }
            "}" => return None,
            _ => {}
        }
    }
    None
}

fn code_features(text: &str) -> CodeFeatures {
    let masked = mask_non_code(text);
    let tokens = lex_tokens(&masked);
    let mut features = CodeFeatures::default();
    let mut declarations = Vec::new();
    for index in 0..tokens.len() {
        let token = tokens[index].text;
        let function_like = matches!(token, "fn" | "function" | "def" | "func" | "fun")
            && tokens.get(index + 1).is_some_and(|next| is_identifier(next.text))
            && tokens.get(index + 2).is_some_and(|next| next.text == "(");
        let arrow = matches!(token, "const" | "let" | "var")
            && tokens.get(index + 1).is_some_and(|next| is_identifier(next.text))
            && tokens.get(index + 2).is_some_and(|next| next.text == "=")
            && tokens.get(index + 3).is_some_and(|next| {
                next.text == "(" || next.text == "async"
            });
        if function_like && line_prefix_is_structural(&masked, tokens[index].start) {
            let opening = index + 2;
            let Some(close) = find_matching(&tokens, opening, "(", ")") else {
                continue;
            };
            let boundary = if token == "def" {
                (tokens.get(close + 1).map(|next| next.text) == Some(":"))
                    .then_some((None, None))
            } else {
                declaration_boundary(text, &tokens, close + 1, false)
            };
            let Some((body_start, body_end)) = boundary else {
                continue;
            };
            features.functions = true;
            features.api |= directly_qualified(&tokens, index);
            declarations.push(FunctionDeclaration {
                is_async: index > 0 && tokens[index - 1].text == "async",
                body_start,
                body_end,
            });
        } else if arrow && line_prefix_is_structural(&masked, tokens[index].start) {
            let mut opening = index + 3;
            let is_async = tokens[opening].text == "async";
            if is_async {
                opening += 1;
            }
            if tokens.get(opening).map_or(true, |next| next.text != "(") {
                continue;
            }
            let Some(close) = find_matching(&tokens, opening, "(", ")") else {
                continue;
            };
            if tokens.get(close + 1).map_or(true, |next| next.text != "=")
                || tokens.get(close + 2).map_or(true, |next| next.text != ">")
            {
                continue;
            }
            let Some((body_start, body_end)) =
                declaration_boundary(text, &tokens, close + 3, true)
            else {
                continue;
            };
            features.functions = true;
            features.api |= directly_qualified(&tokens, index);
            declarations.push(FunctionDeclaration {
                is_async,
                body_start,
                body_end,
            });
        }
    }

    for declaration in declarations {
        let Some(body_start) = declaration.body_start else {
            continue;
        };
        let Some(body_end) = declaration.body_end else {
            continue;
        };
        if declaration.is_async {
            features.async_await |= tokens[body_start..body_end]
                .iter()
                .enumerate()
                .any(|(offset, token)| {
                    token.text == "await"
                        && (offset == 0 || tokens[body_start + offset - 1].text != ".")
                });
        }
    }
    features
}

fn valid_concept_map(map_text: &str) -> Option<Vec<ConceptDefinition>> {
    let map = serde_json::from_str::<ConceptMap>(map_text).ok()?;
    if map.concepts.is_empty() {
        return None;
    }
    let mut ids = BTreeSet::new();
    for concept in &map.concepts {
        if concept.id.is_empty() || !ids.insert(concept.id.clone()) {
            return None;
        }
    }
    let indexes = map
        .concepts
        .iter()
        .enumerate()
        .map(|(index, concept)| (concept.id.clone(), index))
        .collect::<BTreeMap<_, _>>();
    if map.concepts.iter().enumerate().any(|(index, concept)| {
        concept.prerequisites.iter().any(|id| {
            indexes
                .get(id)
                .map_or(true, |prerequisite| *prerequisite >= index)
        })
    }) {
        return None;
    }

    fn visits_cycle(
        index: usize,
        concepts: &[ConceptDefinition],
        indexes: &BTreeMap<String, usize>,
        states: &mut [u8],
    ) -> bool {
        match states[index] {
            1 => return true,
            2 => return false,
            _ => states[index] = 1,
        }
        for prerequisite in &concepts[index].prerequisites {
            let Some(&prerequisite_index) = indexes.get(prerequisite) else {
                return true;
            };
            if visits_cycle(prerequisite_index, concepts, indexes, states) {
                return true;
            }
        }
        states[index] = 2;
        false
    }

    let mut states = vec![0; map.concepts.len()];
    if (0..map.concepts.len()).any(|index| visits_cycle(index, &map.concepts, &indexes, &mut states)) {
        return None;
    }
    Some(map.concepts)
}

fn concepts_from_texts<'a, I>(texts: I, map_text: &str) -> Vec<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let Some(concepts) = valid_concept_map(map_text) else {
        return Vec::new();
    };
    let mut detected = BTreeSet::new();
    for text in texts {
        let features = code_features(text);
        if features.functions {
            detected.insert("functions");
        }
        if features.api {
            detected.insert("api");
        }
        if features.async_await {
            detected.insert("async-await");
        }
    }
    let indexes = concepts
        .iter()
        .enumerate()
        .map(|(index, concept)| (concept.id.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    let mut output = BTreeSet::new();
    let mut pending = detected.into_iter().map(str::to_string).collect::<Vec<_>>();
    while let Some(id) = pending.pop() {
        let Some(&index) = indexes.get(id.as_str()) else {
            continue;
        };
        if output.insert(id) {
            pending.extend(concepts[index].prerequisites.iter().cloned());
        }
    }
    concepts
        .into_iter()
        .filter(|concept| output.contains(&concept.id))
        .map(|concept| concept.id)
        .collect()
}

fn programming_concepts(files: &[ScopedFileContext]) -> Vec<String> {
    concepts_from_texts(
        files
            .iter()
            .filter(|file| file.content_status == ContentStatus::Text)
            .flat_map(|file| [file.before.as_deref(), file.after.as_deref()])
            .flatten(),
        CONCEPT_MAP,
    )
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
        programming_concepts: programming_concepts(files),
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
    build_analysis_with_generation(project_path, before, after, supplied, 0)
}

/// Build a completed analysis with the watcher generation that owns its
/// frozen boundary.  The zero-generation wrapper above keeps direct
/// deterministic unit fixtures concise; real completion uses this function's
/// boundary identity.
pub fn build_analysis_with_generation(
    project_path: &Path,
    before: &FileSnapshot,
    after: &FileSnapshot,
    supplied: CompletionMetadata,
    completion_generation: u64,
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
            completion_generation,
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
    fn build_analysis_detects_concepts_from_frozen_text_in_any_path() {
        let before = snapshot(&[(
            "notes.weird",
            SnapshotEntry::Content(b"const add = (left, right) => left + right;\n".to_vec()),
        )]);
        let after = snapshot(&[(
            "notes.weird",
            SnapshotEntry::Content(
                b"#[tauri::command]\npub fn serve() {}\nasync fn load() { await fetch(); }\n"
                    .to_vec(),
            ),
        )]);
        let analysis = build_analysis(
            Path::new("C:/project"),
            &before,
            &after,
            CompletionMetadata::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            analysis.record.programming_concepts,
            vec!["functions", "api", "async-await"]
        );
    }

    #[test]
    fn comments_strings_near_tokens_and_prose_do_not_detect_concepts() {
        let before = snapshot(&[(
            "plain.data",
            SnapshotEntry::Content(
                b"// fn fake() {}\n/* pub fn hidden() {} */\nlet value = \"function text() async await\";\n"
                    .to_vec(),
            ),
        )]);
        let after = snapshot(&[(
            "plain.data",
            SnapshotEntry::Content(
                b"myfunction fake(\nasyncThing awaitable\nThe function prose() and async await are words.\n"
                    .to_vec(),
            ),
        )]);
        let analysis = build_analysis(
            Path::new("C:/project"),
            &before,
            &after,
            CompletionMetadata::default(),
        )
        .unwrap()
        .unwrap();
        assert!(analysis.record.programming_concepts.is_empty());
    }

    #[test]
    fn nested_block_comments_do_not_detect_hidden_functions() {
        let features = code_features("/* outer /* inner */ pub fn fake() {} */\n");
        assert!(!features.functions);
        assert!(!features.api);
        assert!(!features.async_await);
    }

    #[test]
    fn unicode_masking_and_tokenization_do_not_panic_or_corrupt_code() {
        let text = "// 中文 fn fake() {}\npub fn serve() { let message = \"你好\"; }\n";
        let features = code_features(text);
        assert!(features.functions);
        assert!(!features.async_await);
        let multibyte_text = "// \u{524d}\u{7f6e} \u{4e2d}\u{6587}\n\npub /* \u{4e2d}\u{9593} \u{6587}\u{5b57} */ fn serve() {}\n";
        let multibyte_features = code_features(multibyte_text);
        assert!(multibyte_features.functions);
        assert!(!multibyte_features.async_await);
    }

    #[test]
    fn function_detection_requires_structural_declarations() {
        assert!(!code_features("function prose()\nobj.await;\n").functions);
        assert!(!code_features("function prose()\nwords {}\n").functions);
        assert!(!code_features("prose #[tauri::command] fn serve() {}\n").functions);
        assert!(!code_features("fn declaration()\n").functions);
        assert!(code_features("pub fn serve()\n{}\n").functions);
        let tauri_command = code_features("#[tauri::command] fn serve() {}\n");
        assert!(tauri_command.functions);
        assert!(tauri_command.api);
        assert!(code_features("def serve():\n    pass\n").functions);
        assert!(code_features("async def serve():\n    pass\n").functions);
        assert!(code_features("function real() { return 1; }\n").functions);
    }

    #[test]
    fn async_function_with_member_await_is_not_async_await() {
        assert!(!code_features("function regular() { obj.await; }\n").async_await);
        assert!(!code_features("async function load() { obj.await; }\n").async_await);
    }

    #[test]
    fn async_function_without_await_is_not_async_await() {
        assert!(!code_features("async function load() { return fetch(); }\n").async_await);
    }

    #[test]
    fn async_function_with_standalone_await_is_async_await() {
        assert!(code_features("async function load() { await fetch(); }\n").async_await);
    }

    #[test]
    fn non_text_scoped_context_is_never_used_for_concepts() {
        let files = vec![
            ScopedFileContext {
                path: "binary.bin".to_string(),
                status: FileChangeStatus::Modified,
                content_status: ContentStatus::Binary,
                before: Some("pub fn hidden() {}".to_string()),
                after: Some("pub fn hidden() {}".to_string()),
            },
            ScopedFileContext {
                path: "text.rs".to_string(),
                status: FileChangeStatus::Modified,
                content_status: ContentStatus::Text,
                before: Some("old".to_string()),
                after: Some("pub fn visible() {}".to_string()),
            },
        ];
        assert_eq!(programming_concepts(&files), vec!["functions"]);
    }

    #[test]
    fn binary_and_unavailable_frozen_entries_have_no_concepts() {
        let before = snapshot(&[
            ("blob.data", SnapshotEntry::Content(vec![0, 1, 2])),
            ("locked.data", SnapshotEntry::Unavailable),
        ]);
        let after = snapshot(&[
            ("blob.data", SnapshotEntry::Content(vec![0, 1, 3])),
            ("locked.data", SnapshotEntry::Unavailable),
        ]);
        let analysis = build_analysis(
            Path::new("C:/project"),
            &before,
            &after,
            CompletionMetadata::default(),
        )
        .unwrap()
        .unwrap();
        assert!(analysis.record.programming_concepts.is_empty());
    }

    #[test]
    fn invalid_concept_map_emits_no_concepts() {
        assert!(concepts_from_texts(["pub fn valid() {}"], "not json").is_empty());
        assert!(concepts_from_texts(
            ["pub fn valid() {}"],
            r#"{"concepts":[]}"#
        )
        .is_empty());
        assert!(concepts_from_texts(
            ["pub fn valid() {}"],
            r#"{"concepts":[{"id":"functions","prerequisites":["api"]},{"id":"api","prerequisites":[]}]}"#
        )
        .is_empty());
        assert!(concepts_from_texts(
            ["pub fn valid() {}"],
            r#"{"concepts":[{"id":"functions","prerequisites":["api"]},{"id":"api","prerequisites":["functions"]}]}"#
        )
        .is_empty());
    }

    #[test]
    fn concept_output_contains_only_valid_map_ids_in_map_order() {
        let map = r#"{
            "concepts": [
                {"id":"functions","prerequisites":[]},
                {"id":"api","prerequisites":["functions"]},
                {"id":"unused","prerequisites":[]}
            ]
        }"#;
        assert_eq!(
            concepts_from_texts(["#[tauri::command]\npub fn serve() {}"], map),
            vec!["functions", "api"]
        );
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
    fn completion_generation_distinguishes_identical_sequential_boundaries() {
        let before = snapshot(&[("src/app.ts", SnapshotEntry::Content(b"old\n".to_vec()))]);
        let after = snapshot(&[("src/app.ts", SnapshotEntry::Content(b"new\n".to_vec()))]);
        let first = build_analysis_with_generation(
            Path::new("C:/project"),
            &before,
            &after,
            CompletionMetadata::default(),
            1,
        )
        .unwrap()
        .unwrap();
        let second = build_analysis_with_generation(
            Path::new("C:/project"),
            &before,
            &after,
            CompletionMetadata::default(),
            2,
        )
        .unwrap()
        .unwrap();
        assert_ne!(first.metadata.completion_generation, second.metadata.completion_generation);
        assert_ne!(first, second);
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

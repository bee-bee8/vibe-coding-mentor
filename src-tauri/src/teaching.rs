use crate::learning_memory::{self, LearningMemoryAppState, LearningMemoryRecord};
use crate::mentor;
use crate::watcher::{self, AppState as WatcherAppState, MentorContext};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use tauri::{AppHandle, Emitter, State};

pub const TEACHING_STATE_EVENT: &str = "teaching-state";
const GUIDE: &str = include_str!("../../teaching-source/TEACHING_GUIDE.md");
const CONCEPT_MAP: &str = include_str!("../../teaching-source/concept-map.json");
const FUNCTIONS: &str = include_str!("../../teaching-source/concepts/functions.md");
const API: &str = include_str!("../../teaching-source/concepts/api.md");
const ASYNC_AWAIT: &str = include_str!("../../teaching-source/concepts/async-await.md");

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TeachingLevel {
    Beginner,
    Intermediate,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeachChangeRequest {
    pub level: TeachingLevel,
    pub selected_path: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TeachingAnswer {
    pub explanation: String,
    pub level: TeachingLevel,
    pub generation: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TeachingStateSnapshot {
    pub status: String,
    pub answer: Option<TeachingAnswer>,
    pub error: Option<String>,
}

impl Default for TeachingStateSnapshot {
    fn default() -> Self {
        Self {
            status: "idle".to_string(),
            answer: None,
            error: None,
        }
    }
}

struct Inflight {
    id: u64,
    cancel: Sender<()>,
    flag: Arc<AtomicBool>,
}
struct TeachingRuntime {
    state: TeachingStateSnapshot,
    next_id: u64,
    inflight: Option<Inflight>,
}
pub struct TeachingAppState {
    runtime: Arc<Mutex<TeachingRuntime>>,
}

impl Default for TeachingAppState {
    fn default() -> Self {
        Self {
            runtime: Arc::new(Mutex::new(TeachingRuntime {
                state: Default::default(),
                next_id: 0,
                inflight: None,
            })),
        }
    }
}

fn lock(state: &TeachingAppState) -> std::sync::MutexGuard<'_, TeachingRuntime> {
    state
        .runtime
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn load_teaching_source() -> Result<String, String> {
    let map: serde_json::Value = serde_json::from_str(CONCEPT_MAP)
        .map_err(|error| format!("Teaching Source concept map is invalid: {error}"))?;
    let concepts = map
        .get("concepts")
        .and_then(|value| value.as_array())
        .ok_or_else(|| "Teaching Source concept map has no concepts".to_string())?;
    if concepts.len() != 3
        || [FUNCTIONS, API, ASYNC_AWAIT]
            .iter()
            .any(|text| text.trim().is_empty())
        || GUIDE.trim().is_empty()
    {
        return Err("Teaching Source is incomplete".to_string());
    }
    Ok(format!(
        "{GUIDE}\n\n{CONCEPT_MAP}\n\n{FUNCTIONS}\n\n{API}\n\n{ASYNC_AWAIT}"
    ))
}

pub fn build_teaching_prompt(
    level: &TeachingLevel,
    context: &MentorContext,
    selected_path: Option<&str>,
) -> Result<String, String> {
    build_teaching_prompt_with_memory(level, context, selected_path, &[])
}

fn level_depth_rules(level: &TeachingLevel) -> &'static str {
    match level {
        TeachingLevel::Beginner => {
            "Proceed step by step. Introduce necessary concepts, explain both why and how, provide a reading order, connect abstract ideas to this real code, and avoid introducing too many concepts at once."
        }
        TeachingLevel::Intermediate => {
            "Do not repeat basic syntax unnecessarily. Focus on architecture, data flow, abstraction, design decisions, dependencies, testing, and software-engineering concepts."
        }
    }
}

fn memory_depth_rules(level: &TeachingLevel) -> &'static str {
    match level {
        TeachingLevel::Beginner => {
            "For a New or unrecorded concept, give the full step-by-step introduction. For Learning, briefly reinforce the core idea before focusing on its flow in this change. For Familiar, give a concise refresher and spend more space on how this change connects the concept to the other code."
        }
        TeachingLevel::Intermediate => {
            "For a New or unrecorded concept, define only the essentials before analyzing the design. For Learning, emphasize implementation choices, dependencies, testing, and tradeoffs. For Familiar, assume the foundation and focus on code-specific implications, abstractions, alternatives, and testing."
        }
    }
}

pub fn build_teaching_prompt_with_memory(
    level: &TeachingLevel,
    context: &MentorContext,
    selected_path: Option<&str>,
    memory: &[LearningMemoryRecord],
) -> Result<String, String> {
    let source = load_teaching_source()?;
    let selected = selected_path.and_then(|path| {
        context
            .analysis
            .frozen_files
            .iter()
            .find(|file| file.path == path)
    });
    if let Some(path) = selected_path {
        if path.trim().is_empty() || selected.is_none() {
            return Err("The selected file is not part of the frozen current change".to_string());
        }
    }
    let evidence = json!({ "changeRecord": &context.analysis.record, "changedFiles": &context.analysis.frozen_files, "selectedFrozenFile": selected });
    let level_name = match level {
        TeachingLevel::Beginner => "beginner",
        TeachingLevel::Intermediate => "intermediate",
    };
    let memory_json = serde_json::to_string(memory).map_err(|error| error.to_string())?;
    Ok(format!("You are Teaching Mode in Codex Mentor. Produce exactly one {level_name} explanation of this current change. Use only the supplied Change Record, frozen changed-file evidence, Teaching Source, and matching Learning Memory. Do not scan the repository, inspect files, use tools, browse, or generate a second explanation. Return only the requested explanation in clear prose.\n\nSelected level: {level_name}\n\nLevel depth rules:\n{}\n\nLearning Memory depth adjustment:\n{}\nUse status as user-controlled context for explanation depth. Encounter count and recency are context only; never infer mastery from them. A concept missing from the memory list is unrecorded/New for depth guidance only.\n\nTeaching Source:\n{source}\n\nMatching Learning Memory (only concepts from this current Change Record):\n{memory_json}\n\nFrozen change evidence (JSON):\n{}", level_depth_rules(level), memory_depth_rules(level), serde_json::to_string(&evidence).map_err(|error| error.to_string())?))
}

fn emit(app: &AppHandle, state: &TeachingStateSnapshot) {
    let _ = app.emit(TEACHING_STATE_EVENT, state.clone());
}

#[tauri::command]
pub fn get_teaching_state(state: State<'_, TeachingAppState>) -> TeachingStateSnapshot {
    lock(&state).state.clone()
}

#[tauri::command]
pub fn teach_change(
    app: AppHandle,
    watcher_state: State<'_, WatcherAppState>,
    teaching_state: State<'_, TeachingAppState>,
    learning_memory_state: State<'_, LearningMemoryAppState>,
    request: TeachChangeRequest,
) -> Result<TeachingStateSnapshot, String> {
    let context = watcher::capture_mentor_context(&watcher_state)?;
    let selected_path = request.selected_path.filter(|path| !path.trim().is_empty());
    let concepts =
        learning_memory::normalize_concepts(&context.analysis.record.programming_concepts);
    let relevant_memory = learning_memory::refresh_relevant(
        &app,
        &learning_memory_state,
        &concepts,
        context.generation,
    )?;
    let prompt = build_teaching_prompt_with_memory(
        &request.level,
        &context,
        selected_path.as_deref(),
        &relevant_memory,
    )?;
    let (id, receiver, flag, current) = {
        let mut runtime = lock(&teaching_state);
        if runtime.inflight.is_some() {
            return Err("Teaching explanation already in progress".to_string());
        }
        runtime.next_id = runtime.next_id.wrapping_add(1);
        let (sender, receiver) = mpsc::channel();
        let flag = Arc::new(AtomicBool::new(false));
        let current = TeachingStateSnapshot {
            status: "loading".to_string(),
            answer: None,
            error: None,
        };
        runtime.inflight = Some(Inflight {
            id: runtime.next_id,
            cancel: sender,
            flag: flag.clone(),
        });
        runtime.state = current.clone();
        (runtime.next_id, receiver, flag, current)
    };
    emit(&app, &current);
    let runtime = teaching_state.runtime.clone();
    let watcher_snapshot = (*watcher_state).clone();
    let learning_memory_state = (*learning_memory_state).clone();
    let concepts_for_record = concepts.clone();
    let project_path = context.project_path.clone();
    let level = request.level;
    thread::spawn(move || {
        let result =
            mentor::run_prompt_request_with_flag(context.clone(), prompt, receiver, flag.clone());
        let successful = result.is_ok();
        let next = match result {
            Ok(explanation) => TeachingStateSnapshot {
                status: "available".to_string(),
                answer: Some(TeachingAnswer {
                    explanation,
                    level,
                    generation: context.generation,
                }),
                error: None,
            },
            Err(error) => TeachingStateSnapshot {
                status: "error".to_string(),
                answer: None,
                error: Some(error),
            },
        };
        let published = watcher::publish_mentor_if_current(&watcher_snapshot, &context, || {
            let mut state = runtime
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.inflight.as_ref().map(|request| request.id) != Some(id) {
                return false;
            }
            if successful {
                let _ = learning_memory::record_successful_teaching(
                    &app,
                    &learning_memory_state,
                    &project_path,
                    context.generation,
                    &concepts_for_record,
                );
            }
            state.inflight = None;
            state.state = next.clone();
            drop(state);
            emit(&app, &next);
            true
        });
        if published != Some(true) {
            let mut state = runtime
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.inflight.as_ref().map(|request| request.id) == Some(id) {
                state.inflight = None;
            }
        }
    });
    Ok(current)
}

#[tauri::command]
pub fn reset_teaching(app: AppHandle, state: State<'_, TeachingAppState>) -> TeachingStateSnapshot {
    let (inflight, current) = {
        let mut runtime = lock(&state);
        let inflight = runtime.inflight.take();
        runtime.state = Default::default();
        (inflight, runtime.state.clone())
    };
    if let Some(request) = inflight {
        request.flag.store(true, Ordering::Release);
        let _ = request.cancel.send(());
    }
    emit(&app, &current);
    current
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{AnalysisMetadata, ChangeAnalysis, ChangeRecord, ScopedFileContext};
    use crate::diff::ContentStatus;
    use crate::watcher::FileChangeStatus;

    fn context() -> MentorContext {
        MentorContext {
            project_path: "C:/project".to_string(),
            generation: 4,
            analysis: ChangeAnalysis {
                record: ChangeRecord {
                    summary: "A function changed".to_string(),
                    purpose: "Teach the flow".to_string(),
                    changed_components: vec!["src/lib.rs".to_string()],
                    key_decisions: vec![],
                    how_it_works: "The function returns a value.".to_string(),
                    impact: "Local".to_string(),
                    risk: "Low".to_string(),
                    review_priority: "Normal".to_string(),
                    programming_concepts: vec!["functions".to_string()],
                    relevant_code_locations: vec!["src/lib.rs".to_string()],
                },
                metadata: AnalysisMetadata {
                    project_path: "C:/project".to_string(),
                    source: "snapshot".to_string(),
                    completion: "complete".to_string(),
                    completion_generation: 4,
                    changed_file_count: 1,
                    supplied: Default::default(),
                },
                frozen_files: vec![ScopedFileContext {
                    path: "src/lib.rs".to_string(),
                    status: FileChangeStatus::Modified,
                    content_status: ContentStatus::Text,
                    before: Some("old".to_string()),
                    after: Some("new".to_string()),
                }],
            },
        }
    }

    #[test]
    fn source_loads_and_validates() {
        assert!(load_teaching_source().unwrap().contains("Teaching rules"));
    }
    #[test]
    fn level_is_explicit_and_prompt_is_scoped() {
        assert!(serde_json::from_str::<TeachChangeRequest>(r#"{"level":"beginner"}"#).is_ok());
        assert!(serde_json::from_str::<TeachChangeRequest>(r#"{}"#).is_err());
        let context = context();
        let prompt =
            build_teaching_prompt(&TeachingLevel::Intermediate, &context, Some("src/lib.rs"))
                .unwrap();
        assert!(prompt.contains("Selected level: intermediate"));
        assert!(prompt.contains("src/lib.rs"));
        assert!(
            build_teaching_prompt(&TeachingLevel::Beginner, &context, Some("other.rs")).is_err()
        );
    }

    #[test]
    fn memory_and_only_requested_level_rules_are_in_prompt() {
        let context = context();
        let memory = vec![LearningMemoryRecord {
            concept: "functions".to_string(),
            times_encountered: 2,
            status: learning_memory::LearningStatus::Learning,
            last_encountered: "2026-08-14T01:02:03.000Z".to_string(),
            projects_encountered: vec!["C:/project".to_string()],
        }];
        let prompt =
            build_teaching_prompt_with_memory(&TeachingLevel::Beginner, &context, None, &memory)
                .unwrap();
        assert!(prompt.contains("status as user-controlled"));
        assert!(prompt.contains("\"timesEncountered\":2"));
        assert!(prompt.contains("Proceed step by step"));
        assert!(!prompt.contains("Focus on architecture, data flow, abstraction"));
    }
}

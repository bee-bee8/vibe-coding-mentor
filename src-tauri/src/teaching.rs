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
pub enum TeachingLevel { Beginner, Intermediate }

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeachChangeRequest { pub level: TeachingLevel, pub selected_path: Option<String> }

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TeachingAnswer { pub explanation: String, pub level: TeachingLevel, pub generation: u64 }

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TeachingStateSnapshot {
    pub status: String,
    pub answer: Option<TeachingAnswer>,
    pub error: Option<String>,
}

impl Default for TeachingStateSnapshot {
    fn default() -> Self { Self { status: "idle".to_string(), answer: None, error: None } }
}

struct Inflight { id: u64, cancel: Sender<()>, flag: Arc<AtomicBool> }
struct TeachingRuntime { state: TeachingStateSnapshot, next_id: u64, inflight: Option<Inflight> }
pub struct TeachingAppState { runtime: Arc<Mutex<TeachingRuntime>> }

impl Default for TeachingAppState {
    fn default() -> Self { Self { runtime: Arc::new(Mutex::new(TeachingRuntime { state: Default::default(), next_id: 0, inflight: None })) } }
}

fn lock(state: &TeachingAppState) -> std::sync::MutexGuard<'_, TeachingRuntime> {
    state.runtime.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn load_teaching_source() -> Result<String, String> {
    let map: serde_json::Value = serde_json::from_str(CONCEPT_MAP)
        .map_err(|error| format!("Teaching Source concept map is invalid: {error}"))?;
    let concepts = map.get("concepts").and_then(|value| value.as_array())
        .ok_or_else(|| "Teaching Source concept map has no concepts".to_string())?;
    if concepts.len() != 3 || [FUNCTIONS, API, ASYNC_AWAIT].iter().any(|text| text.trim().is_empty()) || GUIDE.trim().is_empty() {
        return Err("Teaching Source is incomplete".to_string());
    }
    Ok(format!("{GUIDE}\n\n{CONCEPT_MAP}\n\n{FUNCTIONS}\n\n{API}\n\n{ASYNC_AWAIT}"))
}

pub fn build_teaching_prompt(
    level: &TeachingLevel,
    context: &MentorContext,
    selected_path: Option<&str>,
) -> Result<String, String> {
    let source = load_teaching_source()?;
    let selected = selected_path.and_then(|path| context.analysis.frozen_files.iter().find(|file| file.path == path));
    if let Some(path) = selected_path {
        if path.trim().is_empty() || selected.is_none() { return Err("The selected file is not part of the frozen current change".to_string()); }
    }
    let evidence = json!({ "changeRecord": &context.analysis.record, "changedFiles": &context.analysis.frozen_files, "selectedFrozenFile": selected });
    let level_name = match level { TeachingLevel::Beginner => "beginner", TeachingLevel::Intermediate => "intermediate" };
    Ok(format!("You are Teaching Mode in Codex Mentor. Produce exactly one {level_name} explanation of this current change. Use only the supplied Change Record, frozen changed-file evidence, and Teaching Source. Do not scan the repository, inspect files, use tools, browse, or generate the other level. Return only the explanation in clear prose.\n\nSelected level: {level_name}\n\nTeaching Source:\n{source}\n\nFrozen change evidence (JSON):\n{}", serde_json::to_string(&evidence).map_err(|error| error.to_string())?))
}

fn emit(app: &AppHandle, state: &TeachingStateSnapshot) { let _ = app.emit(TEACHING_STATE_EVENT, state.clone()); }

#[tauri::command]
pub fn get_teaching_state(state: State<'_, TeachingAppState>) -> TeachingStateSnapshot { lock(&state).state.clone() }

#[tauri::command]
pub fn teach_change(app: AppHandle, watcher_state: State<'_, WatcherAppState>, teaching_state: State<'_, TeachingAppState>, request: TeachChangeRequest) -> Result<TeachingStateSnapshot, String> {
    let context = watcher::capture_mentor_context(&watcher_state)?;
    let selected_path = request.selected_path.filter(|path| !path.trim().is_empty());
    let prompt = build_teaching_prompt(&request.level, &context, selected_path.as_deref())?;
    let (id, receiver, flag, current) = {
        let mut runtime = lock(&teaching_state);
        if runtime.inflight.is_some() { return Err("Teaching explanation already in progress".to_string()); }
        runtime.next_id = runtime.next_id.wrapping_add(1);
        let (sender, receiver) = mpsc::channel(); let flag = Arc::new(AtomicBool::new(false));
        let current = TeachingStateSnapshot { status: "loading".to_string(), answer: None, error: None };
        runtime.inflight = Some(Inflight { id: runtime.next_id, cancel: sender, flag: flag.clone() }); runtime.state = current.clone();
        (runtime.next_id, receiver, flag, current)
    };
    emit(&app, &current);
    let runtime = teaching_state.runtime.clone(); let watcher_snapshot = (*watcher_state).clone(); let level = request.level;
    thread::spawn(move || {
        let result = mentor::run_prompt_request_with_flag(context.clone(), prompt, receiver, flag.clone());
        let next = match result { Ok(explanation) => TeachingStateSnapshot { status: "available".to_string(), answer: Some(TeachingAnswer { explanation, level, generation: context.generation }), error: None }, Err(error) => TeachingStateSnapshot { status: "error".to_string(), answer: None, error: Some(error) } };
        if watcher::publish_mentor_if_current(&watcher_snapshot, &context, || {
            let mut state = runtime.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.inflight.as_ref().map(|request| request.id) != Some(id) { return; }
            state.inflight = None; state.state = next.clone(); drop(state); emit(&app, &next);
        }).is_none() {
            let mut state = runtime.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.inflight.as_ref().map(|request| request.id) == Some(id) { state.inflight = None; }
        }
    });
    Ok(current)
}

#[tauri::command]
pub fn reset_teaching(app: AppHandle, state: State<'_, TeachingAppState>) -> TeachingStateSnapshot {
    let (inflight, current) = { let mut runtime = lock(&state); let inflight = runtime.inflight.take(); runtime.state = Default::default(); (inflight, runtime.state.clone()) };
    if let Some(request) = inflight { request.flag.store(true, Ordering::Release); let _ = request.cancel.send(()); }
    emit(&app, &current); current
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{AnalysisMetadata, ChangeAnalysis, ChangeRecord, ScopedFileContext};
    use crate::diff::ContentStatus;
    use crate::watcher::FileChangeStatus;

    fn context() -> MentorContext {
        MentorContext { project_path: "C:/project".to_string(), generation: 4, analysis: ChangeAnalysis {
            record: ChangeRecord { summary: "A function changed".to_string(), purpose: "Teach the flow".to_string(), changed_components: vec!["src/lib.rs".to_string()], key_decisions: vec![], how_it_works: "The function returns a value.".to_string(), impact: "Local".to_string(), risk: "Low".to_string(), review_priority: "Normal".to_string(), programming_concepts: vec!["functions".to_string()], relevant_code_locations: vec!["src/lib.rs".to_string()] },
            metadata: AnalysisMetadata { project_path: "C:/project".to_string(), source: "snapshot".to_string(), completion: "complete".to_string(), completion_generation: 4, changed_file_count: 1, supplied: Default::default() },
            frozen_files: vec![ScopedFileContext { path: "src/lib.rs".to_string(), status: FileChangeStatus::Modified, content_status: ContentStatus::Text, before: Some("old".to_string()), after: Some("new".to_string()) }],
        }}
    }

    #[test] fn source_loads_and_validates() { assert!(load_teaching_source().unwrap().contains("Teaching rules")); }
    #[test] fn level_is_explicit_and_prompt_is_scoped() {
        assert!(serde_json::from_str::<TeachChangeRequest>(r#"{"level":"beginner"}"#).is_ok());
        assert!(serde_json::from_str::<TeachChangeRequest>(r#"{}"#).is_err());
        let context = context();
        let prompt = build_teaching_prompt(&TeachingLevel::Intermediate, &context, Some("src/lib.rs")).unwrap();
        assert!(prompt.contains("Selected level: intermediate"));
        assert!(prompt.contains("src/lib.rs"));
        assert!(build_teaching_prompt(&TeachingLevel::Beginner, &context, Some("other.rs")).is_err());
    }
}

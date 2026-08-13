use crate::analysis::{ChangeAnalysis, ScopedFileContext};
use crate::watcher::{self, AppState as WatcherAppState, MentorContext};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::env;
use std::io::{self, BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, State};

pub const MENTOR_STATE_EVENT: &str = "mentor-state";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
const RECEIVE_SLICE: Duration = Duration::from_millis(100);
const SEND_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MentorStatus {
    Idle,
    Loading,
    Available,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MentorAnswer {
    pub answer: String,
    pub question: String,
    pub selected_path: Option<String>,
    pub generation: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MentorStateSnapshot {
    pub status: MentorStatus,
    pub answer: Option<MentorAnswer>,
    pub question: Option<String>,
    pub selected_path: Option<String>,
    pub error: Option<String>,
}

impl Default for MentorStateSnapshot {
    fn default() -> Self {
        Self {
            status: MentorStatus::Idle,
            answer: None,
            question: None,
            selected_path: None,
            error: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskMentorRequest {
    pub question: String,
    pub selected_path: Option<String>,
}

struct MentorRuntime {
    state: MentorStateSnapshot,
    next_id: u64,
    inflight: Option<InflightRequest>,
}

struct InflightRequest {
    id: u64,
    cancel: Sender<()>,
    cancel_flag: Arc<AtomicBool>,
}

pub struct MentorAppState {
    runtime: Arc<Mutex<MentorRuntime>>,
}

impl Default for MentorAppState {
    fn default() -> Self {
        Self {
            runtime: Arc::new(Mutex::new(MentorRuntime {
                state: MentorStateSnapshot::default(),
                next_id: 0,
                inflight: None,
            })),
        }
    }
}

fn lock_runtime(state: &MentorAppState) -> std::sync::MutexGuard<'_, MentorRuntime> {
    state
        .runtime
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn emit_state(app: &AppHandle, state: &MentorStateSnapshot) {
    let _ = app.emit(MENTOR_STATE_EVENT, state.clone());
}

fn set_state(
    app: &AppHandle,
    state: &Arc<Mutex<MentorRuntime>>,
    request_id: u64,
    next: MentorStateSnapshot,
) -> bool {
    let published = {
        let mut runtime = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if runtime.inflight.as_ref().map(|request| request.id) != Some(request_id) {
            return false;
        }
        runtime.inflight = None;
        runtime.state = next;
        runtime.state.clone()
    };
    // Do not call into Tauri while holding the Mentor runtime mutex.  Event
    // handlers may synchronously re-enter commands that need this state lock.
    emit_state(app, &published);
    true
}

fn clear_request(state: &Arc<Mutex<MentorRuntime>>, request_id: u64) -> bool {
    let mut runtime = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if runtime.inflight.as_ref().map(|request| request.id) != Some(request_id) {
        return false;
    }
    runtime.inflight = None;
    true
}

fn error_state(
    question: Option<String>,
    selected_path: Option<String>,
    message: impl Into<String>,
) -> MentorStateSnapshot {
    MentorStateSnapshot {
        status: MentorStatus::Error,
        answer: None,
        question,
        selected_path,
        error: Some(message.into()),
    }
}

/// Resolve the executable without introducing a shell.  The override is a
/// path/name for a Codex-compatible executable; the app-server subcommand is
/// always appended as required by the official CLI contract.
pub fn app_server_command() -> (String, Vec<String>) {
    let executable = env::var("CODEX_APP_SERVER_EXECUTABLE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "codex".to_string());
    (executable, vec!["app-server".to_string()])
}

fn read_only_sandbox() -> Value {
    json!({
        "type": "readOnly",
        "networkAccess": false
    })
}

fn output_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "answer": { "type": "string" } },
        "required": ["answer"],
        "additionalProperties": false
    })
}

fn compact_file_context(file: &ScopedFileContext, selected: bool) -> Value {
    if selected {
        json!({
            "path": file.path,
            "status": file.status,
            "contentStatus": file.content_status,
            "before": file.before,
            "after": file.after
        })
    } else {
        json!({
            "path": file.path,
            "status": file.status,
            "contentStatus": file.content_status,
            "before": file.before.as_deref().map(compact_text),
            "after": file.after.as_deref().map(compact_text)
        })
    }
}

fn compact_text(text: &str) -> String {
    const MAX_CHARS: usize = 4_000;
    if text.chars().count() <= MAX_CHARS {
        return text.to_string();
    }
    let prefix = text.chars().take(MAX_CHARS).collect::<String>();
    format!("{prefix}\n[truncated by Mentor; supplied evidence continues]")
}

/// Construct a prompt from only the frozen Change Record and frozen changed
/// files.  This function never reads the worktree or performs a repository
/// scan, which keeps the app-server call inside the Change Analysis boundary.
pub fn build_prompt(
    question: &str,
    analysis: &ChangeAnalysis,
    selected_path: Option<&str>,
) -> Result<String, String> {
    let selected =
        selected_path.and_then(|path| analysis.frozen_files.iter().find(|file| file.path == path));
    if let Some(path) = selected_path {
        if selected.is_none() {
            return Err("The selected file is not part of the frozen current change".to_string());
        }
        if path.trim().is_empty() {
            return Err("The selected file path cannot be empty".to_string());
        }
    }

    let files = analysis
        .frozen_files
        .iter()
        .map(|file| compact_file_context(file, false))
        .collect::<Vec<_>>();
    let selected_file = selected.map(|file| compact_file_context(file, true));
    let record = serde_json::to_value(&analysis.record)
        .map_err(|error| format!("Unable to serialize the current Change Record: {error}"))?;
    let context = json!({
        "changeRecord": record,
        "changedFiles": files,
        "selectedFrozenFile": selected_file
    });

    Ok(format!(
        "You are Ask Mentor in Codex Mentor. Answer the user's question using only the supplied current-change evidence below. Do not inspect the project, read files, run commands, use tools, browse the web, call MCP, ask for approval, or infer hidden reasoning. If the evidence is insufficient, say what is missing instead of guessing. Answer only the question in concise technical prose; do not include chain-of-thought.\n\nQuestion:\n{}\n\nSupplied current-change evidence (JSON):\n{}",
        question.trim(),
        serde_json::to_string(&context)
            .map_err(|error| format!("Unable to serialize Ask Mentor context: {error}"))?
    ))
}

fn structured_answer(value: &Value) -> Result<String, String> {
    let answer = value
        .get("answer")
        .and_then(Value::as_str)
        .ok_or_else(|| "Codex returned a malformed structured answer".to_string())?
        .trim();
    if answer.is_empty() {
        return Err("Codex returned an empty Ask Mentor answer".to_string());
    }
    Ok(answer.to_string())
}

fn parse_structured_text(text: &str) -> Result<String, String> {
    let value: Value = serde_json::from_str(text.trim())
        .map_err(|_| "Codex returned a malformed structured answer".to_string())?;
    structured_answer(&value)
}

fn message_id(value: &Value) -> Option<u64> {
    value.get("id").and_then(Value::as_u64)
}

fn response_error(value: &Value) -> Option<String> {
    value
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn result_object<'a>(value: &'a Value, id: u64, method: &str) -> Result<&'a Value, String> {
    if message_id(value) != Some(id) {
        return Err(format!(
            "Codex returned an unexpected response while calling {method}"
        ));
    }
    if let Some(error) = response_error(value) {
        return Err(format!("Codex {method} failed: {error}"));
    }
    value
        .get("result")
        .ok_or_else(|| format!("Codex returned a malformed {method} response"))
}

fn thread_id(value: &Value) -> Result<String, String> {
    value
        .get("thread")
        .and_then(|thread| thread.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| "Codex returned a malformed thread/start response".to_string())
}

fn turn_id(value: &Value) -> Result<String, String> {
    value
        .get("turn")
        .and_then(|turn| turn.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| "Codex returned a malformed turn/start response".to_string())
}

fn value_string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
}

fn event_thread_id(value: &Value) -> Option<&str> {
    let params = value.get("params")?;
    value_string(params, "threadId")
        .or_else(|| {
            params
                .get("thread")
                .and_then(|thread| value_string(thread, "id"))
        })
        .or_else(|| {
            params
                .get("item")
                .and_then(|item| value_string(item, "threadId"))
        })
}

fn event_turn_id(value: &Value) -> Option<&str> {
    let params = value.get("params")?;
    value_string(params, "turnId")
        .or_else(|| params.get("turn").and_then(|turn| value_string(turn, "id")))
        .or_else(|| {
            params
                .get("item")
                .and_then(|item| value_string(item, "turnId"))
        })
}

fn lifecycle_item_id(value: &Value) -> Option<&str> {
    value
        .get("params")?
        .get("item")
        .and_then(|item| value_string(item, "id"))
}

fn delta_item_id(value: &Value) -> Option<&str> {
    value
        .get("params")
        .and_then(|params| value_string(params, "itemId"))
}

fn turn_event_matches(value: &Value, thread_id: &str, turn_id: &str) -> bool {
    let Some(_) = value.get("params") else {
        return false;
    };
    let thread_matches = match event_thread_id(value) {
        Some(event_thread_id) => event_thread_id == thread_id,
        None => value.get("method").and_then(Value::as_str) == Some("turn/completed"),
    };

    thread_matches && event_turn_id(value) == Some(turn_id)
}

fn disallowed_method(method: &str) -> bool {
    let method = method.to_ascii_lowercase();
    [
        "approval",
        "requestuserinput",
        "mcp",
        "tool/",
        "command",
        "file",
        "web",
        "process",
        "shell",
    ]
    .iter()
    .any(|needle| method.contains(needle))
}

fn disallowed_item(item: &Value) -> bool {
    matches!(
        item.get("type").and_then(Value::as_str),
        Some(
            "commandExecution"
                | "fileChange"
                | "mcpToolCall"
                | "dynamicToolCall"
                | "collabToolCall"
                | "webSearch"
                | "imageView"
        )
    )
}

fn reject_server_request<T: ProtocolTransport>(transport: &mut T, value: &Value, method: &str) {
    if let Some(id) = value.get("id") {
        let _ = transport.send(json!({
            "id": id,
            "error": { "code": -32601, "message": "Ask Mentor disallows tools and approvals" }
        }));
    }
    let _ = method;
}

fn delete_thread<T: ProtocolTransport>(transport: &mut T, thread_id: &str) {
    // `thread/delete` is a documented app-server method.  It is best effort:
    // process cleanup below remains authoritative if the server is already
    // shutting down or rejects deletion for an ephemeral thread.
    let _ = transport.send(json!({
        "method": "thread/delete",
        "id": 4,
        "params": { "threadId": thread_id }
    }));
}

pub trait ProtocolTransport {
    fn send(&mut self, message: Value) -> Result<(), String>;
    fn recv(&mut self, timeout: Duration) -> Result<Option<Value>, String>;
    fn close(&mut self);
}

#[derive(Debug, PartialEq, Eq)]
enum ProtocolFailure {
    Error(String),
    Cancelled,
}

fn recv_message<T: ProtocolTransport>(
    transport: &mut T,
    deadline: Instant,
    cancel: &Receiver<()>,
    thread_id: Option<&str>,
    turn_id: Option<&str>,
) -> Result<Value, ProtocolFailure> {
    loop {
        if cancel.try_recv().is_ok() {
            if let (Some(thread_id), Some(turn_id)) = (thread_id, turn_id) {
                let _ = transport.send(json!({
                    "method": "turn/interrupt",
                    "id": 90,
                    "params": { "threadId": thread_id, "turnId": turn_id }
                }));
            }
            return Err(ProtocolFailure::Cancelled);
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(ProtocolFailure::Error(TIMEOUT_ERROR.to_string()));
        }
        let remaining = deadline.saturating_duration_since(now).min(RECEIVE_SLICE);
        match transport.recv(remaining) {
            Ok(Some(value)) => return Ok(value),
            Ok(None) => continue,
            Err(error) => return Err(ProtocolFailure::Error(error)),
        }
    }
}

fn recv_response<T: ProtocolTransport>(
    transport: &mut T,
    deadline: Instant,
    cancel: &Receiver<()>,
    id: u64,
    method: &str,
    thread_id: Option<&str>,
    turn_id: Option<&str>,
) -> Result<Value, ProtocolFailure> {
    loop {
        let value = recv_message(transport, deadline, cancel, thread_id, turn_id)?;
        if let Some(notification) = value.get("method").and_then(Value::as_str) {
            if value.get("id").is_some() {
                reject_server_request(transport, &value, notification);
                return Err(ProtocolFailure::Error(format!(
                    "Ask Mentor rejected unsupported Codex request: {notification}"
                )));
            }
            if disallowed_method(notification) {
                reject_server_request(transport, &value, notification);
                return Err(ProtocolFailure::Error(format!(
                    "Ask Mentor rejected unsupported Codex request: {notification}"
                )));
            }
            if matches!(notification, "item/started" | "item/completed")
                && value
                    .get("params")
                    .and_then(|params| params.get("item"))
                    .map(disallowed_item)
                    .unwrap_or(false)
            {
                return Err(ProtocolFailure::Error(
                    "Ask Mentor rejected an unsupported Codex tool item".to_string(),
                ));
            }
            continue;
        }
        if message_id(&value) == Some(id) {
            return Ok(value);
        }
        return Err(ProtocolFailure::Error(format!(
            "Codex returned an unexpected response while calling {method}"
        )));
    }
}

fn run_protocol_without_cleanup<T: ProtocolTransport>(
    transport: &mut T,
    question: &str,
    context: &MentorContext,
    selected_path: Option<&str>,
    cancel: &Receiver<()>,
    timeout: Duration,
) -> (Result<String, ProtocolFailure>, Option<String>) {
    let mut created_thread: Option<String> = None;
    let result = (|| {
        let prompt = build_prompt(question, &context.analysis, selected_path)
            .map_err(ProtocolFailure::Error)?;
        let deadline = Instant::now() + timeout;
        transport
            .send(json!({
                "method": "initialize",
                "id": 1,
                "params": {
                    "clientInfo": {
                        "name": "codex_mentor",
                        "title": "Codex Mentor",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }
            }))
            .map_err(ProtocolFailure::Error)?;
        let initialize = recv_response(transport, deadline, cancel, 1, "initialize", None, None)?;
        result_object(&initialize, 1, "initialize").map_err(ProtocolFailure::Error)?;
        transport
            .send(json!({ "method": "initialized", "params": {} }))
            .map_err(ProtocolFailure::Error)?;
        transport
            .send(json!({
                "method": "thread/start",
                "id": 2,
                "params": {
                    "cwd": context.project_path.clone(),
                    "approvalPolicy": "never",
                    "sandbox": "read-only"
                }
            }))
            .map_err(ProtocolFailure::Error)?;
        let thread_response =
            recv_response(transport, deadline, cancel, 2, "thread/start", None, None)?;
        let thread = thread_id(
            result_object(&thread_response, 2, "thread/start").map_err(ProtocolFailure::Error)?,
        )
        .map_err(ProtocolFailure::Error)?;
        created_thread = Some(thread.clone());
        transport
            .send(json!({
                "method": "turn/start",
                "id": 3,
                "params": {
                    "threadId": thread.clone(),
                    "input": [{ "type": "text", "text": prompt }],
                    "cwd": context.project_path.clone(),
                    "approvalPolicy": "never",
                    "sandboxPolicy": read_only_sandbox(),
                    "outputSchema": output_schema()
                }
            }))
            .map_err(ProtocolFailure::Error)?;
        let turn_response = recv_response(
            transport,
            deadline,
            cancel,
            3,
            "turn/start",
            Some(&thread),
            None,
        )?;
        let turn = turn_id(
            result_object(&turn_response, 3, "turn/start").map_err(ProtocolFailure::Error)?,
        )
        .map_err(ProtocolFailure::Error)?;

        let mut streamed = String::new();
        let mut completed_text: Option<String> = None;
        let mut agent_item_id: Option<String> = None;
        loop {
            let value = recv_message(transport, deadline, cancel, Some(&thread), Some(&turn))?;
            if let Some(method) = value.get("method").and_then(Value::as_str) {
                if value.get("id").is_some() {
                    reject_server_request(transport, &value, method);
                    return Err(ProtocolFailure::Error(format!(
                        "Ask Mentor rejected unsupported Codex request: {method}"
                    )));
                }
                if disallowed_method(method) {
                    reject_server_request(transport, &value, method);
                    return Err(ProtocolFailure::Error(format!(
                        "Ask Mentor rejected unsupported Codex request: {method}"
                    )));
                }
                match method {
                    "item/agentMessage/delta" => {
                        let Some(expected_item_id) = agent_item_id.as_deref() else {
                            continue;
                        };
                        let Some(item_id) = delta_item_id(&value) else {
                            continue;
                        };
                        if item_id != expected_item_id {
                            continue;
                        }
                        let params = value.get("params").unwrap_or(&Value::Null);
                        if let Some(delta) = params
                            .get("delta")
                            .and_then(Value::as_str)
                            .or_else(|| params.get("text").and_then(Value::as_str))
                        {
                            streamed.push_str(delta);
                        }
                    }
                    "item/completed" => {
                        let item = value
                            .get("params")
                            .and_then(|params| params.get("item"))
                            .ok_or_else(|| {
                                ProtocolFailure::Error(
                                    "Codex returned a malformed item/completed notification"
                                    .to_string(),
                                )
                            })?;
                        let Some(item_id) = lifecycle_item_id(&value) else {
                            continue;
                        };
                        if let Some(expected_item_id) = agent_item_id.as_deref() {
                            if item_id != expected_item_id {
                                continue;
                            }
                        }
                        if item.get("type").and_then(Value::as_str) == Some("agentMessage") {
                            if agent_item_id.is_none() {
                                agent_item_id = Some(item_id.to_string());
                            }
                            completed_text =
                                item.get("text").and_then(Value::as_str).map(str::to_string);
                        } else if disallowed_item(item) {
                            return Err(ProtocolFailure::Error(
                                "Ask Mentor rejected an unsupported Codex tool item".to_string(),
                            ));
                        }
                    }
                    "item/started" => {
                        let item = value
                            .get("params")
                            .and_then(|params| params.get("item"))
                            .ok_or_else(|| {
                                ProtocolFailure::Error(
                                    "Codex returned a malformed item/started notification"
                                    .to_string(),
                                )
                            })?;
                        let Some(item_id) = lifecycle_item_id(&value) else {
                            continue;
                        };
                        if let Some(expected_item_id) = agent_item_id.as_deref() {
                            if item_id != expected_item_id {
                                continue;
                            }
                        }
                        if disallowed_item(item) {
                            return Err(ProtocolFailure::Error(
                                "Ask Mentor rejected an unsupported Codex tool item".to_string(),
                            ));
                        }
                        if item.get("type").and_then(Value::as_str) == Some("agentMessage") {
                            agent_item_id = Some(item_id.to_string());
                        }
                    }
                    "turn/completed" => {
                        if !turn_event_matches(&value, &thread, &turn) {
                            continue;
                        }
                        let turn_status = value
                            .get("params")
                            .and_then(|params| params.get("turn"))
                            .and_then(|turn| turn.get("status"))
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if turn_status == "interrupted" {
                            return Err(ProtocolFailure::Cancelled);
                        }
                        if turn_status != "completed" {
                            let detail = value
                                .get("params")
                                .and_then(|params| params.get("turn"))
                                .and_then(|turn| turn.get("error"))
                                .and_then(|error| error.get("message"))
                                .and_then(Value::as_str)
                                .unwrap_or("Codex did not complete the Ask Mentor turn");
                            return Err(ProtocolFailure::Error(detail.to_string()));
                        }
                        let text = completed_text.as_deref().unwrap_or(streamed.as_str());
                        return parse_structured_text(text).map_err(ProtocolFailure::Error);
                    }
                    "error" => {
                        if !turn_event_matches(&value, &thread, &turn) {
                            continue;
                        }
                        let detail = value
                            .get("params")
                            .and_then(|params| params.get("error"))
                            .and_then(|error| error.get("message"))
                            .and_then(Value::as_str)
                            .unwrap_or("Codex reported an Ask Mentor error");
                        return Err(ProtocolFailure::Error(detail.to_string()));
                    }
                    _ => {}
                }
            } else if value.get("id").is_some() {
                // A response for a request we did not issue is malformed and must
                // not be mistaken for an answer.
                return Err(ProtocolFailure::Error(
                    "Codex returned an unexpected response during Ask Mentor".to_string(),
                ));
            }
        }
    })();
    (result, created_thread)
}

fn run_protocol<T: ProtocolTransport>(
    transport: &mut T,
    question: &str,
    context: &MentorContext,
    selected_path: Option<&str>,
    cancel: &Receiver<()>,
    timeout: Duration,
) -> Result<String, ProtocolFailure> {
    let (result, created_thread) =
        run_protocol_without_cleanup(transport, question, context, selected_path, cancel, timeout);
    if let Some(thread) = created_thread.as_deref() {
        delete_thread(transport, thread);
    }
    result
}

struct ProcessTransport {
    child: Arc<Mutex<Child>>,
    stdin: Option<ChildStdin>,
    lines: Receiver<io::Result<String>>,
    reader: Option<JoinHandle<()>>,
}

impl ProcessTransport {
    fn spawn() -> Result<Self, String> {
        let (executable, args) = app_server_command();
        let mut child = Command::new(&executable)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("Unable to start Codex app-server: {error}"))?;
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Codex app-server stdout was not available".to_string());
            }
        };
        let (sender, receiver) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let result = line;
                if sender.send(result).is_err() {
                    break;
                }
            }
        });
        let stdin = child.stdin.take();
        if stdin.is_none() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err("Codex app-server stdin was not available".to_string());
        }
        Ok(Self {
            child: Arc::new(Mutex::new(child)),
            stdin,
            lines: receiver,
            reader: Some(reader),
        })
    }

    fn kill_child(&self) {
        let mut child = self
            .child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = child.kill();
    }
}

impl ProtocolTransport for ProcessTransport {
    fn send(&mut self, message: Value) -> Result<(), String> {
        let Some(stdin) = self.stdin.take() else {
            return Err("Codex app-server stdin is closed".to_string());
        };
        let line = serde_json::to_string(&message)
            .map_err(|error| format!("Unable to encode Codex app-server request: {error}"))?;
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let mut stdin = stdin;
            let result = stdin
                .write_all(line.as_bytes())
                .and_then(|_| stdin.write_all(b"\n"))
                .and_then(|_| stdin.flush())
                .map_err(|error| format!("Unable to write to Codex app-server: {error}"));
            let _ = sender.send((stdin, result));
        });
        match receiver.recv_timeout(SEND_TIMEOUT) {
            Ok((stdin, result)) => {
                self.stdin = Some(stdin);
                result
            }
            Err(RecvTimeoutError::Timeout) => {
                // A server that never reads stdin must not hold cancellation or
                // timeout behind a pipe write.  Killing the child closes the
                // pipe and lets the detached writer unwind.
                self.kill_child();
                Err("Timed out writing to Codex app-server".to_string())
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.kill_child();
                Err("Codex app-server writer stopped unexpectedly".to_string())
            }
        }
    }

    fn recv(&mut self, timeout: Duration) -> Result<Option<Value>, String> {
        match self.lines.recv_timeout(timeout) {
            Ok(Ok(line)) => serde_json::from_str(&line)
                .map(Some)
                .map_err(|error| format!("Codex returned malformed JSONL: {error}")),
            Ok(Err(error)) => Err(format!("Unable to read Codex app-server output: {error}")),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => {
                Err("Codex app-server closed its output before completing Ask Mentor".to_string())
            }
        }
    }

    fn close(&mut self) {
        self.stdin.take();
        self.kill_child();
        let mut child = self
            .child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

struct ProcessControl {
    cancel: Arc<AtomicBool>,
    timed_out: AtomicBool,
    stopped: AtomicBool,
}

const TIMEOUT_ERROR: &str = "Ask Mentor timed out waiting for Codex";

fn finalize_protocol_result(
    result: Result<String, ProtocolFailure>,
    cancel_requested: bool,
    monitor_timed_out: bool,
) -> Result<String, String> {
    if cancel_requested {
        return Err("Ask Mentor was cancelled".to_string());
    }
    match result {
        Ok(answer) => Ok(answer),
        Err(ProtocolFailure::Cancelled) => Err("Ask Mentor was cancelled".to_string()),
        Err(ProtocolFailure::Error(error)) if monitor_timed_out && error == TIMEOUT_ERROR => {
            Err(TIMEOUT_ERROR.to_string())
        }
        Err(ProtocolFailure::Error(error)) => Err(error),
    }
}

fn run_request(
    context: MentorContext,
    question: String,
    selected_path: Option<String>,
    cancel: Receiver<()>,
) -> Result<String, String> {
    run_request_with_flag(
        context,
        question,
        selected_path,
        cancel,
        Arc::new(AtomicBool::new(false)),
    )
}

fn run_request_with_flag(
    context: MentorContext,
    question: String,
    selected_path: Option<String>,
    cancel: Receiver<()>,
    cancel_flag: Arc<AtomicBool>,
) -> Result<String, String> {
    let mut transport = ProcessTransport::spawn()?;
    let control = Arc::new(ProcessControl {
        cancel: cancel_flag,
        timed_out: AtomicBool::new(false),
        stopped: AtomicBool::new(false),
    });
    let monitor_child = transport.child.clone();
    let monitor_control = control.clone();
    let monitor = thread::spawn(move || {
        // `run_protocol_without_cleanup` owns the active-turn deadline.  Keep
        // this process kill guard slightly behind it so the protocol can
        // return its explicit timeout before the child is terminated.
        let deadline = Instant::now() + DEFAULT_TIMEOUT + SEND_TIMEOUT + RECEIVE_SLICE;
        while !monitor_control.stopped.load(Ordering::Acquire) {
            if monitor_control.cancel.load(Ordering::Acquire) {
                let mut child = monitor_child
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let _ = child.kill();
                break;
            }
            if Instant::now() >= deadline {
                monitor_control.timed_out.store(true, Ordering::Release);
                let mut child = monitor_child
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let _ = child.kill();
                break;
            }
            thread::sleep(RECEIVE_SLICE);
        }
    });
    // Keep the active-turn monitor scoped to the protocol itself.  The
    // best-effort thread/delete cleanup below runs only after this monitor is
    // stopped, so cleanup latency cannot turn a primary success or protocol
    // error into a timeout.
    let (result, created_thread) = run_protocol_without_cleanup(
        &mut transport,
        &question,
        &context,
        selected_path.as_deref(),
        &cancel,
        DEFAULT_TIMEOUT,
    );
    control.stopped.store(true, Ordering::Release);
    let _ = monitor.join();
    if let Some(thread) = created_thread.as_deref() {
        delete_thread(&mut transport, thread);
    }
    // Deleting a just-created thread is documented, but failure is harmless;
    // the process is always terminated so no app-server stays resident.
    transport.close();
    finalize_protocol_result(
        result,
        control.cancel.load(Ordering::Acquire),
        control.timed_out.load(Ordering::Acquire),
    )
}

#[tauri::command]
pub fn get_mentor_state(state: State<'_, MentorAppState>) -> MentorStateSnapshot {
    lock_runtime(&state).state.clone()
}

#[tauri::command]
pub fn ask_mentor(
    app: AppHandle,
    watcher_state: State<'_, WatcherAppState>,
    mentor_state: State<'_, MentorAppState>,
    request: AskMentorRequest,
) -> Result<MentorStateSnapshot, String> {
    let question = request.question.trim().to_string();
    if question.is_empty() {
        return Err("Ask Mentor question cannot be empty".to_string());
    }
    let selected_path = request.selected_path.filter(|path| !path.trim().is_empty());
    let context = watcher::capture_mentor_context(&watcher_state)?;
    build_prompt(&question, &context.analysis, selected_path.as_deref())?;

    let (request_id, cancel, cancel_flag, runtime_state) = {
        let mut runtime = lock_runtime(&mentor_state);
        if runtime.inflight.is_some() {
            return Err("Ask Mentor already has a question in progress".to_string());
        }
        runtime.next_id = runtime.next_id.wrapping_add(1);
        let request_id = runtime.next_id;
        let (cancel, cancel_receiver) = mpsc::channel();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        runtime.inflight = Some(InflightRequest {
            id: request_id,
            cancel,
            cancel_flag: cancel_flag.clone(),
        });
        runtime.state = MentorStateSnapshot {
            status: MentorStatus::Loading,
            answer: None,
            question: Some(question.clone()),
            selected_path: selected_path.clone(),
            error: None,
        };
        (
            request_id,
            cancel_receiver,
            cancel_flag,
            runtime.state.clone(),
        )
    };
    emit_state(&app, &runtime_state);

    let mentor_runtime = mentor_state.runtime.clone();
    let watcher_snapshot = (*watcher_state).clone();
    thread::spawn(move || {
        let result = run_request_with_flag(
            context.clone(),
            question.clone(),
            selected_path.clone(),
            cancel,
            cancel_flag,
        );
        let next = match result {
            Ok(answer) => MentorStateSnapshot {
                status: MentorStatus::Available,
                answer: Some(MentorAnswer {
                    answer,
                    question: question.clone(),
                    selected_path: selected_path.clone(),
                    generation: context.generation,
                }),
                question: Some(question.clone()),
                selected_path: selected_path.clone(),
                error: None,
            },
            Err(error) => error_state(Some(question.clone()), selected_path.clone(), error),
        };
        // The watcher gate makes context validation and Mentor event
        // publication one atomic boundary.  A concurrent complete/start/stop
        // command therefore cannot advance the current change between those
        // two operations and let a stale answer or error win the race.
        if watcher::publish_mentor_if_current(&watcher_snapshot, &context, || {
            set_state(&app, &mentor_runtime, request_id, next)
        })
        .is_none()
        {
            clear_request(&mentor_runtime, request_id);
        }
    });

    Ok(runtime_state)
}

#[tauri::command]
pub fn cancel_mentor(
    app: AppHandle,
    state: State<'_, MentorAppState>,
) -> Result<MentorStateSnapshot, String> {
    let (inflight, current) = {
        let mut runtime = lock_runtime(&state);
        let Some(inflight) = runtime.inflight.take() else {
            return Err("Ask Mentor has no question in progress".to_string());
        };
        // Invalidate the request while holding the same lock that workers use
        // before publishing.  The final response therefore cannot win a race
        // with cancellation and publish Available.
        let current = error_state(
            runtime.state.question.clone(),
            runtime.state.selected_path.clone(),
            "Ask Mentor was cancelled",
        );
        runtime.state = current.clone();
        (inflight, current)
    };
    inflight.cancel_flag.store(true, Ordering::Release);
    let _ = inflight.cancel.send(());
    emit_state(&app, &current);
    Ok(current)
}

/// Clear the Ask Mentor state whenever the dashboard leaves its frozen change
/// boundary.  An in-flight request is cancelled before the state is replaced;
/// its worker then fails the request-id check and cannot publish stale output.
#[tauri::command]
pub fn reset_mentor(app: AppHandle, state: State<'_, MentorAppState>) -> MentorStateSnapshot {
    let (inflight, current) = {
        let mut runtime = lock_runtime(&state);
        let inflight = runtime.inflight.take();
        runtime.state = MentorStateSnapshot::default();
        (inflight, runtime.state.clone())
    };
    if let Some(inflight) = inflight {
        inflight.cancel_flag.store(true, Ordering::Release);
        let _ = inflight.cancel.send(());
    }
    emit_state(&app, &current);
    current
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{AnalysisMetadata, ChangeRecord};
    use crate::diff::ContentStatus;
    use crate::watcher::FileChangeStatus;
    use std::collections::VecDeque;

    fn context() -> MentorContext {
        MentorContext {
            project_path: "C:/project".to_string(),
            generation: 2,
            analysis: ChangeAnalysis {
                record: ChangeRecord {
                    summary: "One file changed".to_string(),
                    purpose: "Unknown: task purpose was not supplied.".to_string(),
                    changed_components: vec!["src/main.ts".to_string()],
                    key_decisions: vec![],
                    how_it_works: "Snapshots are compared.".to_string(),
                    impact: "Unknown: runtime and product impact was not supplied.".to_string(),
                    risk: "Unknown: risk assessment was not supplied.".to_string(),
                    review_priority: "Unknown: review priority was not supplied.".to_string(),
                    programming_concepts: vec![],
                    relevant_code_locations: vec!["src/main.ts".to_string()],
                },
                metadata: AnalysisMetadata {
                    project_path: "C:/project".to_string(),
                    source: "local-snapshot".to_string(),
                    completion: "explicit".to_string(),
                    completion_generation: 2,
                    changed_file_count: 1,
                    supplied: Default::default(),
                },
                frozen_files: vec![ScopedFileContext {
                    path: "src/main.ts".to_string(),
                    status: FileChangeStatus::Modified,
                    content_status: ContentStatus::Text,
                    before: Some("before\n".to_string()),
                    after: Some("after\n".to_string()),
                }],
            },
        }
    }

    struct MockTransport {
        messages: VecDeque<Value>,
        sent: Vec<Value>,
    }

    impl MockTransport {
        fn new(messages: Vec<Value>) -> Self {
            Self {
                messages: messages.into(),
                sent: Vec::new(),
            }
        }
    }

    impl ProtocolTransport for MockTransport {
        fn send(&mut self, message: Value) -> Result<(), String> {
            self.sent.push(message);
            Ok(())
        }

        fn recv(&mut self, _timeout: Duration) -> Result<Option<Value>, String> {
            Ok(self.messages.pop_front())
        }

        fn close(&mut self) {}
    }

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn command_defaults_to_codex_app_server_and_honours_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("CODEX_APP_SERVER_EXECUTABLE");
        assert_eq!(
            app_server_command(),
            ("codex".to_string(), vec!["app-server".to_string()])
        );
        std::env::set_var("CODEX_APP_SERVER_EXECUTABLE", "codex-test");
        assert_eq!(
            app_server_command(),
            ("codex-test".to_string(), vec!["app-server".to_string()])
        );
        std::env::remove_var("CODEX_APP_SERVER_EXECUTABLE");
    }

    #[test]
    fn prompt_contains_only_record_changed_files_and_selected_frozen_content() {
        let prompt = build_prompt("Why?", &context().analysis, Some("src/main.ts")).unwrap();
        assert!(prompt.contains("One file changed"));
        assert!(prompt.contains("before\\n"));
        assert!(prompt.contains("after\\n"));
        assert!(prompt.contains("selectedFrozenFile"));
        assert!(prompt.contains("only the supplied current-change evidence"));
        assert!(!prompt.contains("worktree"));
    }

    #[test]
    fn selected_path_must_match_frozen_file_exactly() {
        let result = build_prompt("Why?", &context().analysis, Some("src\\main.ts"));
        assert!(result.is_err());
    }

    #[test]
    fn malformed_and_empty_structured_answers_are_rejected() {
        assert!(parse_structured_text("plain text").is_err());
        assert!(parse_structured_text(r#"{"answer":"  "}"#).is_err());
        assert_eq!(
            parse_structured_text(r#"{"answer":"Looks good."}"#).unwrap(),
            "Looks good."
        );
    }

    #[test]
    fn protocol_sends_handshake_restrictions_and_prefers_completed_message() {
        let messages = vec![
            json!({ "id": 1, "result": {} }),
            json!({ "id": 2, "result": { "thread": { "id": "thread-1" } } }),
            json!({ "id": 3, "result": { "turn": { "id": "turn-1" } } }),
            json!({ "method": "item/started", "params": { "item": { "id": "item-1", "type": "agentMessage" } } }),
            json!({ "method": "item/agentMessage/delta", "params": { "itemId": "item-1", "delta": r#"{"answer":"streamed"}"# } }),
            json!({ "method": "item/completed", "params": { "item": { "id": "item-1", "type": "agentMessage", "text": r#"{"answer":"completed"}"# } } }),
            json!({ "method": "turn/completed", "params": { "threadId": "thread-1", "turn": { "id": "turn-1", "status": "completed" } } }),
        ];
        let mut transport = MockTransport::new(messages);
        let (_cancel_sender, cancel) = mpsc::channel();
        let result = run_protocol(
            &mut transport,
            "Why?",
            &context(),
            None,
            &cancel,
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(result, "completed");
        assert_eq!(transport.sent[0]["method"], "initialize");
        assert_eq!(transport.sent[1]["method"], "initialized");
        assert_eq!(transport.sent[2]["method"], "thread/start");
        assert_eq!(transport.sent[2]["params"]["sandbox"], "read-only");
        assert_eq!(transport.sent[3]["method"], "turn/start");
        assert_eq!(transport.sent[3]["params"]["approvalPolicy"], "never");
        assert_eq!(
            transport.sent[3]["params"]["sandboxPolicy"],
            json!({
                "type": "readOnly",
                "networkAccess": false
            })
        );
    }

    #[test]
    fn protocol_accepts_completed_message_without_started_notification() {
        let messages = vec![
            json!({ "id": 1, "result": {} }),
            json!({ "id": 2, "result": { "thread": { "id": "thread-1" } } }),
            json!({ "id": 3, "result": { "turn": { "id": "turn-1" } } }),
            json!({ "method": "item/completed", "params": { "item": { "id": "item-1", "type": "agentMessage", "text": r#"{"answer":"completed"}"# } } }),
            json!({ "method": "turn/completed", "params": { "threadId": "thread-1", "turn": { "id": "turn-1", "status": "completed" } } }),
        ];
        let mut transport = MockTransport::new(messages);
        let (_cancel_sender, cancel) = mpsc::channel();
        let result = run_protocol(
            &mut transport,
            "Why?",
            &context(),
            None,
            &cancel,
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(result, "completed");
    }

    #[test]
    fn protocol_ignores_interleaved_agent_items() {
        let messages = vec![
            json!({ "id": 1, "result": {} }),
            json!({ "id": 2, "result": { "thread": { "id": "thread-1" } } }),
            json!({ "id": 3, "result": { "turn": { "id": "turn-1" } } }),
            json!({ "method": "item/started", "params": { "item": { "id": "item-1", "type": "agentMessage" } } }),
            json!({ "method": "item/agentMessage/delta", "params": { "itemId": "item-other", "delta": r#"{"answer":"wrong-item"}"# } }),
            json!({ "method": "item/started", "params": { "item": { "id": "item-other", "type": "agentMessage" } } }),
            json!({ "method": "item/completed", "params": { "item": { "id": "tool-item", "type": "commandExecution" } } }),
            json!({ "method": "item/agentMessage/delta", "params": { "itemId": "item-1", "delta": r#"{"answer":"right"}"# } }),
            json!({ "method": "item/completed", "params": { "item": { "id": "item-other", "type": "agentMessage", "text": r#"{"answer":"wrong-completed"}"# } } }),
            json!({ "method": "item/completed", "params": { "item": { "id": "item-1", "type": "agentMessage", "text": r#"{"answer":"right-completed"}"# } } }),
            json!({ "method": "turn/completed", "params": { "threadId": "thread-1", "turn": { "id": "turn-other", "status": "completed" } } }),
            json!({ "method": "turn/completed", "params": { "threadId": "thread-1", "turn": { "id": "turn-1", "status": "completed" } } }),
        ];
        let mut transport = MockTransport::new(messages);
        let (_cancel_sender, cancel) = mpsc::channel();
        let result = run_protocol(
            &mut transport,
            "Why?",
            &context(),
            None,
            &cancel,
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(result, "right-completed");
        assert!(transport
            .sent
            .iter()
            .any(|message| message["method"] == "thread/delete"));
    }

    #[test]
    fn protocol_rejects_tool_items() {
        let messages = vec![
            json!({ "id": 1, "result": {} }),
            json!({ "id": 2, "result": { "thread": { "id": "thread-1" } } }),
            json!({ "id": 3, "result": { "turn": { "id": "turn-1" } } }),
            json!({ "method": "item/started", "params": { "item": { "id": "tool-item", "type": "commandExecution" } } }),
        ];
        let mut transport = MockTransport::new(messages);
        let (_cancel_sender, cancel) = mpsc::channel();
        let result = run_protocol(
            &mut transport,
            "Why?",
            &context(),
            None,
            &cancel,
            Duration::from_secs(1),
        );
        assert!(
            matches!(result, Err(ProtocolFailure::Error(message)) if message.contains("unsupported Codex tool item") || message.contains("unsupported Codex request"))
        );
        assert!(transport
            .sent
            .iter()
            .any(|message| message["method"] == "thread/delete"));
    }

    #[test]
    fn interrupted_turn_is_explicit_cancellation() {
        let messages = vec![
            json!({ "id": 1, "result": {} }),
            json!({ "id": 2, "result": { "thread": { "id": "thread-1" } } }),
            json!({ "id": 3, "result": { "turn": { "id": "turn-1" } } }),
            json!({ "method": "turn/completed", "params": { "threadId": "thread-1", "turn": { "id": "turn-1", "status": "interrupted" } } }),
        ];
        let mut transport = MockTransport::new(messages);
        let (_cancel_sender, cancel) = mpsc::channel();
        let result = run_protocol(
            &mut transport,
            "Why?",
            &context(),
            None,
            &cancel,
            Duration::from_secs(1),
        );
        assert_eq!(result, Err(ProtocolFailure::Cancelled));
        assert!(transport
            .sent
            .iter()
            .any(|message| message["method"] == "thread/delete"));
    }

    #[test]
    fn turn_completion_correlates_present_thread_and_exact_turn_ids() {
        let documented_without_thread = json!({
            "method": "turn/completed",
            "params": { "turn": { "id": "turn-1", "status": "completed" } }
        });
        let wrong_thread = json!({
            "method": "turn/completed",
            "params": { "threadId": "thread-other", "turn": { "id": "turn-1", "status": "completed" } }
        });
        let wrong_turn = json!({
            "method": "turn/completed",
            "params": { "threadId": "thread-1", "turn": { "id": "turn-other", "status": "completed" } }
        });
        let exact = json!({
            "method": "turn/completed",
            "params": { "threadId": "thread-1", "turn": { "id": "turn-1", "status": "completed" } }
        });

        assert!(turn_event_matches(
            &documented_without_thread,
            "thread-1",
            "turn-1"
        ));
        assert!(!turn_event_matches(&wrong_thread, "thread-1", "turn-1"));
        assert!(!turn_event_matches(&wrong_turn, "thread-1", "turn-1"));
        assert!(turn_event_matches(&exact, "thread-1", "turn-1"));

        let error_missing_thread = json!({
            "method": "error",
            "params": { "turnId": "turn-1", "error": { "message": "failed" } }
        });
        let error_exact = json!({
            "method": "error",
            "params": {
                "threadId": "thread-1",
                "turnId": "turn-1",
                "error": { "message": "failed" }
            }
        });
        assert!(!turn_event_matches(
            &error_missing_thread,
            "thread-1",
            "turn-1"
        ));
        assert!(turn_event_matches(&error_exact, "thread-1", "turn-1"));
    }

    #[test]
    fn cleanup_deadline_cannot_override_primary_protocol_outcome() {
        assert_eq!(
            finalize_protocol_result(Ok("answer".to_string()), false, true),
            Ok("answer".to_string())
        );
        assert_eq!(
            finalize_protocol_result(
                Err(ProtocolFailure::Error(
                    "server rejected the turn".to_string()
                )),
                false,
                true,
            ),
            Err("server rejected the turn".to_string())
        );
        assert_eq!(
            finalize_protocol_result(
                Err(ProtocolFailure::Error(TIMEOUT_ERROR.to_string())),
                false,
                true,
            ),
            Err(TIMEOUT_ERROR.to_string())
        );
    }
}

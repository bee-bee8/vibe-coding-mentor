use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, State};

pub const LEARNING_MEMORY_STATE_EVENT: &str = "learning-memory-state";
const DATABASE_FILE: &str = "learning-memory.sqlite3";
const TABLE_NAME: &str = "learning_memory";
const SCHEMA_VERSION: i64 = 1;

const CREATE_SCHEMA_V1: &str = r#"
CREATE TABLE learning_memory (
    concept TEXT NOT NULL PRIMARY KEY,
    times_encountered INTEGER NOT NULL CHECK (times_encountered >= 1),
    status TEXT NOT NULL CHECK (status IN ('New', 'Learning', 'Familiar')),
    last_encountered TEXT NOT NULL,
    projects_encountered TEXT NOT NULL CHECK (
        json_valid(projects_encountered) = 1
        AND json_type(projects_encountered) = 'array'
    )
);
"#;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LearningStatus {
    New,
    Learning,
    Familiar,
}

impl LearningStatus {
    fn as_database_value(&self) -> &'static str {
        match self {
            Self::New => "New",
            Self::Learning => "Learning",
            Self::Familiar => "Familiar",
        }
    }
}

fn status_from_database(value: &str) -> Result<LearningStatus, String> {
    match value {
        "New" => Ok(LearningStatus::New),
        "Learning" => Ok(LearningStatus::Learning),
        "Familiar" => Ok(LearningStatus::Familiar),
        _ => Err(format!(
            "Learning Memory contains an invalid status: {value}"
        )),
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LearningMemoryRecord {
    pub concept: String,
    pub times_encountered: i64,
    pub status: LearningStatus,
    pub last_encountered: String,
    pub projects_encountered: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LearningMemoryStateStatus {
    Idle,
    Available,
    Error,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LearningMemoryStateSnapshot {
    pub revision: u64,
    pub relevant_concepts: Vec<String>,
    pub analysis_generation: Option<u64>,
    pub status: LearningMemoryStateStatus,
    pub records: Vec<LearningMemoryRecord>,
    pub error: Option<String>,
}

impl Default for LearningMemoryStateSnapshot {
    fn default() -> Self {
        Self {
            revision: 0,
            relevant_concepts: Vec::new(),
            analysis_generation: None,
            status: LearningMemoryStateStatus::Idle,
            records: Vec::new(),
            error: None,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetRelevantLearningMemoryRequest {
    pub concepts: Vec<String>,
    pub analysis_generation: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateLearningMemoryStatusRequest {
    pub concept: String,
    pub status: LearningStatus,
    pub analysis_generation: u64,
}

struct LearningMemoryRuntime {
    state: LearningMemoryStateSnapshot,
    relevant_concepts: Vec<String>,
    analysis_generation: Option<u64>,
    next_revision: u64,
}

#[derive(Clone)]
pub struct LearningMemoryAppState {
    runtime: Arc<Mutex<LearningMemoryRuntime>>,
    operation: Arc<Mutex<()>>,
}

impl Default for LearningMemoryAppState {
    fn default() -> Self {
        Self {
            runtime: Arc::new(Mutex::new(LearningMemoryRuntime {
                state: LearningMemoryStateSnapshot::default(),
                relevant_concepts: Vec::new(),
                analysis_generation: None,
                next_revision: 0,
            })),
            operation: Arc::new(Mutex::new(())),
        }
    }
}

fn lock_runtime(
    runtime: &Arc<Mutex<LearningMemoryRuntime>>,
) -> std::sync::MutexGuard<'_, LearningMemoryRuntime> {
    runtime
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn lock_operation(state: &LearningMemoryAppState) -> std::sync::MutexGuard<'_, ()> {
    state
        .operation
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn snapshot(state: &LearningMemoryAppState) -> LearningMemoryStateSnapshot {
    lock_runtime(&state.runtime).state.clone()
}

fn emit_state(app: &AppHandle, state: &LearningMemoryStateSnapshot) {
    let _ = app.emit(LEARNING_MEMORY_STATE_EVENT, state.clone());
}

fn reserve_revision_locked(
    runtime: &mut LearningMemoryRuntime,
    relevant_concepts: Vec<String>,
    analysis_generation: u64,
) -> Result<u64, String> {
    if let Some(current_generation) = runtime.analysis_generation {
        if analysis_generation < current_generation {
            return Err(format!(
                "The Learning Memory request belongs to stale analysis generation {analysis_generation}; current generation is {current_generation}"
            ));
        }
        if analysis_generation == current_generation
            && runtime.relevant_concepts != relevant_concepts
        {
            return Err(
                "The Learning Memory request does not match the current Change Record scope"
                    .to_string(),
            );
        }
    }
    runtime.next_revision = runtime.next_revision.checked_add(1).unwrap_or(u64::MAX);
    runtime.relevant_concepts = relevant_concepts;
    runtime.analysis_generation = Some(analysis_generation);
    Ok(runtime.next_revision)
}

fn reserve_revision(
    runtime: &Arc<Mutex<LearningMemoryRuntime>>,
    relevant_concepts: Vec<String>,
    analysis_generation: u64,
) -> Result<u64, String> {
    let mut runtime = lock_runtime(runtime);
    reserve_revision_locked(&mut runtime, relevant_concepts, analysis_generation)
}

fn reserve_status_revision(
    runtime: &Arc<Mutex<LearningMemoryRuntime>>,
    analysis_generation: u64,
) -> Result<(Vec<String>, u64), String> {
    let mut runtime = lock_runtime(runtime);
    if runtime.analysis_generation != Some(analysis_generation) {
        return Err("The status update does not match the current Change Record".to_string());
    }
    let relevant_concepts = runtime.relevant_concepts.clone();
    let revision = reserve_revision_locked(
        &mut runtime,
        relevant_concepts.clone(),
        analysis_generation,
    )?;
    Ok((relevant_concepts, revision))
}

fn publish_state(
    app: &AppHandle,
    runtime: &Arc<Mutex<LearningMemoryRuntime>>,
    revision: u64,
    relevant_concepts: Vec<String>,
    analysis_generation: Option<u64>,
    status: LearningMemoryStateStatus,
    records: Vec<LearningMemoryRecord>,
    error: Option<String>,
) -> Option<LearningMemoryStateSnapshot> {
    let next = {
        let mut runtime = lock_runtime(runtime);
        if revision != runtime.next_revision {
            return None;
        }
        let next = LearningMemoryStateSnapshot {
            revision,
            relevant_concepts: relevant_concepts.clone(),
            analysis_generation,
            status,
            records,
            error,
        };
        runtime.relevant_concepts = relevant_concepts;
        runtime.analysis_generation = analysis_generation;
        runtime.state = next.clone();
        next
    };
    emit_state(app, &next);
    Some(next)
}

fn publish_error(
    app: &AppHandle,
    runtime: &Arc<Mutex<LearningMemoryRuntime>>,
    revision: u64,
    relevant_concepts: Vec<String>,
    analysis_generation: Option<u64>,
    error: String,
) {
    let _ = publish_state(
        app,
        runtime,
        revision,
        relevant_concepts,
        analysis_generation,
        LearningMemoryStateStatus::Error,
        Vec::new(),
        Some(error),
    );
}

fn database_path(app: &AppHandle) -> Result<PathBuf, String> {
    let directory = app.path().app_data_dir().map_err(|error| {
        format!("Unable to resolve the Learning Memory app-data folder: {error}")
    })?;
    fs::create_dir_all(&directory).map_err(|error| {
        format!("Unable to create the Learning Memory app-data folder: {error}")
    })?;
    Ok(directory.join(DATABASE_FILE))
}

fn database_error(error: rusqlite::Error) -> String {
    format!("Learning Memory database error: {error}")
}

fn schema_version(connection: &Connection) -> Result<i64, String> {
    connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(database_error)
}

fn validate_schema_v1(connection: &Connection) -> Result<(), String> {
    let mut statement = connection
        .prepare("PRAGMA table_info(learning_memory)")
        .map_err(database_error)?;
    let columns = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(database_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(database_error)?;

    let expected = [
        ("concept", 1_i64, 1_i64),
        ("times_encountered", 1_i64, 0_i64),
        ("status", 1_i64, 0_i64),
        ("last_encountered", 1_i64, 0_i64),
        ("projects_encountered", 1_i64, 0_i64),
    ];
    if columns.len() != expected.len()
        || columns.iter().zip(expected).any(
            |(
                (name, not_null, primary_key),
                (expected_name, expected_not_null, expected_primary_key),
            )| {
                name != expected_name
                    || *not_null != expected_not_null
                    || *primary_key != expected_primary_key
            },
        )
    {
        return Err("Learning Memory schema v1 has unexpected columns".to_string());
    }

    let create_sql: String = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![TABLE_NAME],
            |row| row.get(0),
        )
        .map_err(database_error)?;
    let compact_sql = create_sql
        .to_ascii_lowercase()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    let required_constraints = [
        "check(times_encountered>=1)",
        "check(statusin('new','learning','familiar'))",
        "json_valid(projects_encountered)=1",
        "json_type(projects_encountered)='array'",
    ];
    if required_constraints
        .iter()
        .any(|constraint| !compact_sql.contains(constraint))
    {
        return Err("Learning Memory schema v1 constraints are incomplete".to_string());
    }
    Ok(())
}

fn ensure_schema(connection: &Connection) -> Result<(), String> {
    let version = schema_version(connection)?;
    if version > SCHEMA_VERSION {
        return Err(format!(
            "Learning Memory schema version {version} is newer than supported version {SCHEMA_VERSION}"
        ));
    }
    if version == SCHEMA_VERSION {
        return validate_schema_v1(connection);
    }

    let table_exists: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![TABLE_NAME],
            |row| row.get(0),
        )
        .map_err(database_error)?;
    if table_exists != 0 {
        return Err(
            "Learning Memory database has a table without a supported schema version".to_string(),
        );
    }

    connection
        .execute_batch(CREATE_SCHEMA_V1)
        .map_err(database_error)?;
    connection
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(database_error)
}

fn open_database(path: &Path) -> Result<Connection, String> {
    let connection = Connection::open(path).map_err(database_error)?;
    ensure_schema(&connection)?;
    Ok(connection)
}

pub fn normalize_concept(value: &str) -> Option<String> {
    let normalized = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    (!normalized.is_empty()).then_some(normalized)
}

pub fn normalize_concepts(values: &[String]) -> Vec<String> {
    values
        .iter()
        .filter_map(|value| normalize_concept(value))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn normalize_project(value: &str) -> Option<String> {
    let project = value.trim();
    (!project.is_empty()).then_some(project.to_string())
}

fn dedupe_projects(projects: impl IntoIterator<Item = String>) -> Vec<String> {
    projects
        .into_iter()
        .filter_map(|project| normalize_project(&project))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn parse_projects(value: &str) -> Result<Vec<String>, String> {
    let projects = serde_json::from_str::<Vec<String>>(value)
        .map_err(|error| format!("Learning Memory contains invalid project JSON: {error}"))?;
    let deduped = dedupe_projects(projects.clone());
    if deduped != projects {
        return Err("Learning Memory contains a non-deduplicated project list".to_string());
    }
    Ok(deduped)
}

fn record_from_values(
    concept: String,
    times_encountered: i64,
    status: String,
    last_encountered: String,
    projects_encountered: String,
) -> Result<LearningMemoryRecord, String> {
    if times_encountered < 1 {
        return Err("Learning Memory contains an encounter count below 1".to_string());
    }
    Ok(LearningMemoryRecord {
        concept,
        times_encountered,
        status: status_from_database(&status)?,
        last_encountered,
        projects_encountered: parse_projects(&projects_encountered)?,
    })
}

fn query_record(
    connection: &Connection,
    concept: &str,
) -> Result<Option<LearningMemoryRecord>, String> {
    let values = connection
        .query_row(
            "SELECT concept, times_encountered, status, last_encountered, projects_encountered
             FROM learning_memory WHERE concept = ?1",
            params![concept],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()
        .map_err(database_error)?;
    values
        .map(|(concept, times, status, last, projects)| {
            record_from_values(concept, times, status, last, projects)
        })
        .transpose()
}

fn read_relevant_from_connection(
    connection: &Connection,
    concepts: &[String],
) -> Result<Vec<LearningMemoryRecord>, String> {
    let mut records = Vec::new();
    for concept in normalize_concepts(concepts) {
        if let Some(record) = query_record(connection, &concept)? {
            records.push(record);
        }
    }
    Ok(records)
}

fn read_relevant_at_path(
    path: &Path,
    concepts: &[String],
) -> Result<Vec<LearningMemoryRecord>, String> {
    if normalize_concepts(concepts).is_empty() {
        return Ok(Vec::new());
    }
    let connection = open_database(path)?;
    read_relevant_from_connection(&connection, concepts)
}

fn record_concepts_at_path(
    path: &Path,
    concepts: &[String],
    project_path: &str,
    encountered_at: &str,
) -> Result<Vec<LearningMemoryRecord>, String> {
    let concepts = normalize_concepts(concepts);
    if concepts.is_empty() {
        return Ok(Vec::new());
    }
    let mut connection = open_database(path)?;
    let transaction = connection.transaction().map_err(database_error)?;
    let project = normalize_project(project_path);

    for concept in &concepts {
        if let Some(existing) = query_record(&transaction, concept)? {
            let next_count = existing
                .times_encountered
                .checked_add(1)
                .ok_or_else(|| "Learning Memory encounter count overflowed".to_string())?;
            let mut projects = existing.projects_encountered;
            if let Some(project) = project.as_ref() {
                projects.push(project.clone());
            }
            let projects_json = serde_json::to_string(&dedupe_projects(projects))
                .map_err(|error| format!("Unable to encode Learning Memory projects: {error}"))?;
            transaction
                .execute(
                    "UPDATE learning_memory
                     SET times_encountered = ?1, last_encountered = ?2, projects_encountered = ?3
                     WHERE concept = ?4",
                    params![next_count, encountered_at, projects_json, concept],
                )
                .map_err(database_error)?;
        } else {
            let projects_json = serde_json::to_string(
                &project.clone().into_iter().collect::<Vec<_>>(),
            )
            .map_err(|error| format!("Unable to encode Learning Memory projects: {error}"))?;
            transaction
                .execute(
                    "INSERT INTO learning_memory
                     (concept, times_encountered, status, last_encountered, projects_encountered)
                     VALUES (?1, 1, 'New', ?2, ?3)",
                    params![concept, encountered_at, projects_json],
                )
                .map_err(database_error)?;
        }
    }

    transaction.commit().map_err(database_error)?;
    read_relevant_from_connection(&connection, &concepts)
}

fn update_status_at_path(
    path: &Path,
    concept: &str,
    status: &LearningStatus,
) -> Result<LearningMemoryRecord, String> {
    let concept = normalize_concept(concept)
        .ok_or_else(|| "Learning Memory status updates require a concept".to_string())?;
    let connection = open_database(path)?;
    if query_record(&connection, &concept)?.is_none() {
        return Err("Learning Memory status updates require an encountered concept".to_string());
    }
    connection
        .execute(
            "UPDATE learning_memory SET status = ?1 WHERE concept = ?2",
            params![status.as_database_value(), concept],
        )
        .map_err(database_error)?;
    query_record(&connection, &concept)?
        .ok_or_else(|| "Learning Memory status update did not return its concept".to_string())
}

fn utc_timestamp() -> String {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let total_seconds = duration.as_secs() as i64;
    let days = total_seconds.div_euclid(86_400);
    let seconds_of_day = total_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_date_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    let millis = duration.subsec_millis();
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

fn civil_date_from_days(days_since_unix_epoch: i64) -> (i64, i64, i64) {
    let shifted = days_since_unix_epoch + 719_468;
    let era = if shifted >= 0 {
        shifted / 146_097
    } else {
        (shifted - 146_096) / 146_097
    };
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = month_part + if month_part < 10 { 3 } else { -9 };
    let year = year + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

pub(crate) fn refresh_relevant(
    app: &AppHandle,
    state: &LearningMemoryAppState,
    concepts: &[String],
    analysis_generation: u64,
) -> Result<Vec<LearningMemoryRecord>, String> {
    refresh_relevant_for_generation(app, state, concepts, analysis_generation)
}

fn refresh_relevant_for_generation(
    app: &AppHandle,
    state: &LearningMemoryAppState,
    concepts: &[String],
    analysis_generation: u64,
) -> Result<Vec<LearningMemoryRecord>, String> {
    let _operation = lock_operation(state);
    let normalized = normalize_concepts(concepts);
    let revision = reserve_revision(
        &state.runtime,
        normalized.clone(),
        analysis_generation,
    )?;
    refresh_relevant_after_reservation(
        app,
        state,
        normalized,
        revision,
        analysis_generation,
    )
}

fn refresh_relevant_after_reservation(
    app: &AppHandle,
    state: &LearningMemoryAppState,
    normalized: Vec<String>,
    revision: u64,
    analysis_generation: u64,
) -> Result<Vec<LearningMemoryRecord>, String> {
    if normalized.is_empty() {
        let _ = publish_state(
            app,
            &state.runtime,
            revision,
            normalized,
            Some(analysis_generation),
            LearningMemoryStateStatus::Available,
            Vec::new(),
            None,
        );
        return Ok(Vec::new());
    }

    let result = database_path(app).and_then(|path| read_relevant_at_path(&path, &normalized));
    match result {
        Ok(records) => {
            let _ = publish_state(
                app,
                &state.runtime,
                revision,
                normalized,
                Some(analysis_generation),
                LearningMemoryStateStatus::Available,
                records.clone(),
                None,
            );
            Ok(records)
        }
        Err(error) => {
            publish_error(
                app,
                &state.runtime,
                revision,
                normalized,
                Some(analysis_generation),
                error.clone(),
            );
            Err(error)
        }
    }
}

pub(crate) fn record_successful_teaching(
    app: &AppHandle,
    state: &LearningMemoryAppState,
    project_path: &str,
    analysis_generation: u64,
    concepts: &[String],
) -> Result<(), String> {
    let _operation = lock_operation(state);
    let normalized = normalize_concepts(concepts);
    let revision = reserve_revision(&state.runtime, normalized.clone(), analysis_generation)?;
    if normalized.is_empty() {
        let _ = publish_state(
            app,
            &state.runtime,
            revision,
            normalized,
            Some(analysis_generation),
            LearningMemoryStateStatus::Available,
            Vec::new(),
            None,
        );
        return Ok(());
    }

    let result = database_path(app).and_then(|path| {
        record_concepts_at_path(&path, &normalized, project_path, &utc_timestamp())
    });
    match result {
        Ok(records) => {
            let _ = publish_state(
                app,
                &state.runtime,
                revision,
                normalized,
                Some(analysis_generation),
                LearningMemoryStateStatus::Available,
                records,
                None,
            );
            Ok(())
        }
        Err(error) => {
            publish_error(
                app,
                &state.runtime,
                revision,
                normalized,
                Some(analysis_generation),
                error.clone(),
            );
            Err(error)
        }
    }
}

#[tauri::command]
pub fn get_learning_memory_state(
    state: State<'_, LearningMemoryAppState>,
) -> LearningMemoryStateSnapshot {
    snapshot(&state)
}

#[tauri::command]
pub fn get_relevant_learning_memory(
    app: AppHandle,
    state: State<'_, LearningMemoryAppState>,
    request: GetRelevantLearningMemoryRequest,
) -> Result<LearningMemoryStateSnapshot, String> {
    refresh_relevant_for_generation(
        &app,
        &state,
        &request.concepts,
        request.analysis_generation,
    )?;
    Ok(snapshot(&state))
}

#[tauri::command]
pub fn update_learning_memory_status(
    app: AppHandle,
    state: State<'_, LearningMemoryAppState>,
    request: UpdateLearningMemoryStatusRequest,
) -> Result<LearningMemoryStateSnapshot, String> {
    let concept = normalize_concept(&request.concept)
        .ok_or_else(|| "Learning Memory status updates require a concept".to_string())?;
    let _operation = lock_operation(&state);
    let analysis_generation = request.analysis_generation;
    let (relevant_concepts, revision) =
        reserve_status_revision(&state.runtime, analysis_generation)?;
    if !relevant_concepts.iter().any(|item| item == &concept) {
        let error = "The concept is not part of the current Change Record".to_string();
        publish_error(
            &app,
            &state.runtime,
            revision,
            relevant_concepts,
            Some(analysis_generation),
            error.clone(),
        );
        return Err(error);
    }

    let result = database_path(&app).and_then(|path| {
        update_status_at_path(&path, &concept, &request.status)?;
        read_relevant_at_path(&path, &relevant_concepts)
    });
    match result {
        Ok(records) => {
            let _ = publish_state(
                &app,
                &state.runtime,
                revision,
                relevant_concepts,
                Some(analysis_generation),
                LearningMemoryStateStatus::Available,
                records,
                None,
            );
            Ok(snapshot(&state))
        }
        Err(error) => {
            publish_error(
                &app,
                &state.runtime,
                revision,
                relevant_concepts,
                Some(analysis_generation),
                error.clone(),
            );
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{mpsc, Arc};
    use std::thread;

    static TEST_DATABASE_ID: AtomicU64 = AtomicU64::new(0);

    fn test_database_path() -> PathBuf {
        let id = TEST_DATABASE_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "codex-mentor-learning-memory-{}-{id}.sqlite3",
            std::process::id()
        ))
    }

    fn remove_database(path: &Path) {
        let _ = fs::remove_file(path);
    }

    #[test]
    fn normalizes_and_deduplicates_current_concepts() {
        let concepts = vec![
            " Functions ".to_string(),
            "functions".to_string(),
            "FUNCTIONS".to_string(),
            " async   await ".to_string(),
            "".to_string(),
        ];
        assert_eq!(
            normalize_concepts(&concepts),
            vec!["async await".to_string(), "functions".to_string()]
        );
    }

    #[test]
    fn reverse_generation_reservation_keeps_newer_scope() {
        let state = LearningMemoryAppState::default();
        let newer_concepts = vec!["new concept".to_string()];
        let newer_revision =
            reserve_revision(&state.runtime, newer_concepts.clone(), 8).unwrap();

        let stale = reserve_revision(&state.runtime, vec!["old concept".to_string()], 7);

        assert!(stale.unwrap_err().contains("stale analysis generation"));
        let runtime = lock_runtime(&state.runtime);
        assert_eq!(runtime.analysis_generation, Some(8));
        assert_eq!(runtime.relevant_concepts, newer_concepts);
        assert_eq!(runtime.next_revision, newer_revision);
    }

    #[test]
    fn same_generation_stale_concepts_cannot_reserve_a_revision() {
        let state = LearningMemoryAppState::default();
        let current_concepts = vec!["functions".to_string()];
        let current_revision =
            reserve_revision(&state.runtime, current_concepts.clone(), 8).unwrap();

        let stale = reserve_revision(&state.runtime, vec!["loops".to_string()], 8);

        assert!(stale.unwrap_err().contains("current Change Record scope"));
        let runtime = lock_runtime(&state.runtime);
        assert_eq!(runtime.analysis_generation, Some(8));
        assert_eq!(runtime.relevant_concepts, current_concepts);
        assert_eq!(runtime.next_revision, current_revision);
    }

    #[test]
    fn concurrent_status_writes_are_serialized_before_persisting() {
        let path = test_database_path();
        record_concepts_at_path(
            &path,
            &["functions".to_string()],
            "C:/project",
            "2026-08-14T01:02:03.000Z",
        )
        .unwrap();

        let state = Arc::new(LearningMemoryAppState::default());
        {
            let _operation = lock_operation(&state);
            reserve_revision(&state.runtime, vec!["functions".to_string()], 8).unwrap();
        }

        let (first_ready_sender, first_ready_receiver) = mpsc::sync_channel(0);
        let (release_first_sender, release_first_receiver) = mpsc::sync_channel(0);
        let first_state = state.clone();
        let first_path = path.clone();
        let first = thread::spawn(move || {
            let _operation = lock_operation(&first_state);
            let (relevant_concepts, revision) =
                reserve_status_revision(&first_state.runtime, 8).unwrap();
            first_ready_sender.send(revision).unwrap();
            release_first_receiver.recv().unwrap();
            let record = update_status_at_path(
                &first_path,
                &relevant_concepts[0],
                &LearningStatus::Learning,
            )
            .unwrap();
            (revision, record)
        });

        let reserved_first_revision = first_ready_receiver.recv().unwrap();
        let (second_started_sender, second_started_receiver) = mpsc::sync_channel(0);
        let (second_acquired_sender, second_acquired_receiver) = mpsc::channel();
        let second_state = state.clone();
        let second_path = path.clone();
        let second = thread::spawn(move || {
            second_started_sender.send(()).unwrap();
            let _operation = lock_operation(&second_state);
            second_acquired_sender.send(()).unwrap();
            let (relevant_concepts, revision) =
                reserve_status_revision(&second_state.runtime, 8).unwrap();
            let record = update_status_at_path(
                &second_path,
                &relevant_concepts[0],
                &LearningStatus::Familiar,
            )
            .unwrap();
            (revision, record)
        });

        second_started_receiver.recv().unwrap();
        assert!(second_acquired_receiver.try_recv().is_err());
        release_first_sender.send(()).unwrap();

        let (first_revision, first_record) = first.join().unwrap();
        second_acquired_receiver.recv().unwrap();
        let (second_revision, second_record) = second.join().unwrap();
        let final_record = read_relevant_at_path(&path, &["functions".to_string()])
            .unwrap()
            .pop()
            .unwrap();

        assert_eq!(first_revision, reserved_first_revision);
        assert!(second_revision > first_revision);
        assert_eq!(first_record.status, LearningStatus::Learning);
        assert_eq!(second_record.status, LearningStatus::Familiar);
        assert_eq!(final_record.status, LearningStatus::Familiar);
        remove_database(&path);
    }

    #[test]
    fn schema_v1_creates_new_records_with_new_status() {
        let path = test_database_path();
        let concepts = vec![" Functions ".to_string(), "functions".to_string()];
        let records =
            record_concepts_at_path(&path, &concepts, "C:/one", "2026-08-14T01:02:03.000Z")
                .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].concept, "functions");
        assert_eq!(records[0].times_encountered, 1);
        assert_eq!(records[0].status, LearningStatus::New);
        assert_eq!(records[0].projects_encountered, vec!["C:/one"]);

        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        validate_schema_v1(&connection).unwrap();
        remove_database(&path);
    }

    #[test]
    fn later_encounters_increment_once_and_preserve_user_status() {
        let path = test_database_path();
        let concept = vec!["api".to_string(), "API".to_string()];
        record_concepts_at_path(&path, &concept, "C:/one", "2026-08-14T01:02:03.000Z").unwrap();
        let first = update_status_at_path(&path, "API", &LearningStatus::Learning).unwrap();
        assert_eq!(first.status, LearningStatus::Learning);

        let second = record_concepts_at_path(
            &path,
            &[" api ".to_string(), "API".to_string()],
            "C:/one",
            "2026-08-15T01:02:03.000Z",
        )
        .unwrap();
        assert_eq!(second[0].times_encountered, 2);
        assert_eq!(second[0].status, LearningStatus::Learning);
        assert_eq!(second[0].last_encountered, "2026-08-15T01:02:03.000Z");
        assert_eq!(second[0].projects_encountered, vec!["C:/one"]);

        let third = record_concepts_at_path(
            &path,
            &["api".to_string()],
            "C:/two",
            "2026-08-16T01:02:03.000Z",
        )
        .unwrap();
        assert_eq!(third[0].times_encountered, 3);
        assert_eq!(third[0].status, LearningStatus::Learning);
        assert_eq!(third[0].projects_encountered, vec!["C:/one", "C:/two"]);
        remove_database(&path);
    }

    #[test]
    fn explicit_status_update_does_not_change_encounter_data() {
        let path = test_database_path();
        record_concepts_at_path(
            &path,
            &["loops".to_string()],
            "C:/one",
            "2026-08-14T01:02:03.000Z",
        )
        .unwrap();
        let before = read_relevant_at_path(&path, &["loops".to_string()]).unwrap();
        let after = update_status_at_path(&path, "loops", &LearningStatus::Familiar).unwrap();
        assert_eq!(after.times_encountered, before[0].times_encountered);
        assert_eq!(after.last_encountered, before[0].last_encountered);
        assert_eq!(after.projects_encountered, before[0].projects_encountered);
        assert_eq!(after.status, LearningStatus::Familiar);
        remove_database(&path);
    }

    #[test]
    fn future_schema_is_rejected_without_initializing_or_mutating_it() {
        let path = test_database_path();
        let connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        drop(connection);

        let result = read_relevant_at_path(&path, &["functions".to_string()]);
        assert!(result.unwrap_err().contains("newer than supported"));

        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let table_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                params![TABLE_NAME],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION + 1);
        assert_eq!(table_count, 0);
        remove_database(&path);
    }

    #[test]
    fn timestamps_are_explicit_utc_text() {
        assert!(utc_timestamp().ends_with('Z'));
        assert_eq!(civil_date_from_days(0), (1970, 1, 1));
        assert_eq!(civil_date_from_days(20_000), (2024, 10, 4));
    }
}

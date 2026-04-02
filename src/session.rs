use crate::config::{AppConfig, AppPaths};
use crate::error::{AppError, AppResult, EXIT_SESSION, ResultCodeExt};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use ulid::Ulid;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    Meta(SessionMeta),
    Message(SessionMessage),
    Response(SessionResponse),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub session_id: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    pub role: String,
    pub content: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionResponse {
    pub provider: String,
    pub model: String,
    pub finish_reason: String,
    pub latency_ms: u64,
    pub usage: Usage,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionState {
    pub current_session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_temp: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub session_id: String,
    pub is_current: bool,
    pub is_temp: bool,
    pub created_at: Option<u64>,
    pub updated_at: Option<u64>,
    pub first_prompt: Option<String>,
    pub user_messages: usize,
    pub assistant_messages: usize,
}

pub fn generate_session_id() -> String {
    format!("sess_{}", Ulid::new())
}

pub fn generate_temp_session_id() -> String {
    format!("tmp_{}", Ulid::new())
}

pub fn is_temp_session(session_id: &str) -> bool {
    session_id.starts_with("tmp_")
}

/// Returns the short display ID: strips the prefix (sess_/tmp_) and takes the first 8 chars.
pub fn short_id(session_id: &str) -> String {
    let bare = session_id
        .strip_prefix("sess_")
        .or_else(|| session_id.strip_prefix("tmp_"))
        .unwrap_or(session_id);
    let prefix = if session_id.starts_with("tmp_") {
        "tmp_"
    } else {
        ""
    };
    let short = if bare.len() > 8 { &bare[..8] } else { bare };
    format!("{prefix}{short}")
}

/// Resolve a potentially abbreviated session ID to its full form via prefix matching.
/// Supports: full ID, prefixed short ID ("sess_01JQ"), bare short ID ("01JQ"), tmp-prefixed ("tmp_01JQ").
pub fn resolve_session_id(paths: &AppPaths, config: &AppConfig, input: &str) -> AppResult<String> {
    let all_sessions = list_sessions(paths, config)?;

    // Exact match first
    if all_sessions.contains(&input.to_string()) {
        return Ok(input.to_string());
    }

    // Prefix match: try input as-is prefix
    let mut matches: Vec<&String> = all_sessions
        .iter()
        .filter(|id| id.starts_with(input))
        .collect();

    // If no match, try matching as bare ID (without sess_/tmp_ prefix) against the bare part
    if matches.is_empty() {
        matches = all_sessions
            .iter()
            .filter(|id| {
                let bare = id
                    .strip_prefix("sess_")
                    .or_else(|| id.strip_prefix("tmp_"))
                    .unwrap_or(id);
                bare.starts_with(input)
            })
            .collect();
    }

    match matches.len() {
        0 => Err(AppError::new(
            EXIT_SESSION,
            format!("no session matching `{input}`"),
        )),
        1 => Ok(matches[0].clone()),
        n => {
            let previews: Vec<String> = matches.iter().map(|id| short_id(id)).collect();
            Err(AppError::new(
                EXIT_SESSION,
                format!(
                    "`{input}` is ambiguous, matches {n} sessions: {}",
                    previews.join(", ")
                ),
            ))
        }
    }
}

pub fn session_file(paths: &AppPaths, config: &AppConfig, session_id: &str) -> PathBuf {
    paths
        .sessions_dir(config)
        .join(format!("{session_id}.jsonl"))
}

pub fn load_state(paths: &AppPaths) -> AppResult<SessionState> {
    if !paths.state_file.exists() {
        return Ok(SessionState::default());
    }
    let text = fs::read_to_string(&paths.state_file).code(
        EXIT_SESSION,
        format!("failed to read `{}`", paths.state_file.display()),
    )?;
    toml::from_str(&text).code(EXIT_SESSION, "failed to parse state.toml")
}

pub fn save_state(paths: &AppPaths, state: &SessionState) -> AppResult<()> {
    if let Some(parent) = paths.state_file.parent() {
        fs::create_dir_all(parent).code(EXIT_SESSION, "failed to create state file parent dir")?;
    }
    let text =
        toml::to_string_pretty(state).code(EXIT_SESSION, "failed to serialize session state")?;
    fs::write(&paths.state_file, text).code(
        EXIT_SESSION,
        format!("failed to write `{}`", paths.state_file.display()),
    )
}

pub fn set_current_session(
    paths: &AppPaths,
    config: &AppConfig,
    session_id: Option<&str>,
    temp: bool,
) -> AppResult<()> {
    let mut state = load_state(paths)?;

    // Auto-cleanup: if the old current session was temp and we're switching away, delete it
    if let Some(old_id) = &state.current_session {
        let switching_away = session_id.map_or(true, |new_id| new_id != old_id);
        if switching_away && state.is_temp.unwrap_or(false) {
            let old_file = session_file(paths, config, old_id);
            if old_file.exists() {
                let _ = fs::remove_file(&old_file);
            }
        }
    }

    state.current_session = session_id.map(|value| value.to_string());
    state.is_temp = if temp { Some(true) } else { None };
    save_state(paths, &state)
}

pub fn append_events(
    paths: &AppPaths,
    config: &AppConfig,
    session_id: &str,
    events: &[SessionEvent],
) -> AppResult<()> {
    let path = session_file(paths, config, session_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).code(EXIT_SESSION, "failed to create sessions dir")?;
    }
    let exists = path.exists();
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .code(
            EXIT_SESSION,
            format!("failed to open session file `{}`", path.display()),
        )?;
    if !exists {
        let meta = SessionEvent::Meta(SessionMeta {
            session_id: session_id.to_string(),
            created_at: now_rfc3339(),
        });
        let line =
            serde_json::to_string(&meta).code(EXIT_SESSION, "failed to serialize session meta")?;
        writeln!(file, "{line}").code(EXIT_SESSION, "failed to write session meta")?;
    }
    for event in events {
        let line =
            serde_json::to_string(event).code(EXIT_SESSION, "failed to serialize session event")?;
        writeln!(file, "{line}").code(EXIT_SESSION, "failed to write session event")?;
    }
    Ok(())
}

pub fn read_events(
    paths: &AppPaths,
    config: &AppConfig,
    session_id: &str,
) -> AppResult<Vec<SessionEvent>> {
    let path = session_file(paths, config, session_id);
    if !path.exists() {
        return Err(AppError::new(
            EXIT_SESSION,
            format!("session `{session_id}` does not exist"),
        ));
    }
    let text = fs::read_to_string(&path).code(
        EXIT_SESSION,
        format!("failed to read session file `{}`", path.display()),
    )?;
    let mut events = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let event = serde_json::from_str::<SessionEvent>(line).map_err(|err| {
            AppError::new(
                EXIT_SESSION,
                format!(
                    "failed to parse session file `{}` at line {}: {}",
                    path.display(),
                    index + 1,
                    err
                ),
            )
        })?;
        events.push(event);
    }
    Ok(events)
}

pub fn list_sessions(paths: &AppPaths, config: &AppConfig) -> AppResult<Vec<String>> {
    let dir = paths.sessions_dir(config);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut items = Vec::new();
    for entry in fs::read_dir(&dir).code(EXIT_SESSION, "failed to read sessions dir")? {
        let entry = entry.code(EXIT_SESSION, "failed to read sessions dir entry")?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                items.push(stem.to_string());
            }
        }
    }
    items.sort();
    items.reverse();
    Ok(items)
}

pub fn list_session_summaries(
    paths: &AppPaths,
    config: &AppConfig,
    current_session: Option<&str>,
) -> AppResult<Vec<SessionSummary>> {
    let session_ids = list_sessions(paths, config)?;
    let mut summaries = Vec::new();
    for session_id in session_ids {
        let events = read_events(paths, config, &session_id)?;
        summaries.push(build_session_summary(
            &session_id,
            &events,
            current_session.is_some_and(|value| value == session_id),
        ));
    }
    summaries.sort_by_key(|summary| std::cmp::Reverse(summary.updated_at.unwrap_or(0)));
    Ok(summaries)
}

pub fn delete_session(paths: &AppPaths, config: &AppConfig, session_id: &str) -> AppResult<()> {
    let path = session_file(paths, config, session_id);
    if !path.exists() {
        return Err(AppError::new(
            EXIT_SESSION,
            format!("session `{session_id}` does not exist"),
        ));
    }
    fs::remove_file(&path).code(
        EXIT_SESSION,
        format!("failed to delete `{}`", path.display()),
    )
}

pub fn clear_sessions(
    paths: &AppPaths,
    config: &AppConfig,
    include_current: bool,
) -> AppResult<usize> {
    let dir = paths.sessions_dir(config);
    if !dir.exists() {
        return Ok(0);
    }
    let current = if include_current {
        String::new()
    } else {
        load_state(paths)?.current_session.unwrap_or_default()
    };
    let mut removed = 0;
    for entry in fs::read_dir(&dir).code(EXIT_SESSION, "failed to read sessions dir")? {
        let entry = entry.code(EXIT_SESSION, "failed to read sessions dir entry")?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if !current.is_empty() && name == current {
            continue;
        }
        if fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

pub fn gc_sessions(paths: &AppPaths, config: &AppConfig) -> AppResult<usize> {
    let dir = paths.sessions_dir(config);
    if !dir.exists() {
        return Ok(0);
    }
    let state = load_state(paths)?;
    let current = state.current_session.as_deref().unwrap_or("");
    let mut removed = 0;
    for entry in fs::read_dir(&dir).code(EXIT_SESSION, "failed to read sessions dir")? {
        let entry = entry.code(EXIT_SESSION, "failed to read sessions dir entry")?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let metadata = fs::metadata(&path).code(EXIT_SESSION, "failed to read session metadata")?;
        let should_remove = metadata.len() == 0 || (is_temp_session(stem) && stem != current);
        if should_remove {
            fs::remove_file(&path).code(EXIT_SESSION, "failed to remove session file")?;
            removed += 1;
        }
    }
    Ok(removed)
}

pub fn now_rfc3339() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    secs.to_string()
}

fn build_session_summary(
    session_id: &str,
    events: &[SessionEvent],
    is_current: bool,
) -> SessionSummary {
    let mut created_at = None;
    let mut updated_at = None;
    let mut first_prompt = None;
    let mut user_messages = 0;
    let mut assistant_messages = 0;

    for event in events {
        match event {
            SessionEvent::Meta(meta) => {
                created_at = parse_timestamp(&meta.created_at);
                updated_at = created_at;
            }
            SessionEvent::Message(message) => {
                let ts = parse_timestamp(&message.created_at);
                if ts.is_some() {
                    updated_at = ts;
                }
                if message.role == "user" {
                    user_messages += 1;
                    if first_prompt.is_none() {
                        first_prompt = Some(preview_text(&message.content));
                    }
                } else if message.role == "assistant" {
                    assistant_messages += 1;
                }
            }
            SessionEvent::Response(response) => {
                let ts = parse_timestamp(&response.created_at);
                if ts.is_some() {
                    updated_at = ts;
                }
            }
        }
    }

    SessionSummary {
        session_id: session_id.to_string(),
        is_current,
        is_temp: is_temp_session(session_id),
        created_at,
        updated_at,
        first_prompt,
        user_messages,
        assistant_messages,
    }
}

fn parse_timestamp(value: &str) -> Option<u64> {
    value.parse().ok()
}

fn preview_text(value: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= 60 {
        normalized
    } else {
        format!("{}...", normalized.chars().take(60).collect::<String>())
    }
}

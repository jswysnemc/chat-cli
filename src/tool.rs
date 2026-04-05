use crate::config::{AppConfig, expand_tilde};
use crate::error::{AppError, AppResult, EXIT_ARGS};
use serde_json::{Value, json};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

// ANSI codes for tool UI
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";
const CYAN: &str = "\x1b[36m";
const YELLOW: &str = "\x1b[33m";
const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const SKILL_FILE_NAME: &str = "SKILL.md";
const MAX_SKILL_READ_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSideEffects {
    ReadOnly,
    Mutating,
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolParallelism {
    ParallelSafe,
    SequentialOnly,
}

#[derive(Debug, Clone, Copy)]
pub struct ToolSpec {
    pub name: &'static str,
    #[allow(dead_code)]
    pub description: &'static str,
    pub side_effects: ToolSideEffects,
    pub parallelism: ToolParallelism,
    pub requires_confirmation: bool,
    pub definition: fn() -> Value,
}

impl ToolSpec {
    pub fn is_parallel_safe(&self) -> bool {
        self.parallelism == ToolParallelism::ParallelSafe
    }
}

#[derive(Debug, Clone, Copy)]
struct ToolRuntimeContext<'a> {
    auto_confirm: bool,
    config: &'a AppConfig,
}

#[derive(Clone, Copy)]
struct ToolHandler {
    spec: ToolSpec,
    execute: fn(&ToolCall, &ToolRuntimeContext<'_>) -> AppResult<String>,
}

/// Confirmation result from the user.
#[derive(Debug)]
enum ConfirmResult {
    Yes,
    No,
    Edit(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SkillEntry {
    name: String,
    scope: String,
    path: PathBuf,
    summary: Option<String>,
}

const BUILTIN_TOOL_HANDLERS: [ToolHandler; 7] = [
    ToolHandler {
        spec: ToolSpec {
            name: "read",
            description: "Read the contents of a file at the given path.",
            side_effects: ToolSideEffects::ReadOnly,
            parallelism: ToolParallelism::ParallelSafe,
            requires_confirmation: false,
            definition: define_read_tool,
        },
        execute: execute_read_tool,
    },
    ToolHandler {
        spec: ToolSpec {
            name: "write",
            description: "Write content to a file via overwrite or unified diff patch.",
            side_effects: ToolSideEffects::Mutating,
            parallelism: ToolParallelism::SequentialOnly,
            requires_confirmation: true,
            definition: define_write_tool,
        },
        execute: execute_write_tool,
    },
    ToolHandler {
        spec: ToolSpec {
            name: "bash",
            description: "Execute a bash command and return stdout and stderr.",
            side_effects: ToolSideEffects::Mutating,
            parallelism: ToolParallelism::SequentialOnly,
            requires_confirmation: true,
            definition: define_bash_tool,
        },
        execute: execute_bash_tool,
    },
    ToolHandler {
        spec: ToolSpec {
            name: "grep",
            description: "Search for a regex pattern in files with ripgrep.",
            side_effects: ToolSideEffects::ReadOnly,
            parallelism: ToolParallelism::ParallelSafe,
            requires_confirmation: false,
            definition: define_grep_tool,
        },
        execute: execute_grep_tool,
    },
    ToolHandler {
        spec: ToolSpec {
            name: "fetch",
            description: "Fetch content from a URL via HTTP GET.",
            side_effects: ToolSideEffects::External,
            parallelism: ToolParallelism::ParallelSafe,
            requires_confirmation: true,
            definition: define_fetch_tool,
        },
        execute: execute_fetch_tool,
    },
    ToolHandler {
        spec: ToolSpec {
            name: "skills_list",
            description: "List available project and global skills.",
            side_effects: ToolSideEffects::ReadOnly,
            parallelism: ToolParallelism::ParallelSafe,
            requires_confirmation: false,
            definition: define_skills_list_tool,
        },
        execute: execute_skills_list_tool,
    },
    ToolHandler {
        spec: ToolSpec {
            name: "skill_read",
            description: "Read the SKILL.md content for a named skill.",
            side_effects: ToolSideEffects::ReadOnly,
            parallelism: ToolParallelism::ParallelSafe,
            requires_confirmation: false,
            definition: define_skill_read_tool,
        },
        execute: execute_skill_read_tool,
    },
];

fn define_read_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "read",
            "description": "Read the contents of a file at the given path.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute or relative file path to read."
                    }
                },
                "required": ["path"]
            }
        }
    })
}

fn define_write_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "write",
            "description": "Write content to a file. Supports two modes: 'overwrite' (default) replaces the entire file, 'diff' applies a unified diff patch to the existing file.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute or relative file path to write."
                    },
                    "content": {
                        "type": "string",
                        "description": "The content to write. In 'overwrite' mode: full file content. In 'diff' mode: unified diff format."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["overwrite", "diff"],
                        "description": "Write mode. 'overwrite' replaces the file entirely (default). 'diff' applies a unified diff patch."
                    }
                },
                "required": ["path", "content"]
            }
        }
    })
}

fn define_bash_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "bash",
            "description": "Execute a bash command and return its stdout and stderr output.",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The bash command to execute."
                    }
                },
                "required": ["command"]
            }
        }
    })
}

fn define_grep_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "grep",
            "description": "Search for a regex pattern in files. Returns matching lines with file paths and line numbers. Max 50 results.",
            "parameters": {
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Regex pattern to search for."
                    },
                    "path": {
                        "type": "string",
                        "description": "File or directory to search in."
                    },
                    "include": {
                        "type": "string",
                        "description": "Optional glob pattern to filter files, e.g. '*.rs', '*.py'."
                    }
                },
                "required": ["pattern", "path"]
            }
        }
    })
}

fn define_fetch_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "fetch",
            "description": "Fetch content from a URL via HTTP GET. Returns the response body as text (max 32KB).",
            "parameters": {
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to fetch."
                    }
                },
                "required": ["url"]
            }
        }
    })
}

fn define_skills_list_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "skills_list",
            "description": "List available skills from the current project's .claude/skills directory and ~/.claude/skills.",
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Optional substring filter for the skill name."
                    }
                }
            }
        }
    })
}

fn define_skill_read_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "skill_read",
            "description": "Read the SKILL.md content for a named skill.",
            "parameters": {
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Skill name, for example 'agent-browser', 'ui-ux-pro-max', or '.system/openai-docs'. You can also use 'project:name' or 'global:name' to disambiguate."
                    },
                    "scope": {
                        "type": "string",
                        "description": "Optional skill scope filter, e.g. 'project', 'global', or 'path:./.claude/skills'."
                    }
                },
                "required": ["name"]
            }
        }
    })
}

fn builtin_tool_handlers() -> &'static [ToolHandler] {
    &BUILTIN_TOOL_HANDLERS
}

fn find_tool_handler(name: &str) -> Option<&'static ToolHandler> {
    builtin_tool_handlers()
        .iter()
        .find(|handler| handler.spec.name == name)
}

pub fn lookup_tool_spec(name: &str) -> Option<&'static ToolSpec> {
    find_tool_handler(name).map(|handler| &handler.spec)
}

/// Returns OpenAI-compatible tool definitions.
pub fn tool_definitions() -> Vec<Value> {
    builtin_tool_handlers()
        .iter()
        .map(|handler| (handler.spec.definition)())
        .collect()
}

/// Parse a tool call from raw API JSON.
pub fn parse_tool_call(raw: &Value) -> AppResult<ToolCall> {
    let id = raw["id"]
        .as_str()
        .ok_or_else(|| AppError::new(EXIT_ARGS, "tool_call missing id"))?
        .to_string();
    let name = raw["function"]["name"]
        .as_str()
        .ok_or_else(|| AppError::new(EXIT_ARGS, "tool_call missing function.name"))?
        .to_string();
    let arguments_str = raw["function"]["arguments"].as_str().unwrap_or("{}");
    let arguments: Value = serde_json::from_str(arguments_str).unwrap_or(json!({}));
    Ok(ToolCall {
        id,
        name,
        arguments,
    })
}

/// Execute a tool call with optional user confirmation.
pub fn execute_tool(
    call: &ToolCall,
    auto_confirm: bool,
    config: &AppConfig,
) -> AppResult<ToolResult> {
    let context = ToolRuntimeContext {
        auto_confirm,
        config,
    };
    let content = match find_tool_handler(&call.name) {
        Some(handler) => (handler.execute)(call, &context)?,
        None => format!("error: unknown tool '{}'", call.name),
    };
    Ok(ToolResult {
        tool_call_id: call.id.clone(),
        content,
    })
}

fn execute_read_tool(call: &ToolCall, _context: &ToolRuntimeContext<'_>) -> AppResult<String> {
    let path = call.arguments["path"]
        .as_str()
        .ok_or_else(|| AppError::new(EXIT_ARGS, "read tool: missing 'path' argument"))?;
    print_tool_header("read", path);
    tool_read(path)
}

fn execute_write_tool(call: &ToolCall, context: &ToolRuntimeContext<'_>) -> AppResult<String> {
    let path = call.arguments["path"]
        .as_str()
        .ok_or_else(|| AppError::new(EXIT_ARGS, "write tool: missing 'path' argument"))?;
    let content = call.arguments["content"]
        .as_str()
        .ok_or_else(|| AppError::new(EXIT_ARGS, "write tool: missing 'content' argument"))?;
    let mode = call.arguments["mode"].as_str().unwrap_or("overwrite");

    match mode {
        "diff" => {
            let (additions, deletions) = count_diff_changes(content);
            print_tool_header_detail(
                "write",
                &format!("{path} {GREEN}+{additions}{RESET} {RED}-{deletions}{RESET}"),
                "diff",
            );
            print_tool_preview(&render_diff_preview(path, content));
            match confirm_tool_action(
                &format!("apply diff to {YELLOW}{path}{RESET}"),
                Some(content),
                context.auto_confirm,
            )? {
                ConfirmResult::Yes => tool_write_diff(path, content),
                ConfirmResult::No => Ok("user declined the write operation".to_string()),
                ConfirmResult::Edit(new_diff) => tool_write_diff(path, &new_diff),
            }
        }
        _ => {
            print_tool_header_detail(
                "write",
                &format!("{path} ({} bytes)", content.len()),
                "overwrite",
            );
            print_tool_preview(&render_write_preview(path, content));
            match confirm_tool_action(
                &format!("write to {YELLOW}{path}{RESET}"),
                None,
                context.auto_confirm,
            )? {
                ConfirmResult::Yes => tool_write(path, content),
                ConfirmResult::No => Ok("user declined the write operation".to_string()),
                ConfirmResult::Edit(new_content) => tool_write(path, &new_content),
            }
        }
    }
}

fn execute_bash_tool(call: &ToolCall, context: &ToolRuntimeContext<'_>) -> AppResult<String> {
    let command = call.arguments["command"]
        .as_str()
        .ok_or_else(|| AppError::new(EXIT_ARGS, "bash tool: missing 'command' argument"))?;
    print_tool_header("bash", command);
    match confirm_tool_action(
        &format!("execute: {YELLOW}{command}{RESET}"),
        Some(command),
        context.auto_confirm,
    )? {
        ConfirmResult::Yes => tool_bash(command),
        ConfirmResult::No => Ok("user declined the bash execution".to_string()),
        ConfirmResult::Edit(new_cmd) => tool_bash(&new_cmd),
    }
}

fn execute_grep_tool(call: &ToolCall, _context: &ToolRuntimeContext<'_>) -> AppResult<String> {
    let pattern = call.arguments["pattern"]
        .as_str()
        .ok_or_else(|| AppError::new(EXIT_ARGS, "grep tool: missing 'pattern' argument"))?;
    let path = call.arguments["path"]
        .as_str()
        .ok_or_else(|| AppError::new(EXIT_ARGS, "grep tool: missing 'path' argument"))?;
    let include = call.arguments["include"].as_str();
    print_tool_header("grep", &format!("/{pattern}/ in {path}"));
    tool_grep(pattern, path, include)
}

fn execute_fetch_tool(call: &ToolCall, context: &ToolRuntimeContext<'_>) -> AppResult<String> {
    let url = call.arguments["url"]
        .as_str()
        .ok_or_else(|| AppError::new(EXIT_ARGS, "fetch tool: missing 'url' argument"))?;
    print_tool_header("fetch", url);
    match confirm_tool_action(
        &format!("fetch {YELLOW}{url}{RESET}"),
        None,
        context.auto_confirm,
    )? {
        ConfirmResult::Yes => tool_fetch(url),
        ConfirmResult::No => Ok("user declined the fetch operation".to_string()),
        ConfirmResult::Edit(new_url) => tool_fetch(&new_url),
    }
}

fn execute_skills_list_tool(
    call: &ToolCall,
    context: &ToolRuntimeContext<'_>,
) -> AppResult<String> {
    let query = call.arguments["query"].as_str();
    print_tool_header("skills_list", query.unwrap_or("all skills"));
    tool_skills_list(query, context.config)
}

fn execute_skill_read_tool(call: &ToolCall, context: &ToolRuntimeContext<'_>) -> AppResult<String> {
    let name = call.arguments["name"]
        .as_str()
        .ok_or_else(|| AppError::new(EXIT_ARGS, "skill_read tool: missing 'name' argument"))?;
    let scope = call.arguments["scope"].as_str();
    print_tool_header("skill_read", name);
    tool_skill_read(name, scope, context.config)
}

// ─── Tool Implementations ───

fn tool_read(path: &str) -> AppResult<String> {
    std::fs::read_to_string(path)
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to read `{path}`: {err}")))
}

fn tool_write(path: &str, content: &str) -> AppResult<String> {
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|err| {
                AppError::new(
                    EXIT_ARGS,
                    format!("failed to create parent dirs for `{path}`: {err}"),
                )
            })?;
        }
    }
    std::fs::write(path, content)
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to write `{path}`: {err}")))?;
    Ok(format!("wrote {} bytes to {path}", content.len()))
}

fn tool_write_diff(path: &str, diff_content: &str) -> AppResult<String> {
    let original = std::fs::read_to_string(path).unwrap_or_default();
    let patched = apply_unified_diff(&original, diff_content)?;
    std::fs::write(path, &patched)
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to write `{path}`: {err}")))?;
    let (add, del) = count_diff_changes(diff_content);
    Ok(format!("patched {path} (+{add} -{del})"))
}

fn tool_bash(command: &str) -> AppResult<String> {
    let output = Command::new("bash")
        .arg("-c")
        .arg(command)
        .output()
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to execute bash: {err}")))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut result = String::new();
    if !stdout.is_empty() {
        result.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str("[stderr]\n");
        result.push_str(&stderr);
    }
    if !output.status.success() {
        result.push_str(&format!(
            "\n[exit code: {}]",
            output.status.code().unwrap_or(-1)
        ));
    }
    if result.is_empty() {
        result = "(no output)".to_string();
    }
    Ok(result)
}

fn tool_grep(pattern: &str, path: &str, include: Option<&str>) -> AppResult<String> {
    let max_results = 50;
    if !Path::new(path).exists() {
        return Err(AppError::new(EXIT_ARGS, format!("path not found: {path}")));
    }

    let output = Command::new("rg")
        .args(build_ripgrep_args(pattern, path, include))
        .output()
        .map_err(|err| {
            AppError::new(
                EXIT_ARGS,
                format!("failed to execute ripgrep (`rg`): {err}"),
            )
        })?;

    match output.status.code() {
        Some(0) | Some(1) => {}
        Some(_) | None => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let message = if stderr.is_empty() {
                "ripgrep search failed".to_string()
            } else {
                format!("ripgrep search failed: {stderr}")
            };
            return Err(AppError::new(EXIT_ARGS, message));
        }
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let (results, truncated) = parse_ripgrep_matches(&stdout, max_results)?;
    if results.is_empty() {
        Ok("no matches found".to_string())
    } else {
        let mut output = results.join("\n");
        if truncated {
            output.push_str(&format!("\n... (truncated at {max_results} results)"));
        }
        Ok(output)
    }
}

fn build_ripgrep_args(pattern: &str, path: &str, include: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "--json".to_string(),
        "--line-number".to_string(),
        "--color".to_string(),
        "never".to_string(),
        "--with-filename".to_string(),
        "--follow".to_string(),
        "--hidden".to_string(),
        "--no-messages".to_string(),
        "--glob".to_string(),
        "!.git/**".to_string(),
        "--glob".to_string(),
        "!node_modules/**".to_string(),
        "--glob".to_string(),
        "!target/**".to_string(),
    ];
    if let Some(include) = include {
        args.push("--glob".to_string());
        args.push(include.to_string());
    }
    args.push("--".to_string());
    args.push(pattern.to_string());
    args.push(path.to_string());
    args
}

fn parse_ripgrep_matches(stdout: &str, max_results: usize) -> AppResult<(Vec<String>, bool)> {
    let mut results = Vec::new();
    let mut truncated = false;

    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let raw: Value = serde_json::from_str(line).map_err(|err| {
            AppError::new(EXIT_ARGS, format!("failed to parse ripgrep output: {err}"))
        })?;
        if raw["type"].as_str() != Some("match") {
            continue;
        }
        let path = raw["data"]["path"]["text"].as_str().unwrap_or_default();
        let line_number = raw["data"]["line_number"].as_u64().unwrap_or(0);
        let text = raw["data"]["lines"]["text"]
            .as_str()
            .unwrap_or_default()
            .trim_end_matches(['\r', '\n']);

        if results.len() < max_results {
            results.push(format!("{path}:{line_number}: {text}"));
        } else {
            truncated = true;
        }
    }

    Ok((results, truncated))
}

fn tool_fetch(url: &str) -> AppResult<String> {
    let response = reqwest::blocking::get(url)
        .map_err(|err| AppError::new(EXIT_ARGS, format!("fetch failed: {err}")))?;

    let status = response.status();
    let body = response
        .text()
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to read response body: {err}")))?;

    let max_len = 32 * 1024; // 32KB
    let truncated = if body.len() > max_len {
        format!(
            "{}\n... (truncated at 32KB, total {} bytes)",
            &body[..max_len],
            body.len()
        )
    } else {
        body
    };

    Ok(format!("[HTTP {}]\n{}", status.as_u16(), truncated))
}

fn tool_skills_list(query: Option<&str>, config: &AppConfig) -> AppResult<String> {
    let mut entries = discover_skills(config)?;
    if let Some(query) = query {
        let query = query.to_ascii_lowercase();
        entries.retain(|entry| entry.name.to_ascii_lowercase().contains(&query));
    }
    if entries.is_empty() {
        return Ok("no skills found".to_string());
    }
    Ok(entries
        .into_iter()
        .map(format_skill_entry)
        .collect::<Vec<_>>()
        .join("\n"))
}

fn tool_skill_read(name: &str, scope: Option<&str>, config: &AppConfig) -> AppResult<String> {
    let entries = discover_skills(config)?;
    let entry = resolve_skill_entry(&entries, name, scope)?;
    let content = fs::read_to_string(&entry.path).map_err(|err| {
        AppError::new(
            EXIT_ARGS,
            format!("failed to read skill `{}`: {err}", entry.name),
        )
    })?;
    let rendered = if content.len() > MAX_SKILL_READ_BYTES {
        format!(
            "{}\n\n... (truncated at {} bytes, total {} bytes)",
            &content[..MAX_SKILL_READ_BYTES],
            MAX_SKILL_READ_BYTES,
            content.len()
        )
    } else {
        content
    };
    Ok(format!(
        "name={} scope={} path={}\n{}",
        entry.name,
        entry.scope,
        entry.path.display(),
        rendered
    ))
}

// ─── Unified Diff Patcher ───

fn apply_unified_diff(original: &str, diff: &str) -> AppResult<String> {
    let mut result: Vec<String> = original.lines().map(|s| s.to_string()).collect();

    // Parse hunks from diff
    let hunks = parse_diff_hunks(diff)?;

    // Apply hunks in reverse order to preserve line numbers
    let mut sorted_hunks = hunks;
    sorted_hunks.sort_by(|a, b| b.orig_start.cmp(&a.orig_start));

    for hunk in &sorted_hunks {
        let start = hunk.orig_start.saturating_sub(1); // 1-indexed to 0-indexed
        let end = (start + hunk.orig_count).min(result.len());

        let mut new_lines: Vec<String> = Vec::new();
        for line in &hunk.lines {
            match line.kind {
                DiffLineKind::Context | DiffLineKind::Add => {
                    new_lines.push(line.content.to_string());
                }
                DiffLineKind::Remove => {}
            }
        }

        result.splice(start..end, new_lines);
    }

    let mut output = result.join("\n");
    // Preserve trailing newline if original had one
    if original.ends_with('\n') {
        output.push('\n');
    }
    Ok(output)
}

struct DiffHunk<'a> {
    orig_start: usize,
    orig_count: usize,
    lines: Vec<DiffLine<'a>>,
}

struct DiffLine<'a> {
    kind: DiffLineKind,
    content: &'a str,
}

#[derive(Debug, PartialEq)]
enum DiffLineKind {
    Context,
    Add,
    Remove,
}

fn parse_diff_hunks(diff: &str) -> AppResult<Vec<DiffHunk<'_>>> {
    let mut hunks = Vec::new();
    let lines: Vec<&str> = diff.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        // Look for @@ -start,count +start,count @@
        if line.starts_with("@@") {
            let (orig_start, orig_count) = parse_hunk_header(line)?;
            i += 1;

            let mut hunk_lines = Vec::new();
            while i < lines.len() && !lines[i].starts_with("@@") && !lines[i].starts_with("diff ") {
                let hline = lines[i];
                if let Some(content) = hline.strip_prefix('+') {
                    hunk_lines.push(DiffLine {
                        kind: DiffLineKind::Add,
                        content,
                    });
                } else if let Some(content) = hline.strip_prefix('-') {
                    hunk_lines.push(DiffLine {
                        kind: DiffLineKind::Remove,
                        content,
                    });
                } else if let Some(content) = hline.strip_prefix(' ') {
                    hunk_lines.push(DiffLine {
                        kind: DiffLineKind::Context,
                        content,
                    });
                } else if !hline.starts_with('\\') {
                    // Treat as context
                    hunk_lines.push(DiffLine {
                        kind: DiffLineKind::Context,
                        content: hline,
                    });
                }
                i += 1;
            }

            hunks.push(DiffHunk {
                orig_start,
                orig_count,
                lines: hunk_lines,
            });
        } else {
            i += 1;
        }
    }

    if hunks.is_empty() {
        return Err(AppError::new(
            EXIT_ARGS,
            "no valid diff hunks found in content",
        ));
    }

    Ok(hunks)
}

fn parse_hunk_header(line: &str) -> AppResult<(usize, usize)> {
    // @@ -start,count +start,count @@
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 3 {
        return Err(AppError::new(
            EXIT_ARGS,
            format!("invalid hunk header: {line}"),
        ));
    }
    let orig_part = parts[1]; // -start,count
    let orig = orig_part.trim_start_matches('-');
    let (start, count) = if let Some((s, c)) = orig.split_once(',') {
        (
            s.parse::<usize>().unwrap_or(1),
            c.parse::<usize>().unwrap_or(0),
        )
    } else {
        (orig.parse::<usize>().unwrap_or(1), 1)
    };
    Ok((start, count))
}

fn count_diff_changes(diff: &str) -> (usize, usize) {
    let mut additions = 0;
    let mut deletions = 0;
    for line in diff.lines() {
        if line.starts_with('+') && !line.starts_with("+++") {
            additions += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            deletions += 1;
        }
    }
    (additions, deletions)
}

// ─── UI Helpers ───

fn print_tool_header(name: &str, detail: &str) {
    eprintln!("  {BOLD}{CYAN}{name}{RESET} {YELLOW}{detail}{RESET}");
}

fn print_tool_header_detail(name: &str, detail: &str, mode: &str) {
    eprintln!("  {BOLD}{CYAN}{name}{RESET} {DIM}({mode}){RESET} {detail}");
}

fn print_tool_preview(rendered: &str) {
    for line in rendered.lines() {
        eprintln!("  {line}");
    }
}

fn render_write_preview(path: &str, content: &str) -> String {
    let mut output = String::new();
    output.push_str(&format!("{DIM}{CYAN}╭─ Preview {path}{RESET}\n"));
    if content.is_empty() {
        output.push_str(&format!("{DIM}{CYAN}│{RESET} {DIM}(empty file){RESET}\n"));
    } else {
        if let Some(lang) = preview_language(path) {
            output.push_str(&format!("{DIM}{CYAN}│{RESET} {DIM}{lang}{RESET}\n"));
        }
        for line in content.split('\n') {
            output.push_str(&format!("{DIM}{CYAN}│{RESET} {DIM}{line}{RESET}\n"));
        }
    }
    output.push_str(&format!(
        "{DIM}{CYAN}╰─ {} lines · {} bytes{RESET}",
        preview_line_count(content),
        content.len()
    ));
    output
}

fn render_diff_preview(path: &str, diff: &str) -> String {
    let mut output = String::new();
    output.push_str(&format!("{DIM}{CYAN}╭─ Diff Preview {path}{RESET}\n"));
    if diff.is_empty() {
        output.push_str(&format!("{DIM}{CYAN}│{RESET} {DIM}(empty diff){RESET}\n"));
    } else {
        for line in diff.split('\n') {
            let styled = if line.starts_with('+') && !line.starts_with("+++") {
                format!("{GREEN}{line}{RESET}")
            } else if line.starts_with('-') && !line.starts_with("---") {
                format!("{RED}{line}{RESET}")
            } else if line.starts_with("@@")
                || line.starts_with("diff ")
                || line.starts_with("index ")
                || line.starts_with("---")
                || line.starts_with("+++")
            {
                format!("{CYAN}{line}{RESET}")
            } else {
                format!("{DIM}{line}{RESET}")
            };
            output.push_str(&format!("{DIM}{CYAN}│{RESET} {styled}\n"));
        }
    }
    let (additions, deletions) = count_diff_changes(diff);
    output.push_str(&format!(
        "{DIM}{CYAN}╰─ +{additions} -{deletions} · {} lines{RESET}",
        preview_line_count(diff)
    ));
    output
}

/// Interactive tool confirmation with y/n/e(dit) options.
/// `editable_content` is shown to the user when they choose to edit.
fn confirm_tool_action(
    description: &str,
    editable_content: Option<&str>,
    auto_confirm: bool,
) -> AppResult<ConfirmResult> {
    if auto_confirm {
        return Ok(ConfirmResult::Yes);
    }

    loop {
        if editable_content.is_some() {
            eprint!(
                "  {DIM}{description} {GREEN}y{RESET}{DIM}/{RED}n{RESET}{DIM}/or type replacement:{RESET} "
            );
        } else {
            eprint!("  {DIM}{description} {GREEN}y{RESET}{DIM}/{RED}n{RESET}{DIM}:{RESET} ");
        }
        io::stderr()
            .flush()
            .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to flush stderr: {err}")))?;

        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to read input: {err}")))?;
        let trimmed = input.trim();
        let lower = trimmed.to_lowercase();

        match lower.as_str() {
            "y" | "yes" | "" => return Ok(ConfirmResult::Yes),
            "n" | "no" => return Ok(ConfirmResult::No),
            _ => {
                if editable_content.is_some() {
                    // Any other input is treated as replacement content
                    return Ok(ConfirmResult::Edit(trimmed.to_string()));
                } else {
                    eprintln!("  {DIM}please enter y or n{RESET}");
                }
            }
        }
    }
}

// ─── Utilities ───

fn discover_skills(config: &AppConfig) -> AppResult<Vec<SkillEntry>> {
    discover_skills_from_roots(&skill_roots(config))
}

fn skill_roots(config: &AppConfig) -> Vec<(String, PathBuf)> {
    let mut roots = Vec::new();
    let current_dir = std::env::current_dir().ok();
    let home_dir = std::env::var_os("HOME").map(PathBuf::from);
    for configured in &config.skills.paths {
        let expanded = expand_tilde(configured);
        let root = if expanded.is_relative() {
            current_dir
                .as_ref()
                .map(|cwd| cwd.join(&expanded))
                .unwrap_or(expanded.clone())
        } else {
            expanded.clone()
        };
        let scope = skill_scope_label(
            &root,
            current_dir.as_deref(),
            home_dir.as_deref(),
            configured,
        );
        roots.push((scope, root));
    }
    roots
}

fn skill_scope_label(
    root: &Path,
    current_dir: Option<&Path>,
    home_dir: Option<&Path>,
    configured: &str,
) -> String {
    if current_dir.is_some_and(|cwd| root == cwd.join(".claude").join("skills")) {
        return "project".to_string();
    }
    if home_dir.is_some_and(|home| root == home.join(".claude").join("skills")) {
        return "global".to_string();
    }
    format!("path:{}", configured)
}

fn discover_skills_from_roots(roots: &[(String, PathBuf)]) -> AppResult<Vec<SkillEntry>> {
    let mut entries = Vec::new();
    for (scope, root) in roots {
        collect_skills(root, root, scope, &mut entries)?;
    }
    entries.sort_by(|a, b| a.scope.cmp(&b.scope).then(a.name.cmp(&b.name)));
    Ok(entries)
}

fn collect_skills(
    root: &Path,
    dir: &Path,
    scope: &str,
    entries: &mut Vec<SkillEntry>,
) -> AppResult<()> {
    if !dir.exists() {
        return Ok(());
    }
    let metadata = fs::metadata(dir).map_err(|err| {
        AppError::new(
            EXIT_ARGS,
            format!("failed to inspect skills dir `{}`: {err}", dir.display()),
        )
    })?;
    if !metadata.is_dir() {
        return Ok(());
    }

    let skill_file = dir.join(SKILL_FILE_NAME);
    if skill_file.is_file() {
        if let Ok(relative) = dir.strip_prefix(root) {
            let name = relative.to_string_lossy().replace('\\', "/");
            if !name.is_empty() {
                entries.push(SkillEntry {
                    name,
                    scope: scope.to_string(),
                    summary: read_skill_summary(&skill_file),
                    path: skill_file,
                });
            }
        }
        return Ok(());
    }

    for entry in fs::read_dir(dir).map_err(|err| {
        AppError::new(
            EXIT_ARGS,
            format!("failed to read skills dir `{}`: {err}", dir.display()),
        )
    })? {
        let entry = entry.map_err(|err| {
            AppError::new(
                EXIT_ARGS,
                format!(
                    "failed to read skills dir entry in `{}`: {err}",
                    dir.display()
                ),
            )
        })?;
        collect_skills(root, &entry.path(), scope, entries)?;
    }
    Ok(())
}

fn read_skill_summary(path: &Path) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    extract_skill_summary(&content)
}

fn extract_skill_summary(content: &str) -> Option<String> {
    let mut saw_heading = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('#') {
            saw_heading = true;
            continue;
        }
        let summary = trimmed.to_string();
        return Some(if summary.len() > 120 {
            format!("{}...", &summary[..117])
        } else {
            summary
        });
    }
    if saw_heading {
        Some(String::new())
    } else {
        None
    }
}

fn format_skill_entry(entry: SkillEntry) -> String {
    let summary = entry
        .summary
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    if summary.is_empty() {
        format!(
            "{}:{} path={}",
            entry.scope,
            entry.name,
            entry.path.display()
        )
    } else {
        format!(
            "{}:{} path={} summary={}",
            entry.scope,
            entry.name,
            entry.path.display(),
            serde_json::to_string(&summary).unwrap_or_else(|_| "\"\"".to_string())
        )
    }
}

fn resolve_skill_entry<'a>(
    entries: &'a [SkillEntry],
    name: &str,
    scope: Option<&str>,
) -> AppResult<&'a SkillEntry> {
    let (scope_from_name, bare_name) = if let Some((scope_prefix, name)) = name.split_once(':') {
        (Some(scope_prefix), name)
    } else {
        (None, name)
    };
    let wanted_scope = scope.or(scope_from_name);
    let matches = entries
        .iter()
        .filter(|entry| wanted_scope.is_none_or(|value| value == entry.scope))
        .filter(|entry| entry.name == bare_name)
        .collect::<Vec<_>>();

    match matches.len() {
        1 => Ok(matches[0]),
        0 => Err(AppError::new(
            EXIT_ARGS,
            format!("skill `{bare_name}` was not found"),
        )),
        _ => Err(AppError::new(
            EXIT_ARGS,
            format!(
                "skill `{bare_name}` is ambiguous, matches: {}",
                matches
                    .iter()
                    .map(|entry| format!("{}:{}", entry.scope, entry.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
    }
}

fn preview_line_count(content: &str) -> usize {
    if content.is_empty() {
        0
    } else {
        content.matches('\n').count() + 1
    }
}

fn preview_language(path: &str) -> Option<&'static str> {
    let ext = Path::new(path).extension()?.to_str()?;
    match ext {
        "rs" => Some("rust"),
        "py" => Some("python"),
        "js" => Some("javascript"),
        "ts" => Some("typescript"),
        "tsx" => Some("tsx"),
        "jsx" => Some("jsx"),
        "sh" => Some("shell"),
        "bash" => Some("bash"),
        "zsh" => Some("zsh"),
        "json" => Some("json"),
        "toml" => Some("toml"),
        "yaml" | "yml" => Some("yaml"),
        "md" => Some("markdown"),
        "html" => Some("html"),
        "css" => Some("css"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn build_ripgrep_args_includes_expected_flags() {
        let args = build_ripgrep_args("foo", "src", Some("*.rs"));
        assert!(args.contains(&"--json".to_string()));
        assert!(args.contains(&"--hidden".to_string()));
        assert!(args.contains(&"--follow".to_string()));
        assert!(args.contains(&"*.rs".to_string()));
        assert!(args.contains(&"!.git/**".to_string()));
        assert_eq!(args.last().map(String::as_str), Some("src"));
    }

    #[test]
    fn parse_ripgrep_matches_formats_match_lines() {
        let stdout = concat!(
            "{\"type\":\"begin\",\"data\":{\"path\":{\"text\":\"src/tool.rs\"}}}\n",
            "{\"type\":\"match\",\"data\":{\"path\":{\"text\":\"src/tool.rs\"},\"lines\":{\"text\":\"let value = 1;\\n\"},\"line_number\":42}}\n",
            "{\"type\":\"summary\",\"data\":{\"elapsed_total\":{\"human\":\"0.01s\"}}}\n"
        );
        let (results, truncated) = parse_ripgrep_matches(stdout, 50).unwrap();
        assert_eq!(results, vec!["src/tool.rs:42: let value = 1;"]);
        assert!(!truncated);
    }

    #[test]
    fn parse_ripgrep_matches_marks_truncation() {
        let stdout = concat!(
            "{\"type\":\"match\",\"data\":{\"path\":{\"text\":\"a.rs\"},\"lines\":{\"text\":\"first\\n\"},\"line_number\":1}}\n",
            "{\"type\":\"match\",\"data\":{\"path\":{\"text\":\"b.rs\"},\"lines\":{\"text\":\"second\\n\"},\"line_number\":2}}\n"
        );
        let (results, truncated) = parse_ripgrep_matches(stdout, 1).unwrap();
        assert_eq!(results, vec!["a.rs:1: first"]);
        assert!(truncated);
    }

    #[test]
    fn discover_skills_from_roots_finds_project_and_global_skills() {
        let temp_root = make_temp_dir("skills-scan");
        let project_root = temp_root.join("project");
        let global_root = temp_root.join("global");
        fs::create_dir_all(project_root.join(".claude/skills/local-skill")).unwrap();
        fs::create_dir_all(global_root.join(".claude/skills/.system/agent-browser")).unwrap();
        fs::write(
            project_root.join(".claude/skills/local-skill/SKILL.md"),
            "# Local Skill\n\nLocal summary",
        )
        .unwrap();
        fs::write(
            global_root.join(".claude/skills/.system/agent-browser/SKILL.md"),
            "# Agent Browser\n\nHeadless browser automation",
        )
        .unwrap();

        let roots = vec![
            ("project".to_string(), project_root.join(".claude/skills")),
            ("global".to_string(), global_root.join(".claude/skills")),
        ];
        let entries = discover_skills_from_roots(&roots).unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, ".system/agent-browser");
        assert_eq!(entries[1].name, "local-skill");

        let _ = fs::remove_dir_all(temp_root);
    }

    #[test]
    fn resolve_skill_entry_supports_scoped_lookup() {
        let entries = vec![
            SkillEntry {
                name: "dup".to_string(),
                scope: "project".to_string(),
                path: PathBuf::from("/tmp/project/SKILL.md"),
                summary: None,
            },
            SkillEntry {
                name: "dup".to_string(),
                scope: "global".to_string(),
                path: PathBuf::from("/tmp/global/SKILL.md"),
                summary: None,
            },
        ];

        let project = resolve_skill_entry(&entries, "project:dup", None).unwrap();
        assert_eq!(project.scope, "project");

        let global = resolve_skill_entry(&entries, "dup", Some("global")).unwrap();
        assert_eq!(global.scope, "global");

        let err = resolve_skill_entry(&entries, "dup", None).unwrap_err();
        assert!(err.message.contains("ambiguous"));
    }

    #[test]
    fn lookup_tool_spec_exposes_registry_metadata() {
        let grep = lookup_tool_spec("grep").unwrap();
        assert_eq!(grep.side_effects, ToolSideEffects::ReadOnly);
        assert!(grep.is_parallel_safe());
        assert!(!grep.requires_confirmation);

        let fetch = lookup_tool_spec("fetch").unwrap();
        assert_eq!(fetch.side_effects, ToolSideEffects::External);
        assert!(fetch.requires_confirmation);
    }

    fn make_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("chat-cli-{label}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}

use crate::error::{AppError, AppResult, EXIT_ARGS};
use regex::Regex;
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::process::Command;
use walkdir::WalkDir;

// ANSI codes for tool UI
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";
const CYAN: &str = "\x1b[36m";
const YELLOW: &str = "\x1b[33m";
const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";

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

/// Confirmation result from the user.
#[derive(Debug)]
enum ConfirmResult {
    Yes,
    No,
    Edit(String),
}

/// Returns OpenAI-compatible tool definitions.
pub fn tool_definitions() -> Vec<Value> {
    vec![
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
        }),
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
        }),
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
        }),
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
        }),
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
        }),
    ]
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
    let arguments_str = raw["function"]["arguments"]
        .as_str()
        .unwrap_or("{}");
    let arguments: Value = serde_json::from_str(arguments_str).unwrap_or(json!({}));
    Ok(ToolCall {
        id,
        name,
        arguments,
    })
}

/// Execute a tool call with optional user confirmation.
pub fn execute_tool(call: &ToolCall, auto_confirm: bool) -> AppResult<ToolResult> {
    let content = match call.name.as_str() {
        "read" => {
            let path = call.arguments["path"]
                .as_str()
                .ok_or_else(|| AppError::new(EXIT_ARGS, "read tool: missing 'path' argument"))?;
            print_tool_header("read", path);
            tool_read(path)?
        }
        "write" => {
            let path = call.arguments["path"]
                .as_str()
                .ok_or_else(|| AppError::new(EXIT_ARGS, "write tool: missing 'path' argument"))?;
            let content = call.arguments["content"]
                .as_str()
                .ok_or_else(|| {
                    AppError::new(EXIT_ARGS, "write tool: missing 'content' argument")
                })?;
            let mode = call.arguments["mode"]
                .as_str()
                .unwrap_or("overwrite");

            match mode {
                "diff" => {
                    let (additions, deletions) = count_diff_changes(content);
                    print_tool_header_detail(
                        "write",
                        &format!("{path} {GREEN}+{additions}{RESET} {RED}-{deletions}{RESET}"),
                        "diff",
                    );
                    match confirm_tool_action(
                        &format!("apply diff to {YELLOW}{path}{RESET}"),
                        Some(content),
                        auto_confirm,
                    )? {
                        ConfirmResult::Yes => tool_write_diff(path, content)?,
                        ConfirmResult::No => "user declined the write operation".to_string(),
                        ConfirmResult::Edit(new_diff) => tool_write_diff(path, &new_diff)?,
                    }
                }
                _ => {
                    print_tool_header_detail(
                        "write",
                        &format!("{path} ({} bytes)", content.len()),
                        "overwrite",
                    );
                    match confirm_tool_action(
                        &format!("write to {YELLOW}{path}{RESET}"),
                        None,
                        auto_confirm,
                    )? {
                        ConfirmResult::Yes => tool_write(path, content)?,
                        ConfirmResult::No => "user declined the write operation".to_string(),
                        ConfirmResult::Edit(new_content) => tool_write(path, &new_content)?,
                    }
                }
            }
        }
        "bash" => {
            let command = call.arguments["command"]
                .as_str()
                .ok_or_else(|| AppError::new(EXIT_ARGS, "bash tool: missing 'command' argument"))?;
            print_tool_header("bash", command);
            match confirm_tool_action(
                &format!("execute: {YELLOW}{command}{RESET}"),
                Some(command),
                auto_confirm,
            )? {
                ConfirmResult::Yes => tool_bash(command)?,
                ConfirmResult::No => "user declined the bash execution".to_string(),
                ConfirmResult::Edit(new_cmd) => tool_bash(&new_cmd)?,
            }
        }
        "grep" => {
            let pattern = call.arguments["pattern"]
                .as_str()
                .ok_or_else(|| AppError::new(EXIT_ARGS, "grep tool: missing 'pattern' argument"))?;
            let path = call.arguments["path"]
                .as_str()
                .ok_or_else(|| AppError::new(EXIT_ARGS, "grep tool: missing 'path' argument"))?;
            let include = call.arguments["include"].as_str();
            print_tool_header("grep", &format!("/{pattern}/ in {path}"));
            tool_grep(pattern, path, include)?
        }
        "fetch" => {
            let url = call.arguments["url"]
                .as_str()
                .ok_or_else(|| AppError::new(EXIT_ARGS, "fetch tool: missing 'url' argument"))?;
            print_tool_header("fetch", url);
            match confirm_tool_action(
                &format!("fetch {YELLOW}{url}{RESET}"),
                None,
                auto_confirm,
            )? {
                ConfirmResult::Yes => tool_fetch(url)?,
                ConfirmResult::No => "user declined the fetch operation".to_string(),
                ConfirmResult::Edit(new_url) => tool_fetch(&new_url)?,
            }
        }
        other => {
            format!("error: unknown tool '{other}'")
        }
    };
    Ok(ToolResult {
        tool_call_id: call.id.clone(),
        content,
    })
}

// ─── Tool Implementations ───

fn tool_read(path: &str) -> AppResult<String> {
    std::fs::read_to_string(path).map_err(|err| {
        AppError::new(EXIT_ARGS, format!("failed to read `{path}`: {err}"))
    })
}

fn tool_write(path: &str, content: &str) -> AppResult<String> {
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|err| {
                AppError::new(EXIT_ARGS, format!("failed to create parent dirs for `{path}`: {err}"))
            })?;
        }
    }
    std::fs::write(path, content).map_err(|err| {
        AppError::new(EXIT_ARGS, format!("failed to write `{path}`: {err}"))
    })?;
    Ok(format!("wrote {} bytes to {path}", content.len()))
}

fn tool_write_diff(path: &str, diff_content: &str) -> AppResult<String> {
    let original = std::fs::read_to_string(path).unwrap_or_default();
    let patched = apply_unified_diff(&original, diff_content)?;
    std::fs::write(path, &patched).map_err(|err| {
        AppError::new(EXIT_ARGS, format!("failed to write `{path}`: {err}"))
    })?;
    let (add, del) = count_diff_changes(diff_content);
    Ok(format!("patched {path} (+{add} -{del})"))
}

fn tool_bash(command: &str) -> AppResult<String> {
    let output = Command::new("bash")
        .arg("-c")
        .arg(command)
        .output()
        .map_err(|err| {
            AppError::new(EXIT_ARGS, format!("failed to execute bash: {err}"))
        })?;

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
        result.push_str(&format!("\n[exit code: {}]", output.status.code().unwrap_or(-1)));
    }
    if result.is_empty() {
        result = "(no output)".to_string();
    }
    Ok(result)
}

fn tool_grep(pattern: &str, path: &str, include: Option<&str>) -> AppResult<String> {
    let re = Regex::new(pattern).map_err(|err| {
        AppError::new(EXIT_ARGS, format!("invalid regex pattern: {err}"))
    })?;

    let glob_pattern = include.and_then(|g| glob_to_regex(g));
    let mut results = Vec::new();
    let max_results = 50;

    let p = Path::new(path);
    if p.is_file() {
        grep_file(&re, p, &mut results, max_results)?;
    } else if p.is_dir() {
        for entry in WalkDir::new(path)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if results.len() >= max_results {
                break;
            }
            let ep = entry.path();
            if !ep.is_file() {
                continue;
            }
            // Skip hidden dirs and common non-text dirs
            let path_str = ep.to_string_lossy();
            if path_str.contains("/.git/")
                || path_str.contains("/node_modules/")
                || path_str.contains("/target/")
            {
                continue;
            }
            // Apply include filter
            if let Some(ref glob_re) = glob_pattern {
                let name = ep.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
                if !glob_re.is_match(&name) {
                    continue;
                }
            }
            grep_file(&re, ep, &mut results, max_results)?;
        }
    } else {
        return Err(AppError::new(EXIT_ARGS, format!("path not found: {path}")));
    }

    if results.is_empty() {
        Ok("no matches found".to_string())
    } else {
        let count = results.len();
        let mut output = results.join("\n");
        if count >= max_results {
            output.push_str(&format!("\n... (truncated at {max_results} results)"));
        }
        Ok(output)
    }
}

fn grep_file(re: &Regex, path: &Path, results: &mut Vec<String>, max: usize) -> AppResult<()> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Ok(()), // Skip unreadable files
    };
    let reader = io::BufReader::new(file);
    let path_str = path.display();
    for (line_num, line) in reader.lines().enumerate() {
        if results.len() >= max {
            break;
        }
        let line = match line {
            Ok(l) => l,
            Err(_) => break, // Binary file or read error
        };
        if re.is_match(&line) {
            results.push(format!("{path_str}:{}: {line}", line_num + 1));
        }
    }
    Ok(())
}

fn tool_fetch(url: &str) -> AppResult<String> {
    let response = reqwest::blocking::get(url).map_err(|err| {
        AppError::new(EXIT_ARGS, format!("fetch failed: {err}"))
    })?;

    let status = response.status();
    let body = response.text().map_err(|err| {
        AppError::new(EXIT_ARGS, format!("failed to read response body: {err}"))
    })?;

    let max_len = 32 * 1024; // 32KB
    let truncated = if body.len() > max_len {
        format!("{}\n... (truncated at 32KB, total {} bytes)", &body[..max_len], body.len())
    } else {
        body
    };

    Ok(format!("[HTTP {}]\n{}", status.as_u16(), truncated))
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
                    hunk_lines.push(DiffLine { kind: DiffLineKind::Add, content });
                } else if let Some(content) = hline.strip_prefix('-') {
                    hunk_lines.push(DiffLine { kind: DiffLineKind::Remove, content });
                } else if let Some(content) = hline.strip_prefix(' ') {
                    hunk_lines.push(DiffLine { kind: DiffLineKind::Context, content });
                } else if !hline.starts_with('\\') {
                    // Treat as context
                    hunk_lines.push(DiffLine { kind: DiffLineKind::Context, content: hline });
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
        return Err(AppError::new(EXIT_ARGS, "no valid diff hunks found in content"));
    }

    Ok(hunks)
}

fn parse_hunk_header(line: &str) -> AppResult<(usize, usize)> {
    // @@ -start,count +start,count @@
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 3 {
        return Err(AppError::new(EXIT_ARGS, format!("invalid hunk header: {line}")));
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
            eprint!("  {DIM}{description} ({GREEN}y{DIM})es / ({RED}n{DIM})o / ({YELLOW}e{DIM})dit{RESET} > ");
        } else {
            eprint!("  {DIM}{description} ({GREEN}y{DIM})es / ({RED}n{DIM})o{RESET} > ");
        }
        io::stderr()
            .flush()
            .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to flush stderr: {err}")))?;

        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to read input: {err}")))?;
        let answer = input.trim().to_lowercase();

        match answer.as_str() {
            "y" | "yes" | "" => return Ok(ConfirmResult::Yes),
            "n" | "no" => return Ok(ConfirmResult::No),
            "e" | "edit" => {
                if editable_content.is_some() {
                    eprint!("  {DIM}enter new content (single line):{RESET} ");
                    io::stderr().flush().map_err(|err| {
                        AppError::new(EXIT_ARGS, format!("failed to flush stderr: {err}"))
                    })?;
                    let mut new_content = String::new();
                    io::stdin().read_line(&mut new_content).map_err(|err| {
                        AppError::new(EXIT_ARGS, format!("failed to read input: {err}"))
                    })?;
                    let trimmed = new_content.trim().to_string();
                    if trimmed.is_empty() {
                        eprintln!("  {DIM}empty input, cancelled{RESET}");
                        return Ok(ConfirmResult::No);
                    }
                    return Ok(ConfirmResult::Edit(trimmed));
                } else {
                    eprintln!("  {DIM}edit not available for this action{RESET}");
                }
            }
            _ => {
                eprintln!("  {DIM}please enter y, n{}{RESET}",
                    if editable_content.is_some() { ", or e" } else { "" });
            }
        }
    }
}

// ─── Utilities ───

/// Convert a simple glob pattern like "*.rs" to a regex.
fn glob_to_regex(glob: &str) -> Option<Regex> {
    let mut regex_str = String::from("^");
    for c in glob.chars() {
        match c {
            '*' => regex_str.push_str(".*"),
            '?' => regex_str.push('.'),
            '.' => regex_str.push_str("\\."),
            c => regex_str.push(c),
        }
    }
    regex_str.push('$');
    Regex::new(&regex_str).ok()
}

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
const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const BRIGHT_GREEN: &str = "\x1b[38;5;151m";
const BRIGHT_RED: &str = "\x1b[38;5;224m";
const GUTTER: &str = "\x1b[38;5;245m";
const ADD_BG: &str = "\x1b[48;5;22m";
const REMOVE_BG: &str = "\x1b[48;5;52m";
const SKILL_FILE_NAME: &str = "SKILL.md";
const MAX_SKILL_READ_BYTES: usize = 64 * 1024;
const MAX_READ_LINES: usize = 2000;
const MAX_EDIT_FILE_SIZE_BYTES: u64 = 1024 * 1024 * 1024;
const TOOL_NAME_WIDTH: usize = 12;
const PREVIEW_MAX_CHARS: usize = 120;
const DIFF_PREVIEW_MAX_LINES: usize = 8;
const DIFF_CONTEXT_LINES: usize = 1;
const BASH_SEARCH_COMMANDS: &[&str] = &[
    "find", "grep", "rg", "ag", "ack", "locate", "which", "whereis",
];
const BASH_READ_COMMANDS: &[&str] = &[
    "cat", "head", "tail", "less", "more", "wc", "stat", "file", "strings", "jq", "awk", "cut",
    "sort", "uniq", "tr", "pwd", "date", "uname", "whoami", "id", "ps", "env", "printenv",
];
const BASH_LIST_COMMANDS: &[&str] = &["ls", "tree", "du"];
const BASH_NEUTRAL_COMMANDS: &[&str] = &["echo", "printf", "true", "false", ":"];

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
    pub aliases: &'static [&'static str],
    #[allow(dead_code)]
    pub description: &'static str,
    pub search_hint: &'static str,
    pub side_effects: ToolSideEffects,
    pub parallelism: ToolParallelism,
    pub requires_confirmation: bool,
    pub defer_loading: bool,
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
    No(Option<String>),
    Edit(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SkillEntry {
    name: String,
    scope: String,
    path: PathBuf,
    summary: Option<String>,
}

const BUILTIN_TOOL_HANDLERS: [ToolHandler; 9] = [
    ToolHandler {
        spec: ToolSpec {
            name: "ToolSearch",
            aliases: &["tool_search"],
            description: "",
            search_hint: "search deferred tools",
            side_effects: ToolSideEffects::ReadOnly,
            parallelism: ToolParallelism::ParallelSafe,
            requires_confirmation: false,
            defer_loading: false,
            definition: define_tool_search_tool,
        },
        execute: execute_tool_search_tool,
    },
    ToolHandler {
        spec: ToolSpec {
            name: "Read",
            aliases: &["read"],
            description: "Read a file from the local filesystem.",
            search_hint: "read file contents",
            side_effects: ToolSideEffects::ReadOnly,
            parallelism: ToolParallelism::ParallelSafe,
            requires_confirmation: false,
            defer_loading: true,
            definition: define_read_tool,
        },
        execute: execute_read_tool,
    },
    ToolHandler {
        spec: ToolSpec {
            name: "Edit",
            aliases: &["edit", "write", "Write"],
            description: "Create a new file or edit an existing file by replacing old text with new text.",
            search_hint: "create or modify file contents",
            side_effects: ToolSideEffects::Mutating,
            parallelism: ToolParallelism::SequentialOnly,
            requires_confirmation: true,
            defer_loading: true,
            definition: define_edit_tool,
        },
        execute: execute_edit_tool,
    },
    ToolHandler {
        spec: ToolSpec {
            name: "Bash",
            aliases: &["bash"],
            description: "Run a shell command.",
            search_hint: "execute shell commands",
            side_effects: ToolSideEffects::Mutating,
            parallelism: ToolParallelism::SequentialOnly,
            requires_confirmation: true,
            defer_loading: true,
            definition: define_bash_tool,
        },
        execute: execute_bash_tool,
    },
    ToolHandler {
        spec: ToolSpec {
            name: "Grep",
            aliases: &["grep"],
            description: "Search for a pattern in files.",
            search_hint: "search file contents by pattern",
            side_effects: ToolSideEffects::ReadOnly,
            parallelism: ToolParallelism::ParallelSafe,
            requires_confirmation: false,
            defer_loading: true,
            definition: define_grep_tool,
        },
        execute: execute_grep_tool,
    },
    ToolHandler {
        spec: ToolSpec {
            name: "Glob",
            aliases: &["glob"],
            description: "Find files by glob pattern.",
            search_hint: "find files by wildcard",
            side_effects: ToolSideEffects::ReadOnly,
            parallelism: ToolParallelism::ParallelSafe,
            requires_confirmation: false,
            defer_loading: true,
            definition: define_glob_tool,
        },
        execute: execute_glob_tool,
    },
    ToolHandler {
        spec: ToolSpec {
            name: "WebFetch",
            aliases: &["fetch", "web_fetch"],
            description: "Fetch content from a URL via HTTP GET.",
            search_hint: "fetch web page contents",
            side_effects: ToolSideEffects::External,
            parallelism: ToolParallelism::ParallelSafe,
            requires_confirmation: false,
            defer_loading: true,
            definition: define_fetch_tool,
        },
        execute: execute_fetch_tool,
    },
    ToolHandler {
        spec: ToolSpec {
            name: "SkillsList",
            aliases: &["skills_list"],
            description: "List available project and global skills.",
            search_hint: "list available skills",
            side_effects: ToolSideEffects::ReadOnly,
            parallelism: ToolParallelism::ParallelSafe,
            requires_confirmation: false,
            defer_loading: true,
            definition: define_skills_list_tool,
        },
        execute: execute_skills_list_tool,
    },
    ToolHandler {
        spec: ToolSpec {
            name: "SkillRead",
            aliases: &["skill_read"],
            description: "Read the SKILL.md content for a named skill.",
            search_hint: "read skill instructions",
            side_effects: ToolSideEffects::ReadOnly,
            parallelism: ToolParallelism::ParallelSafe,
            requires_confirmation: false,
            defer_loading: true,
            definition: define_skill_read_tool,
        },
        execute: execute_skill_read_tool,
    },
];

fn deferred_tool_names() -> Vec<&'static str> {
    builtin_tool_handlers()
        .iter()
        .filter(|handler| handler.spec.defer_loading)
        .map(|handler| handler.spec.name)
        .collect()
}

fn define_tool_search_tool() -> Value {
    let available = deferred_tool_names().join(", ");
    json!({
        "type": "function",
        "function": {
            "name": "ToolSearch",
            "description": format!("Search deferred tools and load their schemas for later turns. Available tool names: {available}. Use this first before calling any deferred tool."),
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Tool name or capability phrase to search for."
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum number of tool schemas to load. Default 5."
                    }
                },
                "required": ["query"]
            }
        }
    })
}

fn define_read_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "Read",
            "description": "Read a file from the local filesystem.",
            "parameters": {
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "Absolute file path to the file to read."
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Optional 1-based line number to start reading from."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Optional maximum number of lines to read."
                    }
                },
                "required": ["file_path"]
            }
        }
    })
}

fn define_edit_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "Edit",
            "description": "Create a new file or edit an existing file by replacing old text with new text. Use old_string=\"\" when creating a new file or populating an empty file.",
            "parameters": {
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "Absolute file path to create or edit."
                    },
                    "old_string": {
                        "type": "string",
                        "description": "Exact text to replace. Use an empty string to create a new file or populate an empty file."
                    },
                    "new_string": {
                        "type": "string",
                        "description": "Replacement text, or the full file contents when creating a file."
                    },
                    "replace_all": {
                        "type": "boolean",
                        "description": "Replace all matches instead of exactly one."
                    }
                },
                "required": ["file_path", "new_string"]
            }
        }
    })
}

fn define_bash_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "Bash",
            "description": "Run a shell command and return its stdout and stderr.",
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
            "name": "Grep",
            "description": "Search for a regex pattern in files.",
            "parameters": {
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Regex pattern to search for."
                    },
                    "path": {
                        "type": "string",
                        "description": "Optional file or directory to search in. Defaults to the current working directory."
                    },
                    "glob": {
                        "type": "string",
                        "description": "Optional glob pattern to filter files, e.g. '*.rs', '*.py'."
                    },
                    "output_mode": {
                        "type": "string",
                        "enum": ["content", "files_with_matches", "count"],
                        "description": "Search output mode. Defaults to files_with_matches."
                    }
                },
                "required": ["pattern"]
            }
        }
    })
}

fn define_glob_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "Glob",
            "description": "Find files by glob pattern.",
            "parameters": {
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern such as **/*.rs or src/*.ts."
                    },
                    "path": {
                        "type": "string",
                        "description": "Optional base directory to search in."
                    }
                },
                "required": ["pattern"]
            }
        }
    })
}

fn define_fetch_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "WebFetch",
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
            "name": "SkillsList",
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
            "name": "SkillRead",
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

fn progressive_loading_enabled(config: &AppConfig) -> bool {
    config.tools.progressive_loading.unwrap_or(true)
}

fn full_tool_definitions() -> Vec<Value> {
    builtin_tool_handlers()
        .iter()
        .filter(|handler| handler.spec.name != "ToolSearch")
        .map(|handler| (handler.spec.definition)())
        .collect()
}

fn find_tool_handler(name: &str) -> Option<&'static ToolHandler> {
    builtin_tool_handlers()
        .iter()
        .find(|handler| handler.spec.name == name || handler.spec.aliases.contains(&name))
}

pub fn lookup_tool_spec(name: &str) -> Option<&'static ToolSpec> {
    find_tool_handler(name).map(|handler| &handler.spec)
}

pub fn tool_call_side_effects(call: &ToolCall) -> ToolSideEffects {
    if matches!(call.name.as_str(), "Bash" | "bash")
        && call.arguments["command"]
            .as_str()
            .is_some_and(is_read_only_bash_command)
    {
        return ToolSideEffects::ReadOnly;
    }
    lookup_tool_spec(&call.name)
        .map(|spec| spec.side_effects)
        .unwrap_or(ToolSideEffects::Mutating)
}

pub fn tool_call_requires_confirmation(call: &ToolCall) -> bool {
    if matches!(tool_call_side_effects(call), ToolSideEffects::ReadOnly) {
        return false;
    }
    lookup_tool_spec(&call.name)
        .map(|spec| spec.requires_confirmation)
        .unwrap_or(true)
}

pub fn initial_tool_definitions(config: &AppConfig) -> Vec<Value> {
    if !progressive_loading_enabled(config) {
        return full_tool_definitions();
    }
    builtin_tool_handlers()
        .iter()
        .filter(|handler| handler.spec.name == "ToolSearch")
        .map(|handler| (handler.spec.definition)())
        .collect()
}

pub fn tool_definitions_for_names(config: &AppConfig, names: &[String]) -> Vec<Value> {
    if !progressive_loading_enabled(config) {
        return full_tool_definitions();
    }
    let mut tools = initial_tool_definitions(config);
    for name in names {
        if let Some(handler) = find_tool_handler(name)
            && handler.spec.defer_loading
        {
            tools.push((handler.spec.definition)());
        }
    }
    tools
}

pub fn tool_search_matches(query: &str, max_results: usize) -> Vec<&'static ToolSpec> {
    let query = query.trim().to_ascii_lowercase();
    let mut matches = builtin_tool_handlers()
        .iter()
        .filter(|handler| handler.spec.defer_loading)
        .map(|handler| {
            let mut score = 0usize;
            if handler.spec.name.to_ascii_lowercase() == query {
                score += 100;
            }
            if handler.spec.name.to_ascii_lowercase().contains(&query) {
                score += 50;
            }
            if handler
                .spec
                .aliases
                .iter()
                .any(|alias| alias.to_ascii_lowercase().contains(&query))
            {
                score += 25;
            }
            if handler
                .spec
                .search_hint
                .to_ascii_lowercase()
                .contains(&query)
            {
                score += 10;
            }
            (score, &handler.spec)
        })
        .filter(|(score, _)| *score > 0)
        .collect::<Vec<_>>();
    matches.sort_by(|(left_score, left_spec), (right_score, right_spec)| {
        right_score
            .cmp(left_score)
            .then_with(|| left_spec.name.cmp(right_spec.name))
    });
    matches
        .into_iter()
        .take(max_results.max(1))
        .map(|(_, spec)| spec)
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

fn execute_tool_search_tool(
    call: &ToolCall,
    _context: &ToolRuntimeContext<'_>,
) -> AppResult<String> {
    let query = call.arguments["query"]
        .as_str()
        .ok_or_else(|| AppError::new(EXIT_ARGS, "ToolSearch: missing 'query' argument"))?;
    let max_results = call.arguments["max_results"].as_u64().unwrap_or(5) as usize;
    let matches = tool_search_matches(query, max_results);
    if matches.is_empty() {
        return Ok("no matching tools found".to_string());
    }
    let results = matches
        .into_iter()
        .filter_map(|spec| {
            find_tool_handler(spec.name).map(|handler| {
                json!({
                    "name": spec.name,
                    "description": spec.description,
                    "aliases": spec.aliases,
                    "side_effects": match spec.side_effects {
                        ToolSideEffects::ReadOnly => "read_only",
                        ToolSideEffects::Mutating => "mutating",
                        ToolSideEffects::External => "external",
                    },
                    "schema": (handler.spec.definition)(),
                })
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string_pretty(&json!({
        "loaded_tools": results.iter().map(|item| item["name"].clone()).collect::<Vec<_>>(),
        "results": results,
    }))
    .map_err(|err| {
        AppError::new(
            EXIT_ARGS,
            format!("failed to render ToolSearch results: {err}"),
        )
    })
}

fn execute_read_tool(call: &ToolCall, _context: &ToolRuntimeContext<'_>) -> AppResult<String> {
    let path = call.arguments["file_path"]
        .as_str()
        .or_else(|| call.arguments["path"].as_str())
        .ok_or_else(|| AppError::new(EXIT_ARGS, "Read: missing 'file_path' argument"))?;
    let normalized = normalize_tool_path(path);
    let offset = call.arguments["offset"]
        .as_u64()
        .map(|value| value as usize);
    let limit = call.arguments["limit"].as_u64().map(|value| value as usize);
    print_tool_header("Read", &normalized);
    tool_read(&normalized, offset, limit)
}

fn execute_edit_tool(call: &ToolCall, context: &ToolRuntimeContext<'_>) -> AppResult<String> {
    let path = call.arguments["file_path"]
        .as_str()
        .or_else(|| call.arguments["path"].as_str())
        .ok_or_else(|| AppError::new(EXIT_ARGS, "Edit: missing 'file_path' argument"))?;
    let normalized = normalize_tool_path(path);
    let old_string = call.arguments["old_string"].as_str().unwrap_or("");
    let new_string = call.arguments["new_string"]
        .as_str()
        .or_else(|| call.arguments["content"].as_str())
        .ok_or_else(|| AppError::new(EXIT_ARGS, "Edit: missing 'new_string' argument"))?;
    let replace_all = call.arguments["replace_all"].as_bool().unwrap_or(false);

    let diff = build_edit_preview(&normalized, old_string, new_string, replace_all)?;
    let (additions, deletions) = count_diff_changes(&diff);
    let display_path = display_tool_path(&normalized);
    let mode = if old_string.is_empty() {
        "create"
    } else {
        "replace"
    };
    let action = if old_string.is_empty() {
        "create"
    } else {
        "edit"
    };
    print_tool_header_detail(
        "Edit",
        &format!("{display_path} (+{additions} -{deletions})"),
        mode,
    );
    print_tool_preview(&render_diff_preview(&normalized, &diff));
    match confirm_tool_action(action, Some(new_string), context.auto_confirm)? {
        ConfirmResult::Yes => tool_edit(&normalized, old_string, new_string, replace_all),
        ConfirmResult::No(None) => Ok("user declined the edit operation".to_string()),
        ConfirmResult::No(Some(feedback)) => Ok(format!(
            "user declined the edit operation. user feedback: {feedback}"
        )),
        ConfirmResult::Edit(replacement) => {
            tool_edit(&normalized, old_string, &replacement, replace_all)
        }
    }
}

fn execute_bash_tool(call: &ToolCall, context: &ToolRuntimeContext<'_>) -> AppResult<String> {
    let command = call.arguments["command"]
        .as_str()
        .ok_or_else(|| AppError::new(EXIT_ARGS, "Bash: missing 'command' argument"))?;
    print_tool_header("Bash", &truncate_preview(command, 120));
    let auto_confirm = context.auto_confirm || is_read_only_bash_command(command);
    match confirm_tool_action("run", Some(command), auto_confirm)? {
        ConfirmResult::Yes => tool_bash(command),
        ConfirmResult::No(None) => Ok("user declined the bash execution".to_string()),
        ConfirmResult::No(Some(feedback)) => Ok(format!(
            "user declined the bash execution. user feedback: {feedback}"
        )),
        ConfirmResult::Edit(new_cmd) => tool_bash(&new_cmd),
    }
}

fn execute_grep_tool(call: &ToolCall, _context: &ToolRuntimeContext<'_>) -> AppResult<String> {
    let pattern = call.arguments["pattern"]
        .as_str()
        .ok_or_else(|| AppError::new(EXIT_ARGS, "Grep: missing 'pattern' argument"))?;
    let path = call.arguments["path"].as_str().unwrap_or(".");
    let normalized = normalize_tool_path(path);
    let include = call.arguments["glob"]
        .as_str()
        .or_else(|| call.arguments["include"].as_str());
    let output_mode = call.arguments["output_mode"]
        .as_str()
        .unwrap_or("files_with_matches");
    print_tool_header("Grep", &format!("/{pattern}/ in {normalized}"));
    tool_grep(pattern, &normalized, include, output_mode)
}

fn execute_glob_tool(call: &ToolCall, _context: &ToolRuntimeContext<'_>) -> AppResult<String> {
    let pattern = call.arguments["pattern"]
        .as_str()
        .ok_or_else(|| AppError::new(EXIT_ARGS, "Glob: missing 'pattern' argument"))?;
    let path = normalize_tool_path(call.arguments["path"].as_str().unwrap_or("."));
    print_tool_header("Glob", &format!("{pattern} in {path}"));
    tool_glob(pattern, &path)
}

fn execute_fetch_tool(call: &ToolCall, _context: &ToolRuntimeContext<'_>) -> AppResult<String> {
    let url = call.arguments["url"]
        .as_str()
        .ok_or_else(|| AppError::new(EXIT_ARGS, "WebFetch: missing 'url' argument"))?;
    print_tool_header("WebFetch", url);
    tool_fetch(url)
}

fn execute_skills_list_tool(
    call: &ToolCall,
    context: &ToolRuntimeContext<'_>,
) -> AppResult<String> {
    let query = call.arguments["query"].as_str();
    print_tool_header("SkillsList", query.unwrap_or("all skills"));
    tool_skills_list(query, context.config)
}

fn execute_skill_read_tool(call: &ToolCall, context: &ToolRuntimeContext<'_>) -> AppResult<String> {
    let name = call.arguments["name"]
        .as_str()
        .ok_or_else(|| AppError::new(EXIT_ARGS, "skill_read tool: missing 'name' argument"))?;
    let scope = call.arguments["scope"].as_str();
    print_tool_header("SkillRead", name);
    tool_skill_read(name, scope, context.config)
}

// ─── Tool Implementations ───

fn tool_read(path: &str, offset: Option<usize>, limit: Option<usize>) -> AppResult<String> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to read `{path}`: {err}")))?;
    let lines = text.lines().collect::<Vec<_>>();
    let start = offset.unwrap_or(1).max(1);
    let effective_limit = limit.unwrap_or(MAX_READ_LINES).min(MAX_READ_LINES);
    let end = Some(effective_limit)
        .map(|limit| start.saturating_sub(1) + limit)
        .unwrap_or(lines.len());
    let numbered = lines
        .iter()
        .enumerate()
        .skip(start.saturating_sub(1))
        .take(end.saturating_sub(start.saturating_sub(1)))
        .map(|(index, line)| format!("{:>6}\t{}", index + 1, line))
        .collect::<Vec<_>>();
    let mut output = numbered.join("\n");
    if lines.len() > end {
        output.push_str(&format!(
            "\n... (truncated at {} lines; use offset and limit to read more)",
            effective_limit
        ));
    }
    Ok(output)
}

fn write_file_contents(path: &str, content: &str) -> AppResult<()> {
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
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to write `{path}`: {err}")))
}

fn build_edit_preview(
    path: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> AppResult<String> {
    validate_edit_inputs(path, old_string, new_string)?;
    if !Path::new(path).exists() {
        if !old_string.is_empty() {
            return Err(AppError::new(
                EXIT_ARGS,
                format!("Edit: `{path}` does not exist; use old_string=\"\" to create a new file"),
            ));
        }
        return Ok(build_full_file_diff(path, "", new_string));
    }
    let original = std::fs::read_to_string(path)
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to read `{path}`: {err}")))?;
    if old_string.is_empty() {
        if !original.trim().is_empty() {
            return Err(AppError::new(
                EXIT_ARGS,
                format!(
                    "Edit: cannot create new file at `{path}` because it already exists; read it first and replace exact text"
                ),
            ));
        }
        return Ok(build_full_file_diff(path, "", new_string));
    }
    let occurrences = original.matches(old_string).count();
    if occurrences == 0 {
        return Err(AppError::new(
            EXIT_ARGS,
            format!("Edit: old_string was not found in `{path}`"),
        ));
    }
    if !replace_all && occurrences > 1 {
        return Err(AppError::new(
            EXIT_ARGS,
            format!("Edit: old_string matched {occurrences} times; set replace_all=true"),
        ));
    }
    let match_offsets = original
        .match_indices(old_string)
        .map(|(offset, _)| offset)
        .collect::<Vec<_>>();
    let selected_offsets = if replace_all {
        match_offsets
    } else {
        match_offsets.into_iter().take(1).collect()
    };
    Ok(build_edit_preview_diff(
        path,
        &original,
        &selected_offsets,
        old_string,
        new_string,
    ))
}

fn tool_edit(
    path: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> AppResult<String> {
    validate_edit_inputs(path, old_string, new_string)?;
    if !Path::new(path).exists() {
        if !old_string.is_empty() {
            return Err(AppError::new(
                EXIT_ARGS,
                format!("Edit: `{path}` does not exist; use old_string=\"\" to create a new file"),
            ));
        }
        write_file_contents(path, new_string)?;
        return Ok(format!("created `{path}`"));
    }
    let original = std::fs::read_to_string(path)
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to read `{path}`: {err}")))?;
    if old_string.is_empty() {
        if !original.trim().is_empty() {
            return Err(AppError::new(
                EXIT_ARGS,
                format!(
                    "Edit: cannot create new file at `{path}` because it already exists; read it first and replace exact text"
                ),
            ));
        }
        write_file_contents(path, new_string)?;
        return Ok(format!("created `{path}`"));
    }
    let occurrences = original.matches(old_string).count();
    if occurrences == 0 {
        return Err(AppError::new(
            EXIT_ARGS,
            format!("Edit: old_string was not found in `{path}`"),
        ));
    }
    if !replace_all && occurrences > 1 {
        return Err(AppError::new(
            EXIT_ARGS,
            format!("Edit: old_string matched {occurrences} times; set replace_all=true"),
        ));
    }
    let updated = if replace_all {
        original.replace(old_string, new_string)
    } else {
        original.replacen(old_string, new_string, 1)
    };
    write_file_contents(path, &updated)?;
    Ok(format!("edited `{path}`"))
}

fn validate_edit_inputs(path: &str, old_string: &str, new_string: &str) -> AppResult<()> {
    if old_string == new_string {
        return Err(AppError::new(
            EXIT_ARGS,
            "Edit: old_string and new_string are identical",
        ));
    }
    if path.ends_with(".ipynb") {
        return Err(AppError::new(
            EXIT_ARGS,
            "Edit: notebook files are not supported by this tool",
        ));
    }
    match std::fs::metadata(path) {
        Ok(metadata) => {
            if metadata.len() > MAX_EDIT_FILE_SIZE_BYTES {
                return Err(AppError::new(
                    EXIT_ARGS,
                    format!(
                        "Edit: file exceeds max editable size ({} bytes)",
                        MAX_EDIT_FILE_SIZE_BYTES
                    ),
                ));
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(AppError::new(
                EXIT_ARGS,
                format!("failed to inspect `{path}`: {err}"),
            ));
        }
    }
    Ok(())
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

fn tool_glob(pattern: &str, path: &str) -> AppResult<String> {
    let max_results = 100usize;
    let root = Path::new(path);
    if !root.exists() {
        return Err(AppError::new(EXIT_ARGS, format!("path not found: {path}")));
    }
    let regex = glob_to_regex(pattern)?;
    let mut matches = walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| {
            let display = entry.path().to_string_lossy().replace('\\', "/");
            regex.is_match(&display).then_some(display)
        })
        .collect::<Vec<_>>();
    matches.sort();
    if matches.is_empty() {
        Ok("no matches found".to_string())
    } else {
        let truncated = matches.len() > max_results;
        let mut output = matches
            .into_iter()
            .take(max_results)
            .collect::<Vec<_>>()
            .join("\n");
        if truncated {
            output.push_str(&format!("\n... (truncated at {max_results} results)"));
        }
        Ok(output)
    }
}

fn tool_grep(
    pattern: &str,
    path: &str,
    include: Option<&str>,
    output_mode: &str,
) -> AppResult<String> {
    let max_results = 50;
    if !Path::new(path).exists() {
        return Err(AppError::new(EXIT_ARGS, format!("path not found: {path}")));
    }

    match output_mode {
        "content" => {
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
        "files_with_matches" => {
            let mut args = build_ripgrep_common_args(path, include);
            args.push("-l".to_string());
            args.push("--".to_string());
            args.push(pattern.to_string());
            args.push(path.to_string());
            let output = Command::new("rg").args(args).output().map_err(|err| {
                AppError::new(
                    EXIT_ARGS,
                    format!("failed to execute ripgrep (`rg`): {err}"),
                )
            })?;
            let stdout = handle_ripgrep_plain_output(output, "ripgrep search failed")?;
            let mut files = stdout
                .lines()
                .filter(|line| !line.trim().is_empty())
                .take(max_results + 1)
                .map(|line| line.to_string())
                .collect::<Vec<_>>();
            let truncated = files.len() > max_results;
            files.truncate(max_results);
            if files.is_empty() {
                Ok("no matches found".to_string())
            } else {
                let mut rendered = files.join("\n");
                if truncated {
                    rendered.push_str(&format!("\n... (truncated at {max_results} results)"));
                }
                Ok(rendered)
            }
        }
        "count" => {
            let mut args = build_ripgrep_common_args(path, include);
            args.push("-c".to_string());
            args.push("--".to_string());
            args.push(pattern.to_string());
            args.push(path.to_string());
            let output = Command::new("rg").args(args).output().map_err(|err| {
                AppError::new(
                    EXIT_ARGS,
                    format!("failed to execute ripgrep (`rg`): {err}"),
                )
            })?;
            let stdout = handle_ripgrep_plain_output(output, "ripgrep count failed")?;
            let lines = stdout
                .lines()
                .filter(|line| !line.trim().is_empty())
                .take(max_results + 1)
                .collect::<Vec<_>>();
            if lines.is_empty() {
                Ok("no matches found".to_string())
            } else {
                let truncated = lines.len() > max_results;
                let mut rendered = lines[..lines.len().min(max_results)].join("\n");
                if truncated {
                    rendered.push_str(&format!("\n... (truncated at {max_results} results)"));
                }
                Ok(rendered)
            }
        }
        other => Err(AppError::new(
            EXIT_ARGS,
            format!("unsupported Grep output_mode `{other}`"),
        )),
    }
}

fn handle_ripgrep_plain_output(
    output: std::process::Output,
    failure_label: &str,
) -> AppResult<String> {
    match output.status.code() {
        Some(0) | Some(1) => {}
        Some(_) | None => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let message = if stderr.is_empty() {
                failure_label.to_string()
            } else {
                format!("{failure_label}: {stderr}")
            };
            return Err(AppError::new(EXIT_ARGS, message));
        }
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn build_ripgrep_args(pattern: &str, path: &str, include: Option<&str>) -> Vec<String> {
    let mut args = build_ripgrep_common_args(path, include);
    args.push("--json".to_string());
    args.push("--line-number".to_string());
    args.push("--".to_string());
    args.push(pattern.to_string());
    args.push(path.to_string());
    args
}

fn build_ripgrep_common_args(path: &str, include: Option<&str>) -> Vec<String> {
    let mut args = vec![
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
    let _ = path;
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

struct DiffHunk<'a> {
    orig_start: usize,
    orig_count: usize,
    new_start: usize,
    new_count: usize,
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
            let (orig_start, orig_count, new_start, new_count) = parse_hunk_header(line)?;
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
                new_start,
                new_count,
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

fn parse_hunk_header(line: &str) -> AppResult<(usize, usize, usize, usize)> {
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
    let (orig_start, orig_count) = if let Some((s, c)) = orig.split_once(',') {
        (
            s.parse::<usize>().unwrap_or(1),
            c.parse::<usize>().unwrap_or(0),
        )
    } else {
        (orig.parse::<usize>().unwrap_or(1), 1)
    };
    let new_part = parts[2]; // +start,count
    let new = new_part.trim_start_matches('+');
    let (new_start, new_count) = if let Some((s, c)) = new.split_once(',') {
        (
            s.parse::<usize>().unwrap_or(1),
            c.parse::<usize>().unwrap_or(0),
        )
    } else {
        (new.parse::<usize>().unwrap_or(1), 1)
    };
    Ok((orig_start, orig_count, new_start, new_count))
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

fn glob_to_regex(pattern: &str) -> AppResult<regex::Regex> {
    let mut regex = String::from("^");
    let mut chars = pattern.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '*' => {
                if chars.peek() == Some(&'*') {
                    chars.next();
                    regex.push_str(".*");
                } else {
                    regex.push_str("[^/]*");
                }
            }
            '?' => regex.push('.'),
            '.' => regex.push_str("\\."),
            '/' => regex.push('/'),
            other if "+()[]{}^$|\\".contains(other) => {
                regex.push('\\');
                regex.push(other);
            }
            other => regex.push(other),
        }
    }
    regex.push('$');
    regex::Regex::new(&regex).map_err(|err| {
        AppError::new(
            EXIT_ARGS,
            format!("invalid glob pattern `{pattern}`: {err}"),
        )
    })
}

// ─── UI Helpers ───

fn print_tool_header(name: &str, detail: &str) {
    let label = format_tool_label(name, None);
    eprintln!("  {BOLD}{CYAN}{label}{RESET}{detail}");
}

fn print_tool_header_detail(name: &str, detail: &str, mode: &str) {
    let label = format_tool_label(name, Some(mode));
    eprintln!("  {BOLD}{CYAN}{label}{RESET}{detail}");
}

fn print_tool_preview(rendered: &str) {
    for line in rendered.lines() {
        eprintln!("    {line}");
    }
}

fn render_diff_preview(path: &str, diff: &str) -> String {
    let hunks = match parse_diff_hunks(diff) {
        Ok(hunks) => hunks,
        Err(_) => return render_fallback_diff_preview(path, diff),
    };
    let max_line_number = hunks
        .iter()
        .flat_map(|hunk| {
            [
                hunk.orig_start.saturating_add(hunk.orig_count),
                hunk.new_start.saturating_add(hunk.new_count),
            ]
        })
        .max()
        .unwrap_or(1);
    let gutter_width = max_line_number.max(1).to_string().len();
    let mut output = Vec::new();
    let total_lines = hunks.iter().map(|hunk| hunk.lines.len()).sum::<usize>();
    let mut shown = 0usize;
    let mut truncated = false;

    'hunks: for (index, hunk) in hunks.iter().enumerate() {
        if index > 0 && shown < DIFF_PREVIEW_MAX_LINES {
            output.push(format!("{DIM}...{RESET}"));
        }
        let mut old_line = hunk.orig_start;
        let mut new_line = hunk.new_start;
        for line in &hunk.lines {
            if shown >= DIFF_PREVIEW_MAX_LINES {
                truncated = true;
                break 'hunks;
            }
            match line.kind {
                DiffLineKind::Context => {
                    output.push(format_diff_preview_line(
                        Some(old_line),
                        ' ',
                        line.content,
                        gutter_width,
                        None,
                        Some(DIM),
                    ));
                    old_line += 1;
                    new_line += 1;
                }
                DiffLineKind::Remove => {
                    output.push(format_diff_preview_line(
                        Some(old_line),
                        '-',
                        line.content,
                        gutter_width,
                        Some((REMOVE_BG, BRIGHT_RED)),
                        Some(GUTTER),
                    ));
                    old_line += 1;
                }
                DiffLineKind::Add => {
                    output.push(format_diff_preview_line(
                        Some(new_line),
                        '+',
                        line.content,
                        gutter_width,
                        Some((ADD_BG, BRIGHT_GREEN)),
                        Some(GUTTER),
                    ));
                    new_line += 1;
                }
            }
            shown += 1;
        }
    }
    if truncated || total_lines > shown {
        output.push(format!("{DIM}...{RESET}"));
    }
    if output.is_empty() {
        return render_fallback_diff_preview(path, diff);
    }
    output.join("\n")
}

fn render_fallback_diff_preview(path: &str, diff: &str) -> String {
    let mut output = vec![format!(
        "{DIM}{} · diff preview unavailable{RESET}",
        display_tool_path(path)
    )];
    let mut shown = 0usize;
    for line in diff.lines() {
        if shown >= DIFF_PREVIEW_MAX_LINES {
            break;
        }
        if line.starts_with('+') && !line.starts_with("+++") {
            output.push(format!(
                "{GREEN}+ {}{RESET}",
                truncate_preview(line.trim_start_matches('+'), PREVIEW_MAX_CHARS)
            ));
            shown += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            output.push(format!(
                "{RED}- {}{RESET}",
                truncate_preview(line.trim_start_matches('-'), PREVIEW_MAX_CHARS)
            ));
            shown += 1;
        }
    }
    output.join("\n")
}

fn format_diff_preview_line(
    line_number: Option<usize>,
    marker: char,
    content: &str,
    gutter_width: usize,
    highlight: Option<(&str, &str)>,
    gutter_style: Option<&str>,
) -> String {
    let line_label = line_number
        .map(|value| format!("{value:>gutter_width$}"))
        .unwrap_or_else(|| " ".repeat(gutter_width));
    let content = truncate_code_preview(content, PREVIEW_MAX_CHARS);
    match highlight {
        Some((background, foreground)) => format!(
            "{background}{}{line_label} {foreground}{marker} {content}{RESET}",
            gutter_style.unwrap_or(foreground)
        ),
        None => format!(
            "{}{line_label} {marker} {content}{RESET}",
            gutter_style.unwrap_or(DIM)
        ),
    }
}

fn build_edit_preview_diff(
    path: &str,
    original: &str,
    match_offsets: &[usize],
    old_string: &str,
    new_string: &str,
) -> String {
    let display_path = display_tool_path(path);
    let original_lines = original.lines().collect::<Vec<_>>();
    let line_starts = line_start_offsets(original);
    let mut diff = format!("--- {display_path}\n+++ {display_path}\n");
    let mut line_delta = 0isize;

    for &match_start in match_offsets {
        let match_end = match_start + old_string.len();
        let start_line = line_index_for_offset(&line_starts, match_start);
        let end_line = line_index_for_offset(&line_starts, match_end.saturating_sub(1));
        let context_start = start_line.saturating_sub(DIFF_CONTEXT_LINES);
        let context_end = (end_line + DIFF_CONTEXT_LINES + 1).min(original_lines.len());
        let block_start = line_starts[start_line];
        let block_end = line_end_offset(original, &line_starts, end_line);
        let block = &original[block_start..block_end];
        let local_start = match_start.saturating_sub(block_start);
        let local_end = local_start + old_string.len();
        let updated_block = format!(
            "{}{}{}",
            &block[..local_start],
            new_string,
            &block[local_end..]
        );
        let block_lines = block.lines().collect::<Vec<_>>();
        let updated_block_lines = updated_block.lines().collect::<Vec<_>>();
        let diff_lines = diff_line_sequences(&block_lines, &updated_block_lines);
        let orig_start = context_start + 1;
        let new_start = (orig_start as isize + line_delta).max(1) as usize;
        let orig_count = (start_line - context_start)
            + block_lines.len()
            + context_end.saturating_sub(end_line + 1);
        let new_count = (start_line - context_start)
            + updated_block_lines.len()
            + context_end.saturating_sub(end_line + 1);
        diff.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            orig_start, orig_count, new_start, new_count
        ));
        for line in &original_lines[context_start..start_line] {
            push_diff_line(&mut diff, ' ', line);
        }
        for (kind, content) in diff_lines {
            push_diff_line(
                &mut diff,
                match kind {
                    DiffLineKind::Context => ' ',
                    DiffLineKind::Add => '+',
                    DiffLineKind::Remove => '-',
                },
                &content,
            );
        }
        for line in &original_lines[end_line + 1..context_end] {
            push_diff_line(&mut diff, ' ', line);
        }
        line_delta += updated_block_lines.len() as isize - block_lines.len() as isize;
    }

    diff
}

fn build_full_file_diff(path: &str, original: &str, updated: &str) -> String {
    let display_path = display_tool_path(path);
    let original_lines = if original.is_empty() {
        Vec::new()
    } else {
        original.lines().collect::<Vec<_>>()
    };
    let updated_lines = if updated.is_empty() {
        Vec::new()
    } else {
        updated.lines().collect::<Vec<_>>()
    };
    let mut diff = format!("--- {display_path}\n+++ {display_path}\n");
    diff.push_str(&format!(
        "@@ -1,{} +1,{} @@\n",
        original_lines.len(),
        updated_lines.len()
    ));
    for (kind, content) in diff_line_sequences(&original_lines, &updated_lines) {
        push_diff_line(
            &mut diff,
            match kind {
                DiffLineKind::Context => ' ',
                DiffLineKind::Add => '+',
                DiffLineKind::Remove => '-',
            },
            &content,
        );
    }
    diff
}

fn push_diff_line(output: &mut String, prefix: char, content: &str) {
    output.push(prefix);
    output.push_str(content);
    output.push('\n');
}

fn diff_line_sequences<'a>(
    old_lines: &[&'a str],
    new_lines: &[&'a str],
) -> Vec<(DiffLineKind, String)> {
    let mut lcs = vec![vec![0usize; new_lines.len() + 1]; old_lines.len() + 1];
    for old_index in (0..old_lines.len()).rev() {
        for new_index in (0..new_lines.len()).rev() {
            lcs[old_index][new_index] = if old_lines[old_index] == new_lines[new_index] {
                lcs[old_index + 1][new_index + 1] + 1
            } else {
                lcs[old_index + 1][new_index].max(lcs[old_index][new_index + 1])
            };
        }
    }

    let mut diff = Vec::new();
    let mut old_index = 0usize;
    let mut new_index = 0usize;
    while old_index < old_lines.len() && new_index < new_lines.len() {
        if old_lines[old_index] == new_lines[new_index] {
            diff.push((DiffLineKind::Context, old_lines[old_index].to_string()));
            old_index += 1;
            new_index += 1;
        } else if lcs[old_index + 1][new_index] >= lcs[old_index][new_index + 1] {
            diff.push((DiffLineKind::Remove, old_lines[old_index].to_string()));
            old_index += 1;
        } else {
            diff.push((DiffLineKind::Add, new_lines[new_index].to_string()));
            new_index += 1;
        }
    }
    while old_index < old_lines.len() {
        diff.push((DiffLineKind::Remove, old_lines[old_index].to_string()));
        old_index += 1;
    }
    while new_index < new_lines.len() {
        diff.push((DiffLineKind::Add, new_lines[new_index].to_string()));
        new_index += 1;
    }
    diff
}

fn display_tool_path(path: &str) -> String {
    let candidate = Path::new(path);
    std::env::current_dir()
        .ok()
        .and_then(|cwd| candidate.strip_prefix(&cwd).ok().map(Path::to_path_buf))
        .unwrap_or_else(|| candidate.to_path_buf())
        .to_string_lossy()
        .to_string()
}

fn line_start_offsets(content: &str) -> Vec<usize> {
    let mut offsets = vec![0];
    for (index, byte) in content.bytes().enumerate() {
        if byte == b'\n' && index + 1 < content.len() {
            offsets.push(index + 1);
        }
    }
    offsets
}

fn line_index_for_offset(line_starts: &[usize], offset: usize) -> usize {
    match line_starts.binary_search(&offset) {
        Ok(index) => index,
        Err(0) => 0,
        Err(index) => index.saturating_sub(1),
    }
}

fn line_end_offset(content: &str, line_starts: &[usize], line_index: usize) -> usize {
    line_starts
        .get(line_index + 1)
        .copied()
        .unwrap_or(content.len())
}

/// Interactive tool confirmation with y/n/e(dit) options.
/// `editable_content` is shown to the user when they choose to edit.
fn parse_confirm_input(input: &str, editable_content: Option<&str>) -> Option<ConfirmResult> {
    let trimmed = input.trim();
    let lower = trimmed.to_lowercase();
    match lower.as_str() {
        "y" | "yes" | "" => Some(ConfirmResult::Yes),
        "n" | "no" => Some(ConfirmResult::No(None)),
        _ => {
            if editable_content.is_none() {
                return None;
            }
            if lower == "edit" || lower == "e" {
                return None;
            }
            if lower.starts_with("edit ") {
                let replacement = trimmed[4..].trim();
                if !replacement.is_empty() {
                    return Some(ConfirmResult::Edit(replacement.to_string()));
                }
                return None;
            }
            if lower.starts_with("e ") {
                let replacement = trimmed[1..].trim();
                if !replacement.is_empty() {
                    return Some(ConfirmResult::Edit(replacement.to_string()));
                }
                return None;
            }
            Some(ConfirmResult::No(Some(trimmed.to_string())))
        }
    }
}

fn confirm_tool_action(
    action: &str,
    editable_content: Option<&str>,
    auto_confirm: bool,
) -> AppResult<ConfirmResult> {
    if auto_confirm {
        return Ok(ConfirmResult::Yes);
    }

    loop {
        if editable_content.is_some() {
            eprint!("    {DIM}{action}? {GREEN}y{RESET}{DIM}/{RED}n{RESET}{DIM}/edit:{RESET} ");
        } else {
            eprint!("    {DIM}{action}? {GREEN}y{RESET}{DIM}/{RED}n{RESET}{DIM}:{RESET} ");
        }
        io::stderr()
            .flush()
            .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to flush stderr: {err}")))?;

        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to read input: {err}")))?;
        if let Some(result) = parse_confirm_input(&input, editable_content) {
            return Ok(result);
        }

        if editable_content.is_some() {
            eprintln!(
                "    {DIM}请输入 y / n，或使用 `edit <替换内容>`；其他文本会作为反馈返回给模型{RESET}"
            );
        } else {
            eprintln!("    {DIM}please enter y or n{RESET}");
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

fn format_tool_label(name: &str, mode: Option<&str>) -> String {
    let label = mode.map_or_else(|| name.to_string(), |mode| format!("{name}:{mode}"));
    format!("{label:<TOOL_NAME_WIDTH$}")
}

fn truncate_preview(value: &str, max_chars: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        normalized
    } else {
        format!(
            "{}...",
            normalized.chars().take(max_chars).collect::<String>()
        )
    }
}

fn truncate_code_preview(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value.to_string()
    } else {
        format!("{}...", value.chars().take(max_chars).collect::<String>())
    }
}

fn normalize_tool_path(path: &str) -> String {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        candidate.to_string_lossy().to_string()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(candidate))
            .unwrap_or_else(|_| candidate.to_path_buf())
            .to_string_lossy()
            .to_string()
    }
}

fn split_command_segments(command: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let chars = command.chars().collect::<Vec<_>>();
    let mut i = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        let next = chars.get(i + 1).copied();
        if matches!(ch, ';' | '|')
            || (ch == '&' && next == Some('&'))
            || (ch == '|' && next == Some('|'))
        {
            if !current.trim().is_empty() {
                parts.push(current.trim().to_string());
            }
            current.clear();
            if (ch == '&' || ch == '|') && next == Some(ch) {
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        current.push(ch);
        i += 1;
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }
    parts
}

fn command_base_name(segment: &str) -> Option<&str> {
    segment.split_whitespace().next()
}

fn is_read_only_bash_command(command: &str) -> bool {
    let segments = split_command_segments(command);
    if segments.is_empty() {
        return false;
    }

    let mut has_non_neutral = false;
    for segment in segments {
        if segment.contains('>') || segment.contains(">>") {
            return false;
        }
        let Some(base) = command_base_name(&segment) else {
            continue;
        };
        if BASH_NEUTRAL_COMMANDS.contains(&base) {
            continue;
        }
        has_non_neutral = true;
        if BASH_SEARCH_COMMANDS.contains(&base)
            || BASH_READ_COMMANDS.contains(&base)
            || BASH_LIST_COMMANDS.contains(&base)
        {
            continue;
        }
        if base == "git" {
            let sub = segment.split_whitespace().nth(1).unwrap_or_default();
            if matches!(
                sub,
                "status" | "diff" | "show" | "log" | "branch" | "rev-parse"
            ) {
                continue;
            }
        }
        return false;
    }
    has_non_neutral
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;
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
        assert_eq!(grep.name, "Grep");

        let fetch = lookup_tool_spec("fetch").unwrap();
        assert_eq!(fetch.side_effects, ToolSideEffects::External);
        assert!(!fetch.requires_confirmation);

        let write_alias = lookup_tool_spec("write").unwrap();
        assert_eq!(write_alias.name, "Edit");
        assert!(write_alias.requires_confirmation);
    }

    #[test]
    fn initial_tool_definitions_only_exposes_tool_search_when_progressive_loading_enabled() {
        let mut config = AppConfig::default();
        config.tools.progressive_loading = Some(true);
        let defs = initial_tool_definitions(&config);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0]["function"]["name"].as_str(), Some("ToolSearch"));
    }

    #[test]
    fn initial_tool_definitions_exposes_full_toolset_when_progressive_loading_disabled() {
        let mut config = AppConfig::default();
        config.tools.progressive_loading = Some(false);
        let defs = initial_tool_definitions(&config);
        let names = defs
            .iter()
            .filter_map(|def| def["function"]["name"].as_str())
            .collect::<Vec<_>>();
        assert!(!names.contains(&"ToolSearch"));
        assert!(names.contains(&"Bash"));
        assert!(names.contains(&"Read"));
        assert!(names.contains(&"Edit"));
        assert_eq!(defs.len(), 8);
    }

    #[test]
    fn tool_search_matches_claude_style_tools() {
        let matches = tool_search_matches("shell", 3);
        let names = matches.iter().map(|spec| spec.name).collect::<Vec<_>>();
        assert!(names.contains(&"Bash"));

        let write_matches = tool_search_matches("write", 3);
        let write_names = write_matches
            .iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>();
        assert!(write_names.contains(&"Edit"));
        assert!(!write_names.contains(&"Write"));
    }

    #[test]
    fn bash_read_only_detection_distinguishes_safe_and_mutating_commands() {
        assert!(is_read_only_bash_command("ls -la"));
        assert!(is_read_only_bash_command("pwd && date && uname -a"));
        assert!(!is_read_only_bash_command("echo hi > /tmp/x"));
        assert!(!is_read_only_bash_command("rm -f /tmp/x"));
    }

    #[test]
    fn tool_call_side_effects_downgrades_read_only_bash() {
        let read_only_call = ToolCall {
            id: "call_1".to_string(),
            name: "Bash".to_string(),
            arguments: json!({"command":"ls -la"}),
        };
        let mutating_call = ToolCall {
            id: "call_2".to_string(),
            name: "Bash".to_string(),
            arguments: json!({"command":"rm -f /tmp/x"}),
        };
        assert_eq!(
            tool_call_side_effects(&read_only_call),
            ToolSideEffects::ReadOnly
        );
        assert_eq!(
            tool_call_side_effects(&mutating_call),
            ToolSideEffects::Mutating
        );
        assert!(!tool_call_requires_confirmation(&read_only_call));
        assert!(tool_call_requires_confirmation(&mutating_call));
    }

    #[test]
    fn parse_confirm_input_treats_freeform_text_as_feedback_when_editable() {
        let result = parse_confirm_input("将 echo 结果改为中文", Some("echo test"));
        match result {
            Some(ConfirmResult::No(Some(feedback))) => {
                assert_eq!(feedback, "将 echo 结果改为中文");
            }
            other => panic!("unexpected confirm result: {other:?}"),
        }
    }

    #[test]
    fn parse_confirm_input_only_edits_on_explicit_edit_prefix() {
        let result = parse_confirm_input("edit echo 中文结果", Some("echo test"));
        match result {
            Some(ConfirmResult::Edit(replacement)) => {
                assert_eq!(replacement, "echo 中文结果");
            }
            other => panic!("unexpected confirm result: {other:?}"),
        }
    }

    #[test]
    fn build_edit_preview_diff_includes_hunk_context_and_changes() {
        let original = "fn main() {\n    old_call();\n}\n";
        let match_start = original.find("old_call();").unwrap();
        let diff = build_edit_preview_diff(
            "src/app.rs",
            original,
            &[match_start],
            "old_call();",
            "new_call();\n    trace_call();",
        );

        assert!(diff.contains("@@ -1,3 +1,4 @@"));
        assert!(diff.contains(" fn main() {"));
        assert!(diff.contains("-    old_call();"));
        assert!(diff.contains("+    new_call();"));
        assert!(diff.contains("+    trace_call();"));
    }

    #[test]
    fn build_full_file_diff_renders_file_creation() {
        let diff = build_full_file_diff("src/new.rs", "", "fn main() {}\n");
        assert!(diff.contains("@@ -1,0 +1,1 @@"));
        assert!(diff.contains("+fn main() {}"));
    }

    #[test]
    fn render_diff_preview_renders_line_numbers_for_removed_and_added_lines() {
        let diff = "\
--- src/app.rs
+++ src/app.rs
@@ -10,3 +10,4 @@
 fn main() {
-    old_call();
+    new_call();
+    trace_call();
 }
";
        let stripped = strip_ansi(&render_diff_preview("src/app.rs", diff));

        assert!(stripped.contains("10   fn main() {"));
        assert!(stripped.contains("11 -     old_call();"));
        assert!(stripped.contains("11 +     new_call();"));
        assert!(stripped.contains("12 +     trace_call();"));
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

    fn strip_ansi(value: &str) -> String {
        Regex::new(r"\x1b\[[0-9;]*m")
            .unwrap()
            .replace_all(value, "")
            .into_owned()
    }
}

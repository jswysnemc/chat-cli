use crate::error::{AppError, AppResult, EXIT_ARGS};
use serde_json::{Value, json};
use std::io::{self, Write};
use std::process::Command;

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

/// Returns OpenAI-compatible tool definitions for read/write/bash.
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
                "description": "Write content to a file at the given path. Creates the file if it does not exist, or overwrites it if it does.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Absolute or relative file path to write."
                        },
                        "content": {
                            "type": "string",
                            "description": "The content to write to the file."
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

/// Execute a tool call with optional user confirmation for destructive operations.
pub fn execute_tool(call: &ToolCall, auto_confirm: bool) -> AppResult<ToolResult> {
    let content = match call.name.as_str() {
        "read" => {
            let path = call.arguments["path"]
                .as_str()
                .ok_or_else(|| AppError::new(EXIT_ARGS, "read tool: missing 'path' argument"))?;
            eprintln!("[tool] read: {}", path);
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
            eprintln!("[tool] write: {} ({} bytes)", path, content.len());
            if !auto_confirm && !confirm_action(&format!("write to {path}"))? {
                "user declined the write operation".to_string()
            } else {
                tool_write(path, content)?
            }
        }
        "bash" => {
            let command = call.arguments["command"]
                .as_str()
                .ok_or_else(|| AppError::new(EXIT_ARGS, "bash tool: missing 'command' argument"))?;
            eprintln!("[tool] bash: {}", command);
            if !auto_confirm && !confirm_action(&format!("execute: {command}"))? {
                "user declined the bash execution".to_string()
            } else {
                tool_bash(command)?
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

fn tool_read(path: &str) -> AppResult<String> {
    std::fs::read_to_string(path).map_err(|err| {
        AppError::new(EXIT_ARGS, format!("failed to read `{path}`: {err}"))
    })
}

fn tool_write(path: &str, content: &str) -> AppResult<String> {
    if let Some(parent) = std::path::Path::new(path).parent() {
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

fn confirm_action(description: &str) -> AppResult<bool> {
    eprint!("[confirm] {} (y/n): ", description);
    io::stderr()
        .flush()
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to flush stderr: {err}")))?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to read confirmation: {err}")))?;
    let answer = input.trim().to_lowercase();
    Ok(answer == "y" || answer == "yes")
}

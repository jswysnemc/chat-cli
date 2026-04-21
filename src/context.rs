use chrono::Local;
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs::DirEntry;
use std::path::Path;
use std::process::Command;

const MAX_ROOT_TREE_ENTRIES: usize = 50;
const MAX_SUBDIR_TREE_ENTRIES: usize = 20;

struct CommandProbe<'a> {
    program: &'a str,
    args: &'a [&'a str],
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ValueEnum, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ContextStatusMode {
    #[default]
    Off,
    Always,
    Latest,
    #[serde(alias = "system")]
    #[value(name = "system-once", alias = "system")]
    SystemOnce,
}

impl ContextStatusMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Always => "always",
            Self::Latest => "latest",
            Self::SystemOnce => "system-once",
        }
    }
}

pub fn collect_context_status() -> String {
    let mut parts = Vec::new();

    parts.push("# Current Status".to_string());
    parts.push(format!(
        "**Time**: {}",
        Local::now().format("%Y-%m-%d %H:%M:%S %Z")
    ));

    if let Some(user) = current_user() {
        parts.push(format!("**User**: {user}"));
    }

    if let Ok(cwd) = env::current_dir() {
        parts.push(format!("**Working Directory**: `{}`", cwd.display()));
        append_file_tree(&cwd, &mut parts);
    }

    append_git_status(&mut parts);

    parts.push("## Environment".to_string());
    parts.push(format!("**OS**: {}", current_os_summary()));
    parts.push(format!("**Shell Tool**: {}", shell_tool_status()));

    parts.push("## Tooling".to_string());
    append_tool_status(
        &mut parts,
        "Git",
        &[CommandProbe {
            program: "git",
            args: &["--version"],
        }],
        None,
    );
    append_tool_status(
        &mut parts,
        "ripgrep (`rg`)",
        &[CommandProbe {
            program: "rg",
            args: &["--version"],
        }],
        Some("required by Grep"),
    );
    append_tool_status(
        &mut parts,
        "Python",
        &[
            CommandProbe {
                program: "python3",
                args: &["--version"],
            },
            CommandProbe {
                program: "python",
                args: &["--version"],
            },
        ],
        None,
    );
    append_tool_status(
        &mut parts,
        "Node.js",
        &[CommandProbe {
            program: "node",
            args: &["--version"],
        }],
        None,
    );
    append_tool_status(
        &mut parts,
        "Rust",
        &[CommandProbe {
            program: "rustc",
            args: &["--version"],
        }],
        None,
    );
    append_tool_status(
        &mut parts,
        "Cargo",
        &[CommandProbe {
            program: "cargo",
            args: &["--version"],
        }],
        None,
    );
    append_tool_status(
        &mut parts,
        "uv",
        &[CommandProbe {
            program: "uv",
            args: &["--version"],
        }],
        Some("optional"),
    );
    append_tool_status(
        &mut parts,
        "fzf",
        &[CommandProbe {
            program: "fzf",
            args: &["--version"],
        }],
        Some("optional"),
    );
    append_tool_status(
        &mut parts,
        "Go",
        &[CommandProbe {
            program: "go",
            args: &["version"],
        }],
        None,
    );
    append_tool_status(
        &mut parts,
        "Java",
        &[CommandProbe {
            program: "java",
            args: &["-version"],
        }],
        None,
    );

    parts.join("\n\n")
}

fn current_user() -> Option<String> {
    env::var("USER")
        .or_else(|_| env::var("USERNAME"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn append_file_tree(root: &Path, parts: &mut Vec<String>) {
    let Ok(entries) = visible_dir_entries(root) else {
        return;
    };
    if entries.is_empty() {
        parts.push("**Workspace**: empty directory".to_string());
        return;
    }

    parts.push("## File Tree (depth=2)".to_string());
    let total = entries.len();
    for entry in entries.iter().take(MAX_ROOT_TREE_ENTRIES) {
        parts.push(render_tree_entry(entry));
    }
    if total > MAX_ROOT_TREE_ENTRIES {
        parts.push(format!(
            "... ({} more entries)",
            total - MAX_ROOT_TREE_ENTRIES
        ));
    }
}

fn visible_dir_entries(path: &Path) -> std::io::Result<Vec<DirEntry>> {
    let mut entries = std::fs::read_dir(path)?
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            !is_ignored_entry(&name)
        })
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name().to_string_lossy().to_ascii_lowercase());
    Ok(entries)
}

fn render_tree_entry(entry: &DirEntry) -> String {
    let name = entry.file_name().to_string_lossy().to_string();
    let path = entry.path();
    if !path.is_dir() {
        return format!("- {name}");
    }

    let mut line = format!("- {name}/");
    if let Ok(children) = visible_dir_entries(&path) {
        let total = children.len();
        for child in children.iter().take(MAX_SUBDIR_TREE_ENTRIES) {
            let child_name = child.file_name().to_string_lossy().to_string();
            let suffix = if child.path().is_dir() { "/" } else { "" };
            line.push_str(&format!("\n  - {child_name}{suffix}"));
        }
        if total > MAX_SUBDIR_TREE_ENTRIES {
            line.push_str(&format!(
                "\n  - ... ({} more)",
                total - MAX_SUBDIR_TREE_ENTRIES
            ));
        }
    }
    line
}

fn append_git_status(parts: &mut Vec<String>) {
    let inside = run_command_text("git", &["rev-parse", "--is-inside-work-tree"]);
    if inside.as_deref() != Some("true") {
        return;
    }

    parts.push("## Git".to_string());
    if let Some(branch) = run_command_text("git", &["rev-parse", "--abbrev-ref", "HEAD"]) {
        parts.push(format!("**Branch**: {branch}"));
    }
    if let Some(commit) = run_command_text("git", &["log", "-1", "--format=%h %s (%cr)"])
        && !commit.is_empty()
    {
        parts.push(format!("**Commit**: {commit}"));
    }
}

fn current_os_summary() -> String {
    let info = os_info::get();
    format!("{info} ({}/{})", env::consts::OS, env::consts::ARCH)
}

fn shell_tool_status() -> String {
    #[cfg(windows)]
    {
        if let Some(version) = command_version(&[
            CommandProbe {
                program: "pwsh",
                args: &["--version"],
            },
            CommandProbe {
                program: "powershell",
                args: &[
                    "-NoLogo",
                    "-NoProfile",
                    "-Command",
                    "$PSVersionTable.PSVersion.ToString()",
                ],
            },
        ]) {
            if version.to_ascii_lowercase().contains("powershell") {
                return version;
            }
            return format!("PowerShell {version}");
        }
        "PowerShell not found".to_string()
    }

    #[cfg(not(windows))]
    {
        command_version(&[CommandProbe {
            program: "bash",
            args: &["--version"],
        }])
        .unwrap_or_else(|| "bash not found".to_string())
    }
}

fn append_tool_status(
    parts: &mut Vec<String>,
    label: &str,
    probes: &[CommandProbe<'_>],
    missing_note: Option<&str>,
) {
    let line = match command_version(probes) {
        Some(version) => format!("**{label}**: {version}"),
        None => match missing_note {
            Some(note) => format!("**{label}**: not found ({note})"),
            None => format!("**{label}**: not found"),
        },
    };
    parts.push(line);
}

fn command_version(probes: &[CommandProbe<'_>]) -> Option<String> {
    probes
        .iter()
        .find_map(|probe| run_command_text(probe.program, probe.args))
        .map(|value| value.lines().next().unwrap_or_default().trim().to_string())
        .filter(|value| !value.is_empty())
}

fn run_command_text(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    first_non_empty_line(
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
    )
}

fn first_non_empty_line(stdout: &str, stderr: &str) -> Option<String> {
    stdout
        .lines()
        .chain(stderr.lines())
        .map(|line| line.trim().trim_start_matches('\u{feff}').to_string())
        .find(|line| !line.is_empty())
}

fn is_ignored_entry(name: &str) -> bool {
    const IGNORED: &[&str] = &[
        "node_modules",
        ".git",
        "target",
        "__pycache__",
        ".DS_Store",
        "dist",
        ".next",
        ".nuxt",
        "build",
        ".cache",
        ".turbo",
        "vendor",
    ];
    IGNORED.contains(&name)
}

pub fn resolve_context_status_mode(
    cli_value: Option<ContextStatusMode>,
    config_value: Option<ContextStatusMode>,
) -> ContextStatusMode {
    cli_value.or(config_value).unwrap_or_default()
}

pub fn prepend_context_status(prompt: &str) -> String {
    let status = collect_context_status();
    format!("{status}\n\n---\n\n{prompt}")
}

#[cfg(test)]
mod tests {
    use super::{
        ContextStatusMode, collect_context_status, first_non_empty_line,
        resolve_context_status_mode, visible_dir_entries,
    };
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn cli_context_status_mode_overrides_config() {
        let resolved = resolve_context_status_mode(
            Some(ContextStatusMode::Off),
            Some(ContextStatusMode::Always),
        );
        assert_eq!(resolved, ContextStatusMode::Off);
    }

    #[test]
    fn config_context_status_mode_is_used_when_cli_is_absent() {
        let resolved = resolve_context_status_mode(None, Some(ContextStatusMode::SystemOnce));
        assert_eq!(resolved, ContextStatusMode::SystemOnce);
    }

    #[test]
    fn first_non_empty_line_prefers_stdout_then_stderr() {
        assert_eq!(
            first_non_empty_line("\nvalue\n", "ignored"),
            Some("value".to_string())
        );
        assert_eq!(
            first_non_empty_line("", "\nerror\n"),
            Some("error".to_string())
        );
    }

    #[test]
    fn visible_dir_entries_filters_common_ignored_paths() {
        let root = make_temp_dir("context-visible");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();

        let entries = visible_dir_entries(&root).unwrap();
        let names = entries
            .iter()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect::<Vec<_>>();

        assert!(names.contains(&"src".to_string()));
        assert!(names.contains(&"Cargo.toml".to_string()));
        assert!(!names.contains(&".git".to_string()));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn collect_context_status_contains_core_sections() {
        let status = collect_context_status();
        assert!(status.contains("# Current Status"));
        assert!(status.contains("**Time**:"));
        assert!(status.contains("**Working Directory**:"));
        assert!(status.contains("## Environment"));
        assert!(status.contains("## Tooling"));
        assert!(status.contains("**ripgrep (`rg`)**:"));
    }

    fn make_temp_dir(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("chat-cli-{label}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}

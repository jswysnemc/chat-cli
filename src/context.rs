use clap::ValueEnum;
use serde::{Deserialize, Serialize};

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
    if let Ok(output) = std::process::Command::new("date")
        .arg("+%Y-%m-%d %H:%M:%S %Z")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    {
        parts.push(format!("**Time**: {output}"));
    }

    if let Ok(user) = std::env::var("USER").or_else(|_| std::env::var("USERNAME")) {
        parts.push(format!("**User**: {user}"));
    }

    if let Ok(cwd) = std::env::var("PWD") {
        parts.push(format!("**Working Directory**: `{cwd}`"));
        let path = std::path::Path::new(&cwd);
        if let Ok(entries) = std::fs::read_dir(path) {
            let entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
            if entries.is_empty() {
                parts.push("- (empty directory)".to_string());
            } else {
                let mut file_tree = Vec::new();
                for entry in entries.iter().take(50) {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if is_ignored_entry(&name) {
                        continue;
                    }
                    let mut line = format!("- {name}");
                    if entry.path().is_dir() {
                        if let Ok(sub) = std::fs::read_dir(entry.path()) {
                            let all_sub: Vec<_> = sub.filter_map(|e| e.ok()).collect();
                            let total = all_sub.len();
                            for se in all_sub.iter().take(20) {
                                let sub_name = se.file_name().to_string_lossy().to_string();
                                if is_ignored_entry(&sub_name) {
                                    continue;
                                }
                                line.push_str(&format!("\n  - {sub_name}"));
                            }
                            if total > 20 {
                                line.push_str(&format!("\n  - ... ({} more)", total - 20));
                            }
                        }
                        line.push('/');
                    }
                    file_tree.push(line);
                }
                if !file_tree.is_empty() {
                    parts.push("## File Tree (depth=2)".to_string());
                    parts.extend(file_tree);
                }
                let total = entries.len();
                if total > 50 {
                    parts.push(format!("... ({} more entries)", total - 50));
                }
            }
        }
    }

    if let Ok(output) = std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
    {
        if output.status.success() {
            parts.push("## Git".to_string());
            if let Ok(branch) = std::process::Command::new("git")
                .args(["rev-parse", "--abbrev-ref", "HEAD"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            {
                parts.push(format!("**Branch**: {branch}"));
            }
            if let Ok(commit) = std::process::Command::new("git")
                .args(["log", "-1", "--format=%h %s (%cr)"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            {
                if !commit.is_empty() {
                    parts.push(format!("**Commit**: {commit}"));
                }
            }
        }
    }

    parts.push("## Environment".to_string());
    if let Ok(uname) = std::process::Command::new("uname")
        .arg("-a")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    {
        parts.push(format!("**OS**: {uname}"));
    }

    for (name, cmd) in [
        ("Python", vec!["python3", "--version"]),
        ("Node.js", vec!["node", "--version"]),
        ("Rust", vec!["rustc", "--version"]),
        ("Go", vec!["go", "version"]),
        ("Java", vec!["java", "-version"]),
    ] {
        if let Ok(output) = std::process::Command::new(cmd[0]).args(&cmd[1..]).output() {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let version = if !stdout.is_empty() { stdout } else { stderr };
            if !version.is_empty() {
                parts.push(format!("**{name}**: {version}"));
            }
        }
    }

    parts.join("\n\n")
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
    use super::{ContextStatusMode, resolve_context_status_mode};

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
}

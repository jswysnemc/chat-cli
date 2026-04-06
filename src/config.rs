use crate::cli::OutputFormat;
use crate::error::{AppError, AppResult, EXIT_CONFIG, ResultCodeExt};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_AUDIT_PROMPT_DIR: &str = "prompts";
const DEFAULT_AUDIT_PROMPT_FILE: &str = "audit-default.md";
const DEFAULT_AUDIT_BASH_PROMPT_FILE: &str = "audit-bash.md";
const DEFAULT_AUDIT_EDIT_PROMPT_FILE: &str = "audit-edit.md";
const DEFAULT_AUDIT_PROMPT_TEMPLATE: &str = include_str!("../assets/prompts/audit-default.md");
const DEFAULT_AUDIT_BASH_PROMPT_TEMPLATE: &str = include_str!("../assets/prompts/audit-bash.md");
const DEFAULT_AUDIT_EDIT_PROMPT_TEMPLATE: &str = include_str!("../assets/prompts/audit-edit.md");

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub config_file: PathBuf,
    pub secrets_file: PathBuf,
    pub state_file: PathBuf,
}

impl AppPaths {
    pub fn from_overrides(
        config_dir: Option<PathBuf>,
        data_dir: Option<PathBuf>,
    ) -> AppResult<Self> {
        let home = env::var("HOME")
            .map(PathBuf::from)
            .map_err(|_| AppError::new(EXIT_CONFIG, "HOME is not set"))?;
        let config_dir = config_dir.unwrap_or_else(|| {
            env::var("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| home.join(".config"))
                .join("chat-cli")
        });
        let data_dir = data_dir.unwrap_or_else(|| {
            env::var("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| home.join(".local").join("share"))
                .join("chat-cli")
        });
        let cache_dir = env::var("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join(".cache"))
            .join("chat-cli");
        Ok(Self {
            config_file: config_dir.join("config.toml"),
            secrets_file: config_dir.join("secrets.toml"),
            state_file: data_dir.join("state.toml"),
            config_dir,
            data_dir,
            cache_dir,
        })
    }

    pub fn sessions_dir(&self, config: &AppConfig) -> PathBuf {
        match &config.session.dir {
            Some(dir) => expand_tilde(dir),
            None => self.data_dir.join("sessions"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub defaults: DefaultsConfig,
    #[serde(default)]
    pub session: SessionConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
    #[serde(default)]
    pub audit: AuditConfig,
    #[serde(default)]
    pub skills: SkillsConfig,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub providers: BTreeMap<String, ProviderConfig>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub models: BTreeMap<String, ModelConfig>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<String, ProfileConfig>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            defaults: DefaultsConfig::default(),
            session: SessionConfig::default(),
            tools: ToolsConfig::default(),
            audit: AuditConfig::default(),
            skills: SkillsConfig::default(),
            providers: BTreeMap::new(),
            models: BTreeMap::new(),
            profiles: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefaultsConfig {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub output: Option<OutputFormat>,
    pub auto_create_session: Option<bool>,
    pub auto_save_session: Option<bool>,
    pub session_id_kind: Option<String>,
    pub tools: Option<bool>,
    pub system_prompt_file: Option<String>,
    pub system_prompt_mode: Option<String>,
    pub collapse_thinking: Option<bool>,
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            provider: None,
            model: None,
            mode: Some("auto".to_string()),
            output: Some(OutputFormat::Line),
            auto_create_session: Some(true),
            auto_save_session: Some(true),
            session_id_kind: Some("ulid".to_string()),
            tools: None,
            system_prompt_file: None,
            system_prompt_mode: Some("append".to_string()),
            collapse_thinking: Some(false),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    pub store_format: Option<String>,
    pub dir: Option<String>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            store_format: Some("jsonl".to_string()),
            dir: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsConfig {
    pub max_rounds: Option<u32>,
    pub progressive_loading: Option<bool>,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            max_rounds: Some(20),
            progressive_loading: Some(true),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditConfig {
    pub enabled: Option<bool>,
    pub model: Option<String>,
    pub default_prompt_file: Option<String>,
    pub bash_prompt_file: Option<String>,
    pub edit_prompt_file: Option<String>,
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            enabled: Some(false),
            model: None,
            default_prompt_file: None,
            bash_prompt_file: None,
            edit_prompt_file: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillsConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            paths: vec![".claude/skills".to_string(), "~/.claude/skills".to_string()],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderConfig {
    pub kind: String,
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    pub org: Option<String>,
    pub project: Option<String>,
    pub default_model: Option<String>,
    pub timeout: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelPatchConfig {
    pub system_to_user: Option<bool>,
}

impl ModelPatchConfig {
    fn is_empty(&self) -> bool {
        self.system_to_user.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelConfig {
    pub provider: String,
    pub remote_name: String,
    pub display_name: Option<String>,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    pub temperature: Option<f64>,
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "ModelPatchConfig::is_empty")]
    pub patches: ModelPatchConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProfileConfig {
    pub provider: String,
    pub model: String,
    pub system: Option<String>,
    pub temperature: Option<f64>,
    pub max_output_tokens: Option<u32>,
    pub output: Option<OutputFormat>,
    pub stream: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SecretsConfig {
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderSecret>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderSecret {
    pub api_key: Option<String>,
}

pub fn ensure_dirs(paths: &AppPaths, config: &AppConfig) -> AppResult<()> {
    fs::create_dir_all(&paths.config_dir).code(EXIT_CONFIG, "failed to create config dir")?;
    fs::create_dir_all(&paths.data_dir).code(EXIT_CONFIG, "failed to create data dir")?;
    fs::create_dir_all(&paths.cache_dir).code(EXIT_CONFIG, "failed to create cache dir")?;
    fs::create_dir_all(paths.sessions_dir(config))
        .code(EXIT_CONFIG, "failed to create sessions dir")?;
    ensure_default_audit_prompt_files(paths)?;
    Ok(())
}

pub fn init_config_files(paths: &AppPaths) -> AppResult<()> {
    let mut config = AppConfig::default();
    apply_runtime_config_defaults(paths, &mut config);
    ensure_dirs(paths, &config)?;
    if !paths.config_file.exists() {
        save_config(paths, &config)?;
    }
    if !paths.secrets_file.exists() {
        save_secrets(paths, &SecretsConfig::default())?;
    }
    Ok(())
}

pub fn apply_runtime_config_defaults(paths: &AppPaths, config: &mut AppConfig) {
    config.audit.default_prompt_file.get_or_insert_with(|| {
        default_audit_prompt_path(paths, DEFAULT_AUDIT_PROMPT_FILE)
            .display()
            .to_string()
    });
    config.audit.bash_prompt_file.get_or_insert_with(|| {
        default_audit_prompt_path(paths, DEFAULT_AUDIT_BASH_PROMPT_FILE)
            .display()
            .to_string()
    });
    config.audit.edit_prompt_file.get_or_insert_with(|| {
        default_audit_prompt_path(paths, DEFAULT_AUDIT_EDIT_PROMPT_FILE)
            .display()
            .to_string()
    });
}

pub fn load_config(paths: &AppPaths) -> AppResult<AppConfig> {
    if !paths.config_file.exists() {
        return Ok(AppConfig::default());
    }
    let text =
        fs::read_to_string(&paths.config_file).code(EXIT_CONFIG, "failed to read config file")?;
    toml::from_str(&text).code(EXIT_CONFIG, "failed to parse config.toml")
}

pub fn save_config(paths: &AppPaths, config: &AppConfig) -> AppResult<()> {
    if let Some(parent) = paths.config_file.parent() {
        fs::create_dir_all(parent).code(EXIT_CONFIG, "failed to create config file parent dir")?;
    }
    let text = toml::to_string_pretty(config).code(EXIT_CONFIG, "failed to serialize config")?;
    fs::write(&paths.config_file, text).code(EXIT_CONFIG, "failed to write config.toml")
}

pub fn load_secrets(paths: &AppPaths) -> AppResult<SecretsConfig> {
    if !paths.secrets_file.exists() {
        return Ok(SecretsConfig::default());
    }
    let text =
        fs::read_to_string(&paths.secrets_file).code(EXIT_CONFIG, "failed to read secrets file")?;
    toml::from_str(&text).code(EXIT_CONFIG, "failed to parse secrets.toml")
}

pub fn save_secrets(paths: &AppPaths, secrets: &SecretsConfig) -> AppResult<()> {
    if let Some(parent) = paths.secrets_file.parent() {
        fs::create_dir_all(parent).code(EXIT_CONFIG, "failed to create secrets file parent dir")?;
    }
    let text = toml::to_string_pretty(secrets).code(EXIT_CONFIG, "failed to serialize secrets")?;
    fs::write(&paths.secrets_file, text).code(EXIT_CONFIG, "failed to write secrets.toml")
}

pub fn validate_config(config: &AppConfig) -> Vec<String> {
    let mut issues = Vec::new();
    if let Some(provider) = &config.defaults.provider {
        if !config.providers.contains_key(provider) {
            issues.push(format!(
                "defaults.provider references missing provider `{provider}`"
            ));
        }
    }
    if let Some(model_id) = &config.defaults.model {
        match config.models.get(model_id) {
            Some(model) => {
                if let Some(provider) = &config.defaults.provider {
                    if &model.provider != provider {
                        issues.push(format!(
                            "defaults.model `{model_id}` belongs to provider `{}`, not defaults.provider `{provider}`",
                            model.provider
                        ));
                    }
                }
            }
            None => issues.push(format!(
                "defaults.model references missing model `{model_id}`"
            )),
        }
    }
    for (id, provider) in &config.providers {
        match provider.kind.as_str() {
            "openai_compatible" | "anthropic" | "ollama" => {}
            other => issues.push(format!("provider `{id}` has unsupported kind `{other}`")),
        }
    }
    for (id, model) in &config.models {
        if !config.providers.contains_key(&model.provider) {
            issues.push(format!(
                "model `{id}` references missing provider `{}`",
                model.provider
            ));
        }
    }
    if let Some(model_id) = config
        .audit
        .model
        .as_ref()
        .filter(|value| !value.trim().is_empty())
        && !config.models.contains_key(model_id)
    {
        issues.push(format!("audit.model references missing model `{model_id}`"));
    }
    issues
}

pub fn render_config_value(config: &AppConfig, key: &str) -> AppResult<String> {
    match key {
        "defaults.provider" => Ok(config.defaults.provider.clone().unwrap_or_default()),
        "defaults.model" => Ok(config.defaults.model.clone().unwrap_or_default()),
        "defaults.mode" => Ok(config.defaults.mode.clone().unwrap_or_default()),
        "defaults.output" => Ok(config
            .defaults
            .output
            .clone()
            .unwrap_or(OutputFormat::Line)
            .to_string()),
        "defaults.auto_create_session" => Ok(config
            .defaults
            .auto_create_session
            .unwrap_or(true)
            .to_string()),
        "defaults.auto_save_session" => Ok(config
            .defaults
            .auto_save_session
            .unwrap_or(true)
            .to_string()),
        "defaults.tools" => Ok(config.defaults.tools.unwrap_or(false).to_string()),
        "defaults.system_prompt_file" => Ok(config
            .defaults
            .system_prompt_file
            .clone()
            .unwrap_or_default()),
        "defaults.system_prompt_mode" => Ok(config
            .defaults
            .system_prompt_mode
            .clone()
            .unwrap_or_else(|| "append".to_string())),
        "defaults.collapse_thinking" => Ok(config
            .defaults
            .collapse_thinking
            .unwrap_or(false)
            .to_string()),
        "session.store_format" => Ok(config
            .session
            .store_format
            .clone()
            .unwrap_or_else(|| "jsonl".to_string())),
        "session.dir" => Ok(config.session.dir.clone().unwrap_or_default()),
        "tools.max_rounds" => Ok(config.tools.max_rounds.unwrap_or(20).to_string()),
        "tools.progressive_loading" => {
            Ok(config.tools.progressive_loading.unwrap_or(true).to_string())
        }
        "audit.enabled" => Ok(config.audit.enabled.unwrap_or(false).to_string()),
        "audit.model" => Ok(config.audit.model.clone().unwrap_or_default()),
        "audit.default_prompt_file" => {
            Ok(config.audit.default_prompt_file.clone().unwrap_or_default())
        }
        "audit.bash_prompt_file" => Ok(config.audit.bash_prompt_file.clone().unwrap_or_default()),
        "audit.edit_prompt_file" => Ok(config.audit.edit_prompt_file.clone().unwrap_or_default()),
        "skills.paths" => serde_json::to_string(&config.skills.paths).map_err(|err| {
            AppError::new(EXIT_CONFIG, format!("failed to render skills.paths: {err}"))
        }),
        _ => Err(AppError::new(
            EXIT_CONFIG,
            format!("unsupported config key `{key}`"),
        )),
    }
}

pub fn set_config_value(config: &mut AppConfig, key: &str, value: &str) -> AppResult<()> {
    match key {
        "defaults.provider" => config.defaults.provider = Some(value.to_string()),
        "defaults.model" => config.defaults.model = Some(value.to_string()),
        "defaults.mode" => config.defaults.mode = Some(value.to_string()),
        "defaults.output" => {
            config.defaults.output = Some(parse_output_format(value)?);
        }
        "defaults.auto_create_session" => {
            config.defaults.auto_create_session = Some(parse_bool(value)?);
        }
        "defaults.auto_save_session" => {
            config.defaults.auto_save_session = Some(parse_bool(value)?);
        }
        "defaults.tools" => {
            config.defaults.tools = Some(parse_bool(value)?);
        }
        "defaults.system_prompt_file" => {
            config.defaults.system_prompt_file = Some(value.to_string());
        }
        "defaults.system_prompt_mode" => match value {
            "append" | "override" => {
                config.defaults.system_prompt_mode = Some(value.to_string());
            }
            _ => {
                return Err(AppError::new(
                    EXIT_CONFIG,
                    "system_prompt_mode must be 'append' or 'override'",
                ));
            }
        },
        "defaults.collapse_thinking" => {
            config.defaults.collapse_thinking = Some(parse_bool(value)?);
        }
        "session.store_format" => config.session.store_format = Some(value.to_string()),
        "session.dir" => config.session.dir = Some(value.to_string()),
        "tools.max_rounds" => {
            config.tools.max_rounds = Some(parse_u32(value, "tools.max_rounds")?);
        }
        "tools.progressive_loading" => {
            config.tools.progressive_loading = Some(parse_bool(value)?);
        }
        "audit.enabled" => {
            config.audit.enabled = Some(parse_bool(value)?);
        }
        "audit.model" => {
            config.audit.model = if value.trim().is_empty() {
                None
            } else {
                Some(value.to_string())
            };
        }
        "audit.default_prompt_file" => {
            config.audit.default_prompt_file = normalize_optional_path(value);
        }
        "audit.bash_prompt_file" => {
            config.audit.bash_prompt_file = normalize_optional_path(value);
        }
        "audit.edit_prompt_file" => {
            config.audit.edit_prompt_file = normalize_optional_path(value);
        }
        "skills.paths" => {
            config.skills.paths = parse_string_array(value, "skills.paths")?;
        }
        _ => {
            return Err(AppError::new(
                EXIT_CONFIG,
                format!("unsupported config key `{key}`"),
            ));
        }
    }
    Ok(())
}

pub fn parse_headers(headers: &[String]) -> AppResult<BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    for header in headers {
        let (key, value) = header.split_once('=').ok_or_else(|| {
            AppError::new(
                EXIT_CONFIG,
                format!("invalid header `{header}`, expected KEY=VALUE"),
            )
        })?;
        map.insert(key.trim().to_string(), value.trim().to_string());
    }
    Ok(map)
}

pub fn read_system_prompt(value: &Option<String>) -> AppResult<Option<String>> {
    match value {
        Some(v) if v.starts_with('@') => {
            let path = Path::new(&v[1..]);
            let content = fs::read_to_string(path).code(
                EXIT_CONFIG,
                format!("failed to read system prompt file `{}`", path.display()),
            )?;
            Ok(Some(content))
        }
        Some(v) => Ok(Some(v.clone())),
        None => Ok(None),
    }
}

pub fn expand_tilde(input: &str) -> PathBuf {
    if let Some(stripped) = input.strip_prefix("~/") {
        if let Ok(home) = env::var("HOME") {
            return PathBuf::from(home).join(stripped);
        }
    }
    PathBuf::from(input)
}

fn default_audit_prompt_path(paths: &AppPaths, file_name: &str) -> PathBuf {
    paths
        .config_dir
        .join(DEFAULT_AUDIT_PROMPT_DIR)
        .join(file_name)
}

fn ensure_default_audit_prompt_file(path: &Path, contents: &str) -> AppResult<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).code(EXIT_CONFIG, "failed to create audit prompt dir")?;
    }
    fs::write(path, contents).code(
        EXIT_CONFIG,
        format!("failed to write audit prompt file `{}`", path.display()),
    )
}

fn ensure_default_audit_prompt_files(paths: &AppPaths) -> AppResult<()> {
    ensure_default_audit_prompt_file(
        &default_audit_prompt_path(paths, DEFAULT_AUDIT_PROMPT_FILE),
        DEFAULT_AUDIT_PROMPT_TEMPLATE,
    )?;
    ensure_default_audit_prompt_file(
        &default_audit_prompt_path(paths, DEFAULT_AUDIT_BASH_PROMPT_FILE),
        DEFAULT_AUDIT_BASH_PROMPT_TEMPLATE,
    )?;
    ensure_default_audit_prompt_file(
        &default_audit_prompt_path(paths, DEFAULT_AUDIT_EDIT_PROMPT_FILE),
        DEFAULT_AUDIT_EDIT_PROMPT_TEMPLATE,
    )
}

fn normalize_optional_path(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn parse_output_format(value: &str) -> AppResult<OutputFormat> {
    match value {
        "line" => Ok(OutputFormat::Line),
        "text" => Ok(OutputFormat::Text),
        "json" => Ok(OutputFormat::Json),
        "ndjson" => Ok(OutputFormat::Ndjson),
        _ => Err(AppError::new(
            EXIT_CONFIG,
            format!("unsupported output format `{value}`"),
        )),
    }
}

fn parse_bool(value: &str) -> AppResult<bool> {
    match value {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(AppError::new(
            EXIT_CONFIG,
            format!("invalid boolean value `{value}`"),
        )),
    }
}

fn parse_u32(value: &str, key: &str) -> AppResult<u32> {
    let parsed = value
        .parse::<u32>()
        .map_err(|_| AppError::new(EXIT_CONFIG, format!("{key} must be a positive integer")))?;
    if parsed == 0 {
        return Err(AppError::new(
            EXIT_CONFIG,
            format!("{key} must be greater than 0"),
        ));
    }
    Ok(parsed)
}

fn parse_string_array(value: &str, key: &str) -> AppResult<Vec<String>> {
    serde_json::from_str::<Vec<String>>(value).map_err(|err| {
        AppError::new(
            EXIT_CONFIG,
            format!("{key} must be a JSON string array: {err}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn default_skills_paths_are_populated() {
        let config = AppConfig::default();
        assert_eq!(
            config.skills.paths,
            vec![".claude/skills", "~/.claude/skills"]
        );
        assert_eq!(config.tools.max_rounds, Some(20));
        assert_eq!(config.tools.progressive_loading, Some(true));
    }

    #[test]
    fn set_config_value_parses_skills_paths_array() {
        let mut config = AppConfig::default();
        set_config_value(&mut config, "skills.paths", "[\"a\",\"b\"]").unwrap();
        assert_eq!(config.skills.paths, vec!["a", "b"]);
    }

    #[test]
    fn set_config_value_parses_tools_max_rounds() {
        let mut config = AppConfig::default();
        set_config_value(&mut config, "tools.max_rounds", "12").unwrap();
        set_config_value(&mut config, "tools.progressive_loading", "false").unwrap();
        assert_eq!(config.tools.max_rounds, Some(12));
        assert_eq!(config.tools.progressive_loading, Some(false));
    }

    #[test]
    fn set_config_value_parses_audit_keys() {
        let mut config = AppConfig::default();
        set_config_value(&mut config, "audit.enabled", "true").unwrap();
        set_config_value(&mut config, "audit.model", "audit-model").unwrap();
        set_config_value(
            &mut config,
            "audit.default_prompt_file",
            "/tmp/audit-default.md",
        )
        .unwrap();
        set_config_value(&mut config, "audit.bash_prompt_file", "/tmp/audit-bash.md").unwrap();
        set_config_value(&mut config, "audit.edit_prompt_file", "/tmp/audit-edit.md").unwrap();
        assert_eq!(config.audit.enabled, Some(true));
        assert_eq!(config.audit.model.as_deref(), Some("audit-model"));
        assert_eq!(
            config.audit.default_prompt_file.as_deref(),
            Some("/tmp/audit-default.md")
        );
        assert_eq!(
            config.audit.bash_prompt_file.as_deref(),
            Some("/tmp/audit-bash.md")
        );
        assert_eq!(
            config.audit.edit_prompt_file.as_deref(),
            Some("/tmp/audit-edit.md")
        );

        set_config_value(&mut config, "audit.model", "").unwrap();
        set_config_value(&mut config, "audit.default_prompt_file", "").unwrap();
        assert_eq!(config.audit.model, None);
        assert_eq!(config.audit.default_prompt_file, None);
    }

    #[test]
    fn validate_config_reports_missing_audit_model() {
        let mut config = AppConfig::default();
        config.audit.model = Some("missing-audit-model".to_string());
        let issues = validate_config(&config);
        assert!(
            issues
                .iter()
                .any(|issue| issue.contains("audit.model references missing model"))
        );
    }

    #[test]
    fn runtime_defaults_populate_audit_prompt_paths_and_create_prompt_files() {
        let base = std::env::temp_dir().join(format!("chat-cli-config-test-{}", ulid::Ulid::new()));
        let paths =
            AppPaths::from_overrides(Some(base.join("config")), Some(base.join("data"))).unwrap();
        let mut config = AppConfig::default();
        apply_runtime_config_defaults(&paths, &mut config);
        ensure_dirs(&paths, &config).unwrap();

        let default_path = config.audit.default_prompt_file.clone().unwrap();
        let bash_path = config.audit.bash_prompt_file.clone().unwrap();
        let edit_path = config.audit.edit_prompt_file.clone().unwrap();
        assert!(Path::new(&default_path).exists());
        assert!(Path::new(&bash_path).exists());
        assert!(Path::new(&edit_path).exists());

        let default_text = fs::read_to_string(&default_path).unwrap();
        let bash_text = fs::read_to_string(&bash_path).unwrap();
        let edit_text = fs::read_to_string(&edit_path).unwrap();
        assert!(default_text.contains("\"results\""));
        assert!(bash_text.contains("shell"));
        assert!(edit_text.contains("file edit"));

        let _ = fs::remove_dir_all(base);
    }
}

impl std::fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            OutputFormat::Line => "line",
            OutputFormat::Text => "text",
            OutputFormat::Json => "json",
            OutputFormat::Ndjson => "ndjson",
        };
        f.write_str(value)
    }
}

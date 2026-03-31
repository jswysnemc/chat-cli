use crate::cli::OutputFormat;
use crate::error::{AppError, AppResult, EXIT_CONFIG, ResultCodeExt};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

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
pub struct ModelConfig {
    pub provider: String,
    pub remote_name: String,
    pub display_name: Option<String>,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    pub temperature: Option<f64>,
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
    Ok(())
}

pub fn init_config_files(paths: &AppPaths) -> AppResult<()> {
    let config = AppConfig::default();
    ensure_dirs(paths, &config)?;
    if !paths.config_file.exists() {
        save_config(paths, &config)?;
    }
    if !paths.secrets_file.exists() {
        save_secrets(paths, &SecretsConfig::default())?;
    }
    Ok(())
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
        "defaults.tools" => Ok(config
            .defaults
            .tools
            .unwrap_or(false)
            .to_string()),
        "session.store_format" => Ok(config
            .session
            .store_format
            .clone()
            .unwrap_or_else(|| "jsonl".to_string())),
        "session.dir" => Ok(config.session.dir.clone().unwrap_or_default()),
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
        "session.store_format" => config.session.store_format = Some(value.to_string()),
        "session.dir" => config.session.dir = Some(value.to_string()),
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

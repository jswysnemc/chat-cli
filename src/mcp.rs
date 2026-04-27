use crate::config::{AppConfig, AppPaths, expand_tilde};
use crate::error::{AppError, AppResult, EXIT_ARGS, EXIT_CONFIG, ResultCodeExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    #[serde(default)]
    pub r#type: Option<String>,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_timeout_sec: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_timeout_sec: Option<f64>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enabled_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

const fn default_enabled() -> bool {
    true
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            r#type: None,
            command: String::new(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
            startup_timeout_sec: None,
            tool_timeout_sec: None,
            enabled: true,
            enabled_tools: Vec::new(),
            disabled_tools: Vec::new(),
            description: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpConfigCompat {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mcp: BTreeMap<String, McpServerConfig>,
    #[serde(
        default,
        alias = "mcpServers",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub mcp_servers: BTreeMap<String, McpServerConfig>,
}

impl McpConfigCompat {
    pub fn into_servers(self) -> BTreeMap<String, McpServerConfig> {
        if !self.mcp.is_empty() {
            self.mcp
        } else {
            self.mcp_servers
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolSpec {
    pub full_name: String,
    pub server: String,
    pub remote_name: String,
    pub description: String,
    pub input_schema: Value,
    pub read_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerCacheEntry {
    pub server: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub enabled_tools: Vec<String>,
    pub disabled_tools: Vec<String>,
    pub tools: Vec<McpToolSpec>,
    pub checked_at_unix_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpCache {
    pub servers: BTreeMap<String, McpServerCacheEntry>,
}

#[derive(Debug, Clone)]
pub struct McpServerProbe {
    pub server: String,
    pub ok: bool,
    pub command: String,
    pub tool_count: usize,
    pub tools: Vec<McpToolSpec>,
    pub debug: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct McpWarmupWarning {
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct McpDaemonStatus {
    pub running: bool,
    pub registered: bool,
    pub pid: Option<u32>,
    pub port: Option<u16>,
    pub log_file: PathBuf,
    pub server: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct McpServerStatus {
    pub server: String,
    pub enabled: bool,
    pub cached: bool,
    pub cached_tools: usize,
    pub live_ok: bool,
    pub live_tools: usize,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpDaemonState {
    pub pid: u32,
    pub port: u16,
    pub started_at_unix_ms: u128,
    pub log_file: PathBuf,
    pub server: Option<String>,
}

#[derive(Debug, Clone)]
pub struct McpDaemonStart {
    pub pid_file: PathBuf,
    pub log_file: PathBuf,
    pub server: Option<String>,
}

#[derive(Debug, Clone)]
pub struct McpDaemonStop {
    pub pid: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct McpDaemonRequest {
    kind: String,
    full_name: Option<String>,
    arguments: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct McpDaemonResponse {
    ok: bool,
    content: Option<String>,
    tools: Option<Vec<McpToolSpec>>,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct McpWarmupState {
    started_at_unix_ms: u128,
    result: Option<AppResult<Vec<McpServerProbe>>>,
}

#[derive(Clone)]
pub struct McpWarmupHandle {
    state: Arc<Mutex<McpWarmupState>>,
}

struct McpProcess {
    child: Child,
    stdin: ChildStdin,
    receiver: mpsc::Receiver<Value>,
    stderr_receiver: Option<mpsc::Receiver<String>>,
    next_id: u64,
}

struct DaemonServerSession {
    name: String,
    config: McpServerConfig,
    client: McpProcess,
    tools: Vec<McpToolSpec>,
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

static MCP_TOOL_CACHE: OnceLock<Mutex<BTreeMap<String, Vec<McpToolSpec>>>> = OnceLock::new();
const MCP_PROTOCOL_VERSION: &str = "2025-03-26";
pub const MCP_WARMUP_WAIT_SECS: f64 = 3.0;

pub fn mcp_enabled(config: &AppConfig) -> bool {
    config.tools.mcp.unwrap_or(false)
}

pub fn mcp_cache_path(paths: &AppPaths) -> PathBuf {
    paths.cache_dir.join("mcp-tools.json")
}

pub fn mcp_daemon_pid_path(paths: &AppPaths) -> PathBuf {
    paths.cache_dir.join("mcp-daemon.json")
}

pub fn mcp_daemon_log_path(paths: &AppPaths) -> PathBuf {
    paths.cache_dir.join("mcp-daemon.log")
}

fn load_mcp_daemon_state(paths: &AppPaths) -> AppResult<McpDaemonState> {
    let text = fs::read_to_string(mcp_daemon_pid_path(paths))
        .code(EXIT_CONFIG, "failed to read MCP daemon state")?;
    serde_json::from_str(&text).code(EXIT_CONFIG, "failed to parse MCP daemon state")
}

fn save_mcp_daemon_state(paths: &AppPaths, state: &McpDaemonState) -> AppResult<()> {
    let path = mcp_daemon_pid_path(paths);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).code(EXIT_CONFIG, "failed to create MCP daemon dir")?;
    }
    let text = serde_json::to_string_pretty(state).map_err(|err| {
        AppError::new(
            EXIT_CONFIG,
            format!("failed to encode MCP daemon state: {err}"),
        )
    })?;
    fs::write(path, text).code(EXIT_CONFIG, "failed to write MCP daemon state")
}

pub fn current_mcp_daemon_status(paths: &AppPaths) -> McpDaemonStatus {
    match load_mcp_daemon_state(paths) {
        Ok(state) => {
            let ping = ping_mcp_daemon(&state);
            McpDaemonStatus {
                running: ping.is_ok(),
                registered: true,
                pid: Some(state.pid),
                port: Some(state.port),
                log_file: state.log_file,
                server: state.server,
                error: ping.err().map(|err| err.message),
            }
        }
        Err(_) => McpDaemonStatus {
            running: false,
            registered: false,
            pid: None,
            port: None,
            log_file: mcp_daemon_log_path(paths),
            server: None,
            error: None,
        },
    }
}

pub fn current_mcp_server_statuses(paths: &AppPaths, config: &AppConfig) -> Vec<McpServerStatus> {
    if !mcp_enabled(config) {
        return config
            .mcp
            .keys()
            .map(|name| McpServerStatus {
                server: name.clone(),
                enabled: false,
                cached: false,
                cached_tools: 0,
                live_ok: false,
                live_tools: 0,
                error: Some("mcp disabled by config".to_string()),
            })
            .collect();
    }
    let cache = load_mcp_cache(paths).unwrap_or_default();
    let probes = probe_mcp_servers(config, None)
        .into_iter()
        .map(|probe| (probe.server.clone(), probe))
        .collect::<BTreeMap<_, _>>();
    config
        .mcp
        .iter()
        .map(|(name, server)| {
            let cached_entry = cache.servers.get(name);
            let cached =
                cached_entry.is_some_and(|entry| server_matches_cache_entry(server, entry));
            let cached_tools = cached_entry.map(|entry| entry.tools.len()).unwrap_or(0);
            let probe = probes.get(name);
            McpServerStatus {
                server: name.clone(),
                enabled: server.enabled,
                cached,
                cached_tools,
                live_ok: probe.is_some_and(|probe| probe.ok),
                live_tools: probe.map(|probe| probe.tool_count).unwrap_or(0),
                error: probe.and_then(|probe| probe.error.clone()),
            }
        })
        .collect()
}

pub fn load_mcp_cache(paths: &AppPaths) -> AppResult<McpCache> {
    let path = mcp_cache_path(paths);
    if !path.exists() {
        return Ok(McpCache::default());
    }
    let text = fs::read_to_string(&path).code(EXIT_CONFIG, "failed to read MCP cache")?;
    serde_json::from_str(&text).code(EXIT_CONFIG, "failed to parse MCP cache")
}

fn set_cache_entry(
    cache: &mut McpCache,
    server_name: &str,
    server: &McpServerConfig,
    tools: Vec<McpToolSpec>,
) {
    cache.servers.insert(
        server_name.to_string(),
        McpServerCacheEntry {
            server: server_name.to_string(),
            command: server.command.clone(),
            args: server.args.clone(),
            cwd: server.cwd.clone(),
            enabled_tools: server.enabled_tools.clone(),
            disabled_tools: server.disabled_tools.clone(),
            tools,
            checked_at_unix_ms: now_unix_ms(),
        },
    );
}

pub fn save_mcp_cache(paths: &AppPaths, cache: &McpCache) -> AppResult<()> {
    let path = mcp_cache_path(paths);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).code(EXIT_CONFIG, "failed to create MCP cache dir")?;
    }
    let text = serde_json::to_string_pretty(cache)
        .map_err(|err| AppError::new(EXIT_CONFIG, format!("failed to encode MCP cache: {err}")))?;
    fs::write(path, text).code(EXIT_CONFIG, "failed to write MCP cache")
}

fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn server_matches_cache_entry(server: &McpServerConfig, entry: &McpServerCacheEntry) -> bool {
    server.command == entry.command
        && server.args == entry.args
        && server.cwd == entry.cwd
        && server.enabled_tools == entry.enabled_tools
        && server.disabled_tools == entry.disabled_tools
}

pub fn probe_mcp_servers(config: &AppConfig, only_server: Option<&str>) -> Vec<McpServerProbe> {
    enabled_mcp_servers(config)
        .into_iter()
        .filter(|(name, _)| only_server.is_none_or(|target| target == name))
        .map(|(server_name, server)| probe_single_server(&server_name, &server))
        .collect()
}

pub fn authenticate_and_cache_mcp(
    paths: &AppPaths,
    config: &AppConfig,
    only_server: Option<&str>,
    use_cache: bool,
) -> AppResult<Vec<McpServerProbe>> {
    let probes = probe_mcp_servers(config, only_server);
    if probes.iter().all(|probe| probe.ok) && use_cache {
        let mut cache = load_mcp_cache(paths).unwrap_or_default();
        if only_server.is_none() {
            cache.servers.clear();
        }
        for probe in &probes {
            if let Some(server) = enabled_mcp_servers(config).get(&probe.server) {
                set_cache_entry(&mut cache, &probe.server, server, probe.tools.clone());
            }
        }
        save_mcp_cache(paths, &cache)?;
    }
    if let Some(failed) = probes.iter().find(|probe| !probe.ok) {
        return Err(AppError::new(
            EXIT_CONFIG,
            failed
                .error
                .clone()
                .unwrap_or_else(|| format!("MCP server `{}` probe failed", failed.server)),
        ));
    }
    Ok(probes)
}

pub fn start_mcp_daemon_process(
    paths: &AppPaths,
    only_server: Option<&str>,
) -> AppResult<McpDaemonStart> {
    let exe = std::env::current_exe().code(EXIT_CONFIG, "failed to locate current executable")?;
    let log_file = mcp_daemon_log_path(paths);
    if let Some(parent) = log_file.parent() {
        fs::create_dir_all(parent).code(EXIT_CONFIG, "failed to create MCP log dir")?;
    }
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
        .code(EXIT_CONFIG, "failed to open MCP daemon log")?;
    let log_err = log
        .try_clone()
        .code(EXIT_CONFIG, "failed to clone MCP daemon log")?;
    let mut command = Command::new(exe);
    if let Some(config_dir) = paths.config_file.parent() {
        command.arg("--config-dir").arg(config_dir);
    }
    command.arg("mcp").arg("start");
    if let Some(server) = only_server {
        command.arg("--server").arg(server);
    }
    command.env("CHAT_CLI_MCP_DAEMON", "1");
    command.stdin(Stdio::null());
    command.stdout(Stdio::from(log));
    command.stderr(Stdio::from(log_err));
    let _child = command
        .spawn()
        .code(EXIT_CONFIG, "failed to spawn MCP daemon")?;
    Ok(McpDaemonStart {
        pid_file: mcp_daemon_pid_path(paths),
        log_file,
        server: only_server.map(str::to_string),
    })
}

pub fn run_mcp_daemon(
    paths: &AppPaths,
    config: &AppConfig,
    only_server: Option<&str>,
) -> AppResult<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .code(EXIT_CONFIG, "failed to bind MCP daemon listener")?;
    listener
        .set_nonblocking(false)
        .code(EXIT_CONFIG, "failed to configure MCP daemon listener")?;
    let port = listener
        .local_addr()
        .code(EXIT_CONFIG, "failed to read MCP daemon address")?
        .port();
    let state = McpDaemonState {
        pid: std::process::id(),
        port,
        started_at_unix_ms: now_unix_ms(),
        log_file: mcp_daemon_log_path(paths),
        server: only_server.map(str::to_string),
    };
    save_mcp_daemon_state(paths, &state)?;
    let mut scoped = config.clone();
    if let Some(server_name) = only_server {
        scoped.mcp.retain(|name, _| name == server_name);
    }
    let mut sessions = build_daemon_sessions(&scoped)?;
    persist_daemon_sessions_to_cache(paths, &sessions)?;
    set_cached_mcp_tools(
        &scoped,
        sessions
            .iter()
            .flat_map(|session| session.tools.clone())
            .collect(),
    );
    loop {
        let (mut stream, _) = listener
            .accept()
            .code(EXIT_CONFIG, "MCP daemon accept failed")?;
        let mut body = String::new();
        stream
            .read_to_string(&mut body)
            .code(EXIT_CONFIG, "failed to read MCP daemon request")?;
        let request: McpDaemonRequest =
            serde_json::from_str(&body).code(EXIT_CONFIG, "failed to parse MCP daemon request")?;
        let response = match request.kind.as_str() {
            "stop" => {
                write_daemon_response(
                    &mut stream,
                    &McpDaemonResponse {
                        ok: true,
                        content: Some("stopping".to_string()),
                        tools: None,
                        error: None,
                    },
                )?;
                let _ = fs::remove_file(mcp_daemon_pid_path(paths));
                return Ok(());
            }
            "tools" => McpDaemonResponse {
                ok: true,
                content: None,
                tools: Some(
                    sessions
                        .iter()
                        .flat_map(|session| session.tools.clone())
                        .collect(),
                ),
                error: None,
            },
            "health" => McpDaemonResponse {
                ok: true,
                content: Some("ok".to_string()),
                tools: None,
                error: None,
            },
            "call" => match request.full_name.as_deref() {
                Some(full_name) => {
                    let arguments = request.arguments.unwrap_or(Value::Null);
                    match call_daemon_session_tool(&mut sessions, full_name, &arguments) {
                        Ok(content) => McpDaemonResponse {
                            ok: true,
                            content: Some(content),
                            tools: None,
                            error: None,
                        },
                        Err(err) => McpDaemonResponse {
                            ok: false,
                            content: None,
                            tools: None,
                            error: Some(err.message),
                        },
                    }
                }
                None => McpDaemonResponse {
                    ok: false,
                    content: None,
                    tools: None,
                    error: Some("missing full_name".to_string()),
                },
            },
            other => McpDaemonResponse {
                ok: false,
                content: None,
                tools: None,
                error: Some(format!("unknown MCP daemon request `{other}`")),
            },
        };
        write_daemon_response(&mut stream, &response)?;
    }
}

pub fn stop_mcp_daemon(paths: &AppPaths) -> AppResult<McpDaemonStop> {
    let state = load_mcp_daemon_state(paths)?;
    let response = call_mcp_daemon(
        paths,
        &McpDaemonRequest {
            kind: "stop".to_string(),
            full_name: None,
            arguments: None,
        },
    )?;
    if !response.ok {
        return Err(AppError::new(
            EXIT_CONFIG,
            response
                .error
                .unwrap_or_else(|| "failed to stop MCP daemon".to_string()),
        ));
    }
    Ok(McpDaemonStop { pid: state.pid })
}

fn call_mcp_daemon(paths: &AppPaths, request: &McpDaemonRequest) -> AppResult<McpDaemonResponse> {
    let state = load_mcp_daemon_state(paths)?;
    call_mcp_daemon_with_state(&state, request)
}

fn call_mcp_daemon_with_state(
    state: &McpDaemonState,
    request: &McpDaemonRequest,
) -> AppResult<McpDaemonResponse> {
    let mut stream = TcpStream::connect(("127.0.0.1", state.port))
        .code(EXIT_CONFIG, "failed to connect MCP daemon")?;
    let body = serde_json::to_string(request).map_err(|err| {
        AppError::new(
            EXIT_CONFIG,
            format!("failed to encode MCP daemon request: {err}"),
        )
    })?;
    stream
        .write_all(body.as_bytes())
        .code(EXIT_CONFIG, "failed to write MCP daemon request")?;
    stream.shutdown(Shutdown::Write).ok();
    let mut reply = String::new();
    stream
        .read_to_string(&mut reply)
        .code(EXIT_CONFIG, "failed to read MCP daemon response")?;
    serde_json::from_str(&reply).code(EXIT_CONFIG, "failed to parse MCP daemon response")
}

fn ping_mcp_daemon(state: &McpDaemonState) -> AppResult<()> {
    let response = call_mcp_daemon_with_state(
        state,
        &McpDaemonRequest {
            kind: "health".to_string(),
            full_name: None,
            arguments: None,
        },
    )?;
    if response.ok {
        return Ok(());
    }
    Err(AppError::new(
        EXIT_CONFIG,
        response
            .error
            .unwrap_or_else(|| "MCP daemon health check failed".to_string()),
    ))
}

fn write_daemon_response(stream: &mut TcpStream, response: &McpDaemonResponse) -> AppResult<()> {
    let body = serde_json::to_string(response).map_err(|err| {
        AppError::new(
            EXIT_CONFIG,
            format!("failed to encode MCP daemon response: {err}"),
        )
    })?;
    stream
        .write_all(body.as_bytes())
        .code(EXIT_CONFIG, "failed to write MCP daemon response")
}

fn persist_successful_probe_tools(
    paths: &AppPaths,
    config: &AppConfig,
    probes: &[McpServerProbe],
) -> AppResult<()> {
    let mut cache = load_mcp_cache(paths).unwrap_or_default();
    let enabled = enabled_mcp_servers(config);
    for probe in probes.iter().filter(|probe| probe.ok) {
        if let Some(server) = enabled.get(&probe.server) {
            set_cache_entry(&mut cache, &probe.server, server, probe.tools.clone());
        }
    }
    save_mcp_cache(paths, &cache)
}

fn persist_daemon_sessions_to_cache(
    paths: &AppPaths,
    sessions: &[DaemonServerSession],
) -> AppResult<()> {
    let mut cache = load_mcp_cache(paths).unwrap_or_default();
    for session in sessions {
        set_cache_entry(
            &mut cache,
            &session.name,
            &session.config,
            session.tools.clone(),
        );
    }
    save_mcp_cache(paths, &cache)
}

fn merge_tool_specs(existing: Vec<McpToolSpec>, fresh: Vec<McpToolSpec>) -> Vec<McpToolSpec> {
    let mut merged = BTreeMap::new();
    for tool in existing {
        merged.insert(tool.full_name.clone(), tool);
    }
    for tool in fresh {
        merged.insert(tool.full_name.clone(), tool);
    }
    merged.into_values().collect()
}

pub fn cached_mcp_tools_from_disk(paths: &AppPaths, config: &AppConfig) -> Vec<McpToolSpec> {
    if !mcp_enabled(config) {
        return Vec::new();
    }
    let cache = load_mcp_cache(paths).unwrap_or_default();
    enabled_mcp_servers(config)
        .into_iter()
        .filter_map(|(server_name, server)| {
            cache
                .servers
                .get(&server_name)
                .filter(|entry| server_matches_cache_entry(&server, entry))
                .map(|entry| entry.tools.clone())
        })
        .flatten()
        .collect()
}

pub fn hydrate_cached_mcp_tools(paths: &AppPaths, config: &AppConfig) -> Vec<McpToolSpec> {
    let tools = cached_mcp_tools_from_disk(paths, config);
    if !tools.is_empty() {
        set_cached_mcp_tools(config, tools.clone());
    }
    tools
}

pub fn merge_cached_mcp_tools(config: &AppConfig, tools: Vec<McpToolSpec>) -> Vec<McpToolSpec> {
    let merged = merge_tool_specs(cached_mcp_tools_for_config(config), tools);
    set_cached_mcp_tools(config, merged.clone());
    merged
}

pub fn list_mcp_tools_from_ready_daemon(
    paths: &AppPaths,
    config: &AppConfig,
) -> Option<Vec<McpToolSpec>> {
    if !mcp_enabled(config) {
        return Some(Vec::new());
    }
    let response = call_mcp_daemon(
        paths,
        &McpDaemonRequest {
            kind: "tools".to_string(),
            full_name: None,
            arguments: None,
        },
    )
    .ok()?;
    if !response.ok {
        return None;
    }
    Some(response.tools.unwrap_or_default())
}

pub fn start_mcp_warmup(paths: &AppPaths, config: &AppConfig) -> Option<McpWarmupHandle> {
    if enabled_mcp_servers(config).is_empty() {
        return None;
    }
    let paths = paths.clone();
    let config = config.clone();
    let state = Arc::new(Mutex::new(McpWarmupState {
        started_at_unix_ms: now_unix_ms(),
        result: None,
    }));
    let state_for_thread = state.clone();
    thread::spawn(move || {
        let probes = probe_mcp_servers(&config, None);
        let successful_tools = probes
            .iter()
            .filter(|probe| probe.ok)
            .flat_map(|probe| probe.tools.clone())
            .collect::<Vec<_>>();
        if !successful_tools.is_empty() {
            let _ = persist_successful_probe_tools(&paths, &config, &probes);
            let _ = merge_cached_mcp_tools(&config, successful_tools);
        }
        let mut guard = state_for_thread.lock().unwrap();
        if let Some(failed) = probes.iter().find(|probe| !probe.ok) {
            guard.result = Some(Err(AppError::new(
                EXIT_CONFIG,
                failed
                    .error
                    .clone()
                    .unwrap_or_else(|| format!("MCP server `{}` warmup failed", failed.server)),
            )));
        } else {
            guard.result = Some(Ok(probes));
        }
    });
    Some(McpWarmupHandle { state })
}

pub fn wait_for_mcp_warmup(
    handle: &McpWarmupHandle,
    timeout_sec: f64,
) -> Option<AppResult<Vec<McpServerProbe>>> {
    let timeout = duration_from_secs_f64(timeout_sec)?;
    let start = SystemTime::now();
    loop {
        if let Some(result) = handle.state.lock().unwrap().result.clone() {
            return Some(result);
        }
        if SystemTime::now().duration_since(start).unwrap_or_default() >= timeout {
            return None;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

pub fn warmup_timeout_warning(handle: &McpWarmupHandle) -> McpWarmupWarning {
    let started_at_unix_ms = handle.state.lock().unwrap().started_at_unix_ms;
    McpWarmupWarning {
        message: format!(
            "MCP warmup is still running after {} ms; continuing without blocking until an MCP tool is used",
            now_unix_ms().saturating_sub(started_at_unix_ms)
        ),
    }
}

pub fn enabled_mcp_servers(config: &AppConfig) -> BTreeMap<String, McpServerConfig> {
    if !mcp_enabled(config) {
        return BTreeMap::new();
    }
    config
        .mcp
        .iter()
        .filter(|(_, server)| server.enabled)
        .map(|(name, server)| (name.clone(), server.clone()))
        .collect()
}

pub fn validate_mcp_config(config: &AppConfig) -> Vec<String> {
    let mut issues = Vec::new();
    for (name, server) in &config.mcp {
        if server.command.trim().is_empty() {
            issues.push(format!("mcp.{name}.command cannot be empty"));
        }
        if let Some(kind) = server.r#type.as_deref()
            && kind != "stdio"
        {
            issues.push(format!("mcp.{name}.type must be 'stdio'"));
        }
        if let Some(timeout) = server.startup_timeout_sec
            && timeout <= 0.0
        {
            issues.push(format!(
                "mcp.{name}.startup_timeout_sec must be greater than 0"
            ));
        }
        if let Some(timeout) = server.tool_timeout_sec
            && timeout <= 0.0
        {
            issues.push(format!(
                "mcp.{name}.tool_timeout_sec must be greater than 0"
            ));
        }
    }
    issues
}

pub fn mcp_tool_definitions(config: &AppConfig) -> Vec<Value> {
    cached_mcp_tools_for_config(config)
        .into_iter()
        .map(|tool| define_mcp_tool(&tool))
        .collect()
}

pub fn mcp_tool_definition_for_name(config: &AppConfig, name: &str) -> Option<Value> {
    find_cached_mcp_tool(config, name).map(|tool| define_mcp_tool(&tool))
}

fn is_mcp_tool_search_all_query(query: &str) -> bool {
    matches!(query.trim().to_ascii_lowercase().as_str(), "all" | "*")
}

fn mcp_tool_search_limit(query: &str, max_results: usize) -> usize {
    if is_mcp_tool_search_all_query(query) {
        usize::MAX
    } else {
        max_results.max(1)
    }
}

fn mcp_tool_search_terms(query: &str) -> Vec<String> {
    query
        .split(|ch: char| ch.is_whitespace() || ch == ',' || ch == ';')
        .filter_map(|term| {
            let term = term
                .trim_matches(|ch: char| {
                    !ch.is_alphanumeric() && ch != '_' && ch != '-' && ch != ':'
                })
                .to_ascii_lowercase();
            (!term.is_empty()).then_some(term)
        })
        .collect()
}

fn fuzzy_subsequence_match(haystack: &str, needle: &str) -> bool {
    if needle.chars().count() < 3 {
        return false;
    }
    let mut haystack = haystack.chars();
    needle
        .chars()
        .all(|needle_ch| haystack.any(|haystack_ch| haystack_ch == needle_ch))
}

fn mcp_tool_field_score(
    field: &str,
    term: &str,
    exact_score: usize,
    contains_score: usize,
    fuzzy_score: usize,
) -> usize {
    let field = field.to_ascii_lowercase();
    let mut score = 0usize;
    if field == term {
        score += exact_score;
    }
    if field.contains(term) {
        score += contains_score;
    }
    if score == 0 && fuzzy_subsequence_match(&field, term) {
        score += fuzzy_score;
    }
    score
}

fn best_mcp_tool_field_score<'a>(
    term: &str,
    fields: impl IntoIterator<Item = (&'a str, usize, usize, usize)>,
) -> usize {
    fields
        .into_iter()
        .map(|(field, exact_score, contains_score, fuzzy_score)| {
            mcp_tool_field_score(field, term, exact_score, contains_score, fuzzy_score)
        })
        .max()
        .unwrap_or(0)
}

fn mcp_tool_search_score(tool: &McpToolSpec, query: &str) -> usize {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return 0;
    }
    if is_mcp_tool_search_all_query(&query) {
        return 1;
    }

    let mut score = best_mcp_tool_field_score(
        &query,
        [
            (tool.full_name.as_str(), 100, 50, 20),
            (tool.remote_name.as_str(), 100, 50, 20),
            (tool.description.as_str(), 0, 10, 3),
        ],
    );

    for term in mcp_tool_search_terms(&query) {
        score += best_mcp_tool_field_score(
            &term,
            [
                (tool.full_name.as_str(), 100, 50, 20),
                (tool.remote_name.as_str(), 100, 50, 20),
                (tool.description.as_str(), 0, 10, 3),
            ],
        );
    }

    score
}

pub fn search_mcp_tools(config: &AppConfig, query: &str, max_results: usize) -> Vec<McpToolSpec> {
    let limit = mcp_tool_search_limit(query, max_results);
    let mut matches = cached_mcp_tools_for_config(config)
        .into_iter()
        .map(|tool| {
            let score = mcp_tool_search_score(&tool, query);
            (score, tool)
        })
        .filter(|(score, _)| *score > 0)
        .collect::<Vec<_>>();
    matches.sort_by(|(left_score, left_tool), (right_score, right_tool)| {
        right_score
            .cmp(left_score)
            .then_with(|| left_tool.full_name.cmp(&right_tool.full_name))
    });
    matches
        .into_iter()
        .take(limit)
        .map(|(_, tool)| tool)
        .collect()
}

fn execute_mcp_tool_direct(
    config: &AppConfig,
    full_name: &str,
    arguments: &Value,
) -> AppResult<String> {
    execute_mcp_tool_direct_for_server(config, None, full_name, arguments)
}

fn execute_mcp_tool_direct_for_server(
    config: &AppConfig,
    only_server: Option<&str>,
    full_name: &str,
    arguments: &Value,
) -> AppResult<String> {
    let mut scoped = config.clone();
    if let Some(server_name) = only_server {
        scoped.mcp.retain(|name, _| name == server_name);
    }
    let tool = find_mcp_tool(&scoped, full_name)?
        .ok_or_else(|| AppError::new(EXIT_ARGS, format!("unknown MCP tool `{full_name}`")))?;
    let server = enabled_mcp_servers(&scoped)
        .remove(&tool.server)
        .ok_or_else(|| {
            AppError::new(EXIT_CONFIG, format!("missing MCP server `{}`", tool.server))
        })?;
    call_mcp_tool(&server, &tool.remote_name, arguments)
}

pub fn execute_mcp_tool_with_daemon(
    paths: &AppPaths,
    config: &AppConfig,
    full_name: &str,
    arguments: &Value,
) -> AppResult<String> {
    if !mcp_enabled(config) {
        return Err(AppError::new(EXIT_CONFIG, "mcp disabled by config"));
    }
    if let Ok(response) = call_mcp_daemon(
        paths,
        &McpDaemonRequest {
            kind: "call".to_string(),
            full_name: Some(full_name.to_string()),
            arguments: Some(arguments.clone()),
        },
    ) {
        if response.ok {
            return Ok(response.content.unwrap_or_default());
        }
        if let Some(error) = response.error {
            return Err(AppError::new(EXIT_CONFIG, error));
        }
    }
    execute_mcp_tool_direct(config, full_name, arguments)
}

pub fn execute_mcp_tool(
    config: &AppConfig,
    full_name: &str,
    arguments: &Value,
) -> AppResult<String> {
    execute_mcp_tool_direct(config, full_name, arguments)
}

pub fn expand_mcp_cwd(cwd: &Option<String>) -> Option<PathBuf> {
    cwd.as_deref().map(expand_tilde)
}

fn cache_key(config: &AppConfig) -> String {
    serde_json::to_string(&enabled_mcp_servers(config)).unwrap_or_default()
}

fn cached_mcp_tools_for_config(config: &AppConfig) -> Vec<McpToolSpec> {
    let key = cache_key(config);
    let cache = MCP_TOOL_CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    cache.lock().unwrap().get(&key).cloned().unwrap_or_default()
}

pub fn set_cached_mcp_tools(config: &AppConfig, tools: Vec<McpToolSpec>) {
    let key = cache_key(config);
    let cache = MCP_TOOL_CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    cache.lock().unwrap().insert(key, tools);
}

pub fn has_cached_mcp_tool(config: &AppConfig, full_name: &str) -> bool {
    find_cached_mcp_tool(config, full_name).is_some()
}

fn find_cached_mcp_tool(config: &AppConfig, full_name: &str) -> Option<McpToolSpec> {
    cached_mcp_tools_for_config(config)
        .into_iter()
        .find(|tool| tool.full_name == full_name)
}

fn list_mcp_tools(config: &AppConfig) -> AppResult<Vec<McpToolSpec>> {
    let mut tools = Vec::new();
    for (server_name, server) in enabled_mcp_servers(config) {
        let probe = probe_single_server(&server_name, &server);
        if !probe.ok {
            return Err(AppError::new(
                EXIT_CONFIG,
                probe
                    .error
                    .unwrap_or_else(|| format!("MCP server `{server_name}` probe failed")),
            ));
        }
        tools.extend(probe.tools);
    }
    set_cached_mcp_tools(config, tools.clone());
    Ok(tools)
}

fn find_mcp_tool(config: &AppConfig, full_name: &str) -> AppResult<Option<McpToolSpec>> {
    Ok(list_mcp_tools(config)?
        .into_iter()
        .find(|tool| tool.full_name == full_name))
}

fn decode_tool_specs(
    server_name: &str,
    server: &McpServerConfig,
    result: &Value,
) -> Vec<McpToolSpec> {
    let tools = result["tools"].as_array().cloned().unwrap_or_default();
    let enabled = &server.enabled_tools;
    let disabled = &server.disabled_tools;
    let mut specs = Vec::new();
    for tool in tools {
        let Some(remote_name) = tool["name"].as_str().map(str::to_string) else {
            continue;
        };
        if !enabled.is_empty() && !enabled.iter().any(|name| name == &remote_name) {
            continue;
        }
        if disabled.iter().any(|name| name == &remote_name) {
            continue;
        }
        let description = tool["description"].as_str().unwrap_or_default().to_string();
        let input_schema = tool
            .get("inputSchema")
            .cloned()
            .or_else(|| tool.get("input_schema").cloned())
            .unwrap_or_else(|| {
                json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": true
                })
            });
        let read_only = tool["annotations"]["readOnlyHint"]
            .as_bool()
            .unwrap_or(false);
        specs.push(McpToolSpec {
            full_name: format!("mcp__{server_name}__{remote_name}"),
            server: server_name.to_string(),
            remote_name,
            description,
            input_schema,
            read_only,
        });
    }
    specs
}

fn define_mcp_tool(tool: &McpToolSpec) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.full_name,
            "description": if tool.description.trim().is_empty() {
                format!("[MCP:{}] Call MCP tool `{}`.", tool.server, tool.remote_name)
            } else {
                format!("[MCP:{}] {}", tool.server, tool.description)
            },
            "parameters": tool.input_schema,
        }
    })
}

fn build_daemon_sessions(config: &AppConfig) -> AppResult<Vec<DaemonServerSession>> {
    let mut sessions = Vec::new();
    for (server_name, server) in enabled_mcp_servers(config) {
        let mut client = McpProcess::start(&server, true)?;
        client.initialize(server.startup_timeout_sec)?;
        let result = client.request("tools/list", json!({}), server.tool_timeout_sec)?;
        let tools = decode_tool_specs(&server_name, &server, &result);
        sessions.push(DaemonServerSession {
            name: server_name,
            config: server,
            client,
            tools,
        });
    }
    Ok(sessions)
}

fn call_daemon_session_tool(
    sessions: &mut [DaemonServerSession],
    full_name: &str,
    arguments: &Value,
) -> AppResult<String> {
    let Some(session) = sessions
        .iter_mut()
        .find(|session| session.tools.iter().any(|tool| tool.full_name == full_name))
    else {
        return Err(AppError::new(
            EXIT_ARGS,
            format!("unknown MCP tool `{full_name}`"),
        ));
    };
    let tool = session
        .tools
        .iter()
        .find(|tool| tool.full_name == full_name)
        .ok_or_else(|| AppError::new(EXIT_ARGS, format!("unknown MCP tool `{full_name}`")))?;
    let params = match arguments {
        Value::Object(map) => json!({
            "name": tool.remote_name,
            "arguments": map,
        }),
        Value::Null => json!({ "name": tool.remote_name }),
        other => {
            return Err(AppError::new(
                EXIT_ARGS,
                format!("MCP tool arguments must be a JSON object, got {other}"),
            ));
        }
    };
    let result = session
        .client
        .request("tools/call", params, session.config.tool_timeout_sec)?;
    render_call_tool_result(&result)
}

fn probe_single_server(server_name: &str, server: &McpServerConfig) -> McpServerProbe {
    let cwd = expand_mcp_cwd(&server.cwd);
    let mut debug = vec![format!(
        "start command={} args={:?} cwd={}",
        server.command,
        server.args,
        cwd.as_deref()
            .map(Path::display)
            .map(|display| display.to_string())
            .unwrap_or_else(|| "<inherit>".to_string())
    )];
    let mut client = match McpProcess::start(server, true) {
        Ok(client) => client,
        Err(err) => {
            debug.push(format!("spawn error: {}", err.message));
            return McpServerProbe {
                server: server_name.to_string(),
                ok: false,
                command: server.command.clone(),
                tool_count: 0,
                tools: Vec::new(),
                debug,
                error: Some(err.message),
            };
        }
    };
    match client.initialize(server.startup_timeout_sec) {
        Ok(()) => debug.push("initialize ok".to_string()),
        Err(err) => {
            debug.extend(client.take_debug_lines());
            debug.push(format!("initialize error: {}", err.message));
            return McpServerProbe {
                server: server_name.to_string(),
                ok: false,
                command: server.command.clone(),
                tool_count: 0,
                tools: Vec::new(),
                debug,
                error: Some(err.message),
            };
        }
    }
    let result = match client.request("tools/list", json!({}), server.tool_timeout_sec) {
        Ok(result) => result,
        Err(err) => {
            debug.extend(client.take_debug_lines());
            debug.push(format!("tools/list error: {}", err.message));
            return McpServerProbe {
                server: server_name.to_string(),
                ok: false,
                command: server.command.clone(),
                tool_count: 0,
                tools: Vec::new(),
                debug,
                error: Some(err.message),
            };
        }
    };
    let tools = decode_tool_specs(server_name, server, &result);
    debug.extend(client.take_debug_lines());
    debug.push(format!("tools/list ok count={}", tools.len()));
    McpServerProbe {
        server: server_name.to_string(),
        ok: true,
        command: server.command.clone(),
        tool_count: tools.len(),
        tools,
        debug,
        error: None,
    }
}

fn call_mcp_tool(
    server: &McpServerConfig,
    remote_name: &str,
    arguments: &Value,
) -> AppResult<String> {
    let mut client = McpProcess::start(server, false)?;
    client.initialize(server.startup_timeout_sec)?;
    let params = match arguments {
        Value::Object(map) => json!({
            "name": remote_name,
            "arguments": map,
        }),
        Value::Null => json!({ "name": remote_name }),
        other => {
            return Err(AppError::new(
                EXIT_ARGS,
                format!("MCP tool arguments must be a JSON object, got {other}"),
            ));
        }
    };
    let result = client.request("tools/call", params, server.tool_timeout_sec)?;
    render_call_tool_result(&result)
}

fn render_call_tool_result(result: &Value) -> AppResult<String> {
    let mut blocks = Vec::new();
    if let Some(content) = result["content"].as_array() {
        for item in content {
            if let Some(text) = item["text"].as_str() {
                if !text.trim().is_empty() {
                    blocks.push(text.to_string());
                }
            } else if !item.is_null() {
                blocks.push(serde_json::to_string_pretty(item).map_err(|err| {
                    AppError::new(EXIT_CONFIG, format!("failed to render MCP content: {err}"))
                })?);
            }
        }
    }
    let structured = result
        .get("structuredContent")
        .or_else(|| result.get("structured_content"));
    if let Some(structured) = structured
        && !structured.is_null()
    {
        blocks.push(serde_json::to_string_pretty(structured).map_err(|err| {
            AppError::new(
                EXIT_CONFIG,
                format!("failed to render MCP structured content: {err}"),
            )
        })?);
    }
    if blocks.is_empty() {
        blocks.push(serde_json::to_string_pretty(result).map_err(|err| {
            AppError::new(EXIT_CONFIG, format!("failed to render MCP result: {err}"))
        })?);
    }
    Ok(blocks.join("\n\n"))
}

impl McpProcess {
    fn start(server: &McpServerConfig, capture_debug: bool) -> AppResult<Self> {
        let mut command = Command::new(&server.command);
        command.args(&server.args);
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        if let Some(cwd) = expand_mcp_cwd(&server.cwd) {
            command.current_dir(cwd);
        }
        for (key, value) in &server.env {
            command.env(key, value);
        }
        let mut child = command.spawn().code(
            EXIT_CONFIG,
            format!("failed to start MCP server `{}`", server.command),
        )?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AppError::new(EXIT_CONFIG, "failed to capture MCP stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AppError::new(EXIT_CONFIG, "failed to capture MCP stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| AppError::new(EXIT_CONFIG, "failed to capture MCP stderr"))?;
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else {
                    break;
                };
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    let _ = sender.send(value);
                }
            }
        });
        let stderr_receiver = Some(spawn_stderr_reader(stderr, capture_debug));
        Ok(Self {
            child,
            stdin,
            receiver,
            stderr_receiver,
            next_id: 1,
        })
    }

    fn initialize(&mut self, timeout_sec: Option<f64>) -> AppResult<()> {
        let params = json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": "chat-cli",
                "version": env!("CARGO_PKG_VERSION")
            }
        });
        let _ = self.request("initialize", params, timeout_sec)?;
        self.notify("notifications/initialized", None)
    }

    fn notify(&mut self, method: &str, params: Option<Value>) -> AppResult<()> {
        let payload = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write_message(&payload)
    }

    fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout_sec: Option<f64>,
    ) -> AppResult<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.write_message(&payload)?;
        let timeout = timeout_sec.and_then(duration_from_secs_f64);
        loop {
            let message = match timeout {
                Some(timeout) => self.receiver.recv_timeout(timeout).map_err(|_| {
                    AppError::new(
                        EXIT_CONFIG,
                        format!(
                            "timed out waiting for MCP response to `{method}` after {timeout:?}"
                        ),
                    )
                })?,
                None => self.receiver.recv().map_err(|_| {
                    AppError::new(
                        EXIT_CONFIG,
                        format!("MCP server closed while waiting for `{method}` response"),
                    )
                })?,
            };
            if message["id"].as_u64() != Some(id) {
                continue;
            }
            if !message["error"].is_null() {
                let error = &message["error"];
                let detail = error["message"].as_str().unwrap_or("unknown MCP error");
                return Err(AppError::new(
                    EXIT_CONFIG,
                    format!("MCP request `{method}` failed: {detail}"),
                ));
            }
            return Ok(message["result"].clone());
        }
    }

    fn write_message(&mut self, payload: &Value) -> AppResult<()> {
        let encoded = serde_json::to_string(payload).map_err(|err| {
            AppError::new(EXIT_CONFIG, format!("failed to encode MCP payload: {err}"))
        })?;
        writeln!(self.stdin, "{encoded}").code(EXIT_CONFIG, "failed to write MCP request")?;
        self.stdin
            .flush()
            .code(EXIT_CONFIG, "failed to flush MCP request")
    }

    fn take_debug_lines(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(receiver) = &self.stderr_receiver {
            while let Ok(line) = receiver.try_recv() {
                if !line.trim().is_empty() {
                    lines.push(format!("stderr: {line}"));
                }
            }
        }
        lines
    }
}

fn spawn_stderr_reader(stderr: ChildStderr, forward_lines: bool) -> mpsc::Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            let Ok(line) = line else {
                break;
            };
            if forward_lines {
                let _ = sender.send(line);
            }
        }
    });
    receiver
}

fn duration_from_secs_f64(value: f64) -> Option<Duration> {
    (value > 0.0).then(|| Duration::from_secs_f64(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_paths() -> (AppPaths, PathBuf) {
        let base = std::env::temp_dir().join(format!("chat-cli-mcp-test-{}", ulid::Ulid::new()));
        let config_dir = base.join("config");
        let data_dir = base.join("data");
        let cache_dir = base.join("cache");
        let paths = AppPaths {
            config_dir: config_dir.clone(),
            data_dir: data_dir.clone(),
            cache_dir,
            config_file: config_dir.join("config.toml"),
            secrets_file: config_dir.join("secrets.toml"),
            state_file: data_dir.join("state.toml"),
        };
        (paths, base)
    }

    #[test]
    fn validate_mcp_config_rejects_empty_command() {
        let mut config = AppConfig::default();
        config.mcp.insert(
            "demo".to_string(),
            McpServerConfig {
                command: String::new(),
                ..McpServerConfig::default()
            },
        );
        let issues = validate_mcp_config(&config);
        assert!(
            issues
                .iter()
                .any(|issue| issue.contains("mcp.demo.command cannot be empty"))
        );
    }

    #[test]
    fn search_mcp_tools_returns_empty_without_servers() {
        let config = AppConfig::default();
        assert!(search_mcp_tools(&config, "calendar", 5).is_empty());
    }

    #[test]
    fn search_mcp_tools_supports_multi_term_fuzzy_case_insensitive_and_all() {
        let server_name = format!("toolsearch{}", ulid::Ulid::new());
        let mut config = AppConfig::default();
        config.tools.mcp = Some(true);
        config.mcp.insert(
            server_name.clone(),
            McpServerConfig {
                command: "demo".to_string(),
                ..McpServerConfig::default()
            },
        );
        let tools = vec![
            McpToolSpec {
                full_name: format!("mcp__{server_name}__execute_command"),
                server: server_name.clone(),
                remote_name: "execute_command".to_string(),
                description: "Execute shell command".to_string(),
                input_schema: json!({"type":"object"}),
                read_only: false,
            },
            McpToolSpec {
                full_name: format!("mcp__{server_name}__codebase-retrieval"),
                server: server_name.clone(),
                remote_name: "codebase-retrieval".to_string(),
                description: "Semantic code search".to_string(),
                input_schema: json!({"type":"object"}),
                read_only: true,
            },
        ];
        set_cached_mcp_tools(&config, tools);

        let phrase_matches = search_mcp_tools(&config, "execute command", 5);
        assert_eq!(phrase_matches[0].remote_name, "execute_command");

        let multi_matches = search_mcp_tools(&config, "EXECUTE codebase", 5);
        let multi_names = multi_matches
            .iter()
            .map(|tool| tool.remote_name.as_str())
            .collect::<Vec<_>>();
        assert!(multi_names.contains(&"execute_command"));
        assert!(multi_names.contains(&"codebase-retrieval"));

        let fuzzy_matches = search_mcp_tools(&config, "cdbs", 5);
        assert_eq!(fuzzy_matches[0].remote_name, "codebase-retrieval");

        let all_matches = search_mcp_tools(&config, "all", 1);
        assert_eq!(all_matches.len(), 2);
    }

    #[test]
    fn mcp_cache_roundtrip_works() {
        let (paths, _base) = temp_paths();
        save_mcp_cache(
            &paths,
            &McpCache {
                servers: BTreeMap::from([(
                    "ace".to_string(),
                    McpServerCacheEntry {
                        server: "ace".to_string(),
                        command: "auggie".to_string(),
                        args: vec!["--mcp".to_string()],
                        cwd: None,
                        enabled_tools: Vec::new(),
                        disabled_tools: Vec::new(),
                        tools: vec![McpToolSpec {
                            full_name: "mcp__ace__codebase-retrieval".to_string(),
                            server: "ace".to_string(),
                            remote_name: "codebase-retrieval".to_string(),
                            description: "Search".to_string(),
                            input_schema: json!({"type":"object"}),
                            read_only: true,
                        }],
                        checked_at_unix_ms: 1,
                    },
                )]),
            },
        )
        .unwrap();
        let cache = load_mcp_cache(&paths).unwrap();
        assert_eq!(cache.servers.len(), 1);
        assert_eq!(
            cache.servers["ace"].tools[0].full_name,
            "mcp__ace__codebase-retrieval"
        );
    }

    #[test]
    fn warmup_timeout_warning_mentions_running_state() {
        let handle = McpWarmupHandle {
            state: Arc::new(Mutex::new(McpWarmupState {
                started_at_unix_ms: now_unix_ms().saturating_sub(50),
                result: None,
            })),
        };
        let warning = warmup_timeout_warning(&handle);
        assert!(warning.message.contains("still running"));
    }

    #[test]
    fn daemon_state_roundtrip_works() {
        let (paths, _base) = temp_paths();
        let state = McpDaemonState {
            pid: 123,
            port: 4567,
            started_at_unix_ms: 1,
            log_file: mcp_daemon_log_path(&paths),
            server: Some("ace".to_string()),
        };
        save_mcp_daemon_state(&paths, &state).unwrap();
        let loaded = load_mcp_daemon_state(&paths).unwrap();
        assert_eq!(loaded.pid, 123);
        assert_eq!(loaded.port, 4567);
        assert_eq!(loaded.server.as_deref(), Some("ace"));
    }

    #[test]
    fn current_mcp_daemon_status_reports_missing_state_as_not_running() {
        let (paths, _base) = temp_paths();
        let status = current_mcp_daemon_status(&paths);
        assert!(!status.running);
        assert!(!status.registered);
        assert!(status.pid.is_none());
        assert!(status.port.is_none());
        assert!(status.error.is_none());
    }

    #[test]
    fn current_mcp_daemon_status_reports_stale_registration() {
        let (paths, _base) = temp_paths();
        let state = McpDaemonState {
            pid: 123,
            port: 4567,
            started_at_unix_ms: 1,
            log_file: mcp_daemon_log_path(&paths),
            server: Some("ace".to_string()),
        };
        save_mcp_daemon_state(&paths, &state).unwrap();

        let status = current_mcp_daemon_status(&paths);
        assert!(status.registered);
        assert!(!status.running);
        assert_eq!(status.pid, Some(123));
        assert_eq!(status.port, Some(4567));
        assert!(status.error.is_some());
    }

    #[test]
    fn cached_mcp_tools_from_disk_filters_to_matching_servers() {
        let (paths, _base) = temp_paths();
        let mut config = AppConfig::default();
        config.tools.mcp = Some(true);
        config.mcp.insert(
            "ace".to_string(),
            McpServerConfig {
                command: "auggie".to_string(),
                args: vec!["serve".to_string()],
                ..McpServerConfig::default()
            },
        );
        save_mcp_cache(
            &paths,
            &McpCache {
                servers: BTreeMap::from([
                    (
                        "ace".to_string(),
                        McpServerCacheEntry {
                            server: "ace".to_string(),
                            command: "auggie".to_string(),
                            args: vec!["serve".to_string()],
                            cwd: None,
                            enabled_tools: Vec::new(),
                            disabled_tools: Vec::new(),
                            tools: vec![McpToolSpec {
                                full_name: "mcp__ace__calendar".to_string(),
                                server: "ace".to_string(),
                                remote_name: "calendar".to_string(),
                                description: "Calendar".to_string(),
                                input_schema: json!({"type":"object"}),
                                read_only: true,
                            }],
                            checked_at_unix_ms: 1,
                        },
                    ),
                    (
                        "stale".to_string(),
                        McpServerCacheEntry {
                            server: "stale".to_string(),
                            command: "old".to_string(),
                            args: vec![],
                            cwd: None,
                            enabled_tools: Vec::new(),
                            disabled_tools: Vec::new(),
                            tools: vec![McpToolSpec {
                                full_name: "mcp__stale__tool".to_string(),
                                server: "stale".to_string(),
                                remote_name: "tool".to_string(),
                                description: "Old".to_string(),
                                input_schema: json!({"type":"object"}),
                                read_only: true,
                            }],
                            checked_at_unix_ms: 1,
                        },
                    ),
                ]),
            },
        )
        .unwrap();

        let tools = cached_mcp_tools_from_disk(&paths, &config);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].full_name, "mcp__ace__calendar");
    }
}

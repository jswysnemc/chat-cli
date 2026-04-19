use crate::cli::{
    AskArgs, AuthCommand, AuthSetArgs, Cli, Commands, ConfigCommand, McpArgs, McpAuthArgs,
    McpCommand, McpStartArgs, ModelCommand, ModelSetArgs, OutputFormat, ProviderCommand, ReplArgs,
    SessionCommand,
};
use crate::config::{
    AppConfig, AppPaths, ModelConfig, ModelPatchConfig, ProviderConfig, ProviderSecret,
    SecretsConfig, apply_runtime_config_defaults, ensure_dirs, init_config_files, load_config,
    load_secrets, parse_headers, read_system_prompt, render_config_value, save_config,
    save_secrets, set_config_value, validate_config,
};
use crate::context::{ContextStatusMode, prepend_context_status};
use crate::error::{
    AppError, AppResult, EXIT_ARGS, EXIT_AUTH, EXIT_CONFIG, EXIT_MODEL, EXIT_PROVIDER,
};
use crate::mcp::{
    McpWarmupHandle, authenticate_and_cache_mcp, current_mcp_daemon_status,
    current_mcp_server_statuses, has_cached_mcp_tool, hydrate_cached_mcp_tools,
    list_mcp_tools_from_ready_daemon, load_mcp_cache, mcp_enabled, run_mcp_daemon,
    start_mcp_daemon_process, start_mcp_warmup, stop_mcp_daemon, wait_for_mcp_warmup,
    warmup_timeout_warning,
};
use crate::media::{MessageImage, read_clipboard_image, read_clipboard_text, read_image_inputs};
use crate::output::{AskOutput, AssistantMessage, render_ask_output};
use crate::provider::{
    ChatMessage, ChatRequest, ChatResponse, send_chat, stream_chat, test_provider,
};
use crate::render::{
    StreamPhase, StreamRenderer, StreamStatus, print_status_bar, render_markdown,
    render_markdown_with_width, wrap_ansi_to_width,
};
use crate::session::{
    SessionAudit, SessionEvent, SessionMessage, SessionResponse, append_events, clear_sessions,
    delete_session, gc_sessions, generate_session_id, generate_temp_session_id, is_temp_session,
    list_session_summaries, load_state, now_rfc3339, read_events, resolve_session_id,
    set_current_session, short_id,
};
#[cfg(test)]
use crate::tool::execute_tool;
use crate::tool::{
    continue_bash_session, execute_tool_with_context_and_paths, initial_tool_definitions,
    list_bash_sessions, lookup_tool_spec, parse_tool_call, tool_call_requires_confirmation,
    tool_call_side_effects, tool_definitions_for_names, tool_search_matches,
};
use clap::CommandFactory;
use crossterm::cursor::{self, MoveTo};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::queue;
use crossterm::terminal::{self, Clear, ClearType};
use serde::Deserialize;
use serde_json::{Value, json};
use std::cmp::min;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const CYAN: &str = "\x1b[36m";
const DIM: &str = "\x1b[2m";
const GREEN: &str = "\x1b[32m";
const MAGENTA: &str = "\x1b[35m";
const RED: &str = "\x1b[31m";
const RESET: &str = "\x1b[0m";
const YELLOW: &str = "\x1b[33m";

pub async fn run(cli: Cli) -> AppResult<()> {
    let root = cli.clone();
    let paths = AppPaths::from_overrides(cli.config_dir.clone(), cli.data_dir.clone())?;
    match &cli.command {
        Commands::Completion { shell } => {
            let mut cmd = Cli::command();
            clap_complete::generate(*shell, &mut cmd, "chat", &mut io::stdout());
            return Ok(());
        }
        Commands::Config {
            command: ConfigCommand::Init,
        } => {
            init_config_files(&paths)?;
            println!("initialized config at {}", paths.config_file.display());
            return Ok(());
        }
        Commands::Mcp(args)
            if matches!(args.command, Some(McpCommand::Start(_)))
                && std::env::var("CHAT_CLI_MCP_DAEMON").ok().as_deref() == Some("1") =>
        {
            let mut config = load_config(&paths)?;
            apply_runtime_config_defaults(&paths, &mut config);
            ensure_dirs(&paths, &config)?;
            let only_server = match &args.command {
                Some(McpCommand::Start(serve)) => serve.server.as_deref(),
                _ => None,
            };
            return run_mcp_daemon(&paths, &config, only_server);
        }
        _ => {}
    }

    let mut config = load_config(&paths)?;
    apply_runtime_config_defaults(&paths, &mut config);
    ensure_dirs(&paths, &config)?;
    let mut secrets = load_secrets(&paths)?;

    match cli.command {
        Commands::Ask(args) => handle_ask(&root, &paths, &config, &secrets, args).await,
        Commands::Repl(args) => handle_repl(&root, &paths, &mut config, &secrets, args).await,
        Commands::Mcp(args) => handle_mcp(&paths, &config, args),
        Commands::Session { command } => handle_session(&paths, &config, command),
        Commands::Config { command } => {
            handle_config(&paths, &mut config, &mut secrets, command).await
        }
        Commands::Doctor => handle_doctor(&paths, &config, &secrets).await,
        Commands::Thinking => match crate::render::load_thinking() {
            Some(content) => {
                println!("{}", render_markdown(&content, false));
                Ok(())
            }
            None => {
                eprintln!("no thinking content available");
                Ok(())
            }
        },
        Commands::Completion { .. } => Ok(()),
    }
}

fn handle_mcp(paths: &AppPaths, config: &AppConfig, args: McpArgs) -> AppResult<()> {
    match args.command {
        Some(McpCommand::Auth(auth)) => handle_mcp_auth(paths, config, auth),
        Some(McpCommand::Start(serve)) => handle_mcp_start(paths, serve),
        Some(McpCommand::Stop) => handle_mcp_stop(paths),
        Some(McpCommand::Status) => handle_mcp_status(paths, config),
        None => handle_mcp_auth(
            paths,
            config,
            McpAuthArgs {
                server: args.server,
                no_cache: args.no_cache,
                verbose: args.verbose,
            },
        ),
    }
}

fn handle_mcp_auth(paths: &AppPaths, config: &AppConfig, args: McpAuthArgs) -> AppResult<()> {
    let probes = authenticate_and_cache_mcp(paths, config, args.server.as_deref(), !args.no_cache)?;
    for probe in probes {
        println!("server: {}", probe.server);
        println!("status: {}", if probe.ok { "ok" } else { "error" });
        println!("command: {}", probe.command);
        println!("tools: {}", probe.tool_count);
        if args.verbose || !probe.ok {
            for line in probe.debug {
                println!("debug: {line}");
            }
        }
        if probe.ok {
            for tool in probe.tools {
                println!("tool: {}", tool.full_name);
            }
        }
        if let Some(error) = probe.error {
            println!("error: {error}");
        }
    }
    if !args.no_cache {
        let cache = load_mcp_cache(paths)?;
        println!("cached_servers: {}", cache.servers.len());
    }
    Ok(())
}

fn handle_mcp_start(paths: &AppPaths, args: McpStartArgs) -> AppResult<()> {
    let started = start_mcp_daemon_process(paths, args.server.as_deref())?;
    println!("mcp daemon started");
    println!("pid_file: {}", started.pid_file.display());
    println!("log_file: {}", started.log_file.display());
    if let Some(server) = started.server {
        println!("server: {server}");
    }
    Ok(())
}

fn handle_mcp_stop(paths: &AppPaths) -> AppResult<()> {
    let stopped = stop_mcp_daemon(paths)?;
    println!("mcp daemon stopped");
    println!("pid: {}", stopped.pid);
    Ok(())
}

fn ensure_mcp_daemon_started(paths: &AppPaths, config: &AppConfig) -> AppResult<()> {
    if cfg!(test) || !mcp_enabled(config) {
        return Ok(());
    }
    let daemon = current_mcp_daemon_status(paths);
    if daemon.running {
        return Ok(());
    }
    let _ = start_mcp_daemon_process(paths, None)?;
    Ok(())
}

fn handle_mcp_status(paths: &AppPaths, config: &AppConfig) -> AppResult<()> {
    let daemon = current_mcp_daemon_status(paths);
    println!("mcp_enabled: {}", mcp_enabled(config));
    println!("daemon_registered: {}", daemon.registered);
    println!("daemon_running: {}", daemon.running);
    if let Some(pid) = daemon.pid {
        println!("daemon_pid: {pid}");
    }
    if let Some(port) = daemon.port {
        println!("daemon_port: {port}");
    }
    println!("daemon_log: {}", daemon.log_file.display());
    if let Some(server) = daemon.server {
        println!("daemon_server: {server}");
    }
    if let Some(error) = daemon.error {
        println!("daemon_error: {error}");
    }
    for server in current_mcp_server_statuses(paths, config) {
        println!("server: {}", server.server);
        println!("enabled: {}", server.enabled);
        println!("cache_match: {}", server.cached);
        println!("cache_tools: {}", server.cached_tools);
        println!("live_ok: {}", server.live_ok);
        println!("live_tools: {}", server.live_tools);
        if let Some(error) = server.error {
            println!("error: {error}");
        }
    }
    Ok(())
}

async fn handle_config(
    paths: &AppPaths,
    config: &mut AppConfig,
    secrets: &mut SecretsConfig,
    command: ConfigCommand,
) -> AppResult<()> {
    match command {
        ConfigCommand::Init => unreachable!(),
        ConfigCommand::Path => {
            println!("config_dir={}", paths.config_dir.display());
            println!("config_file={}", paths.config_file.display());
            println!("secrets_file={}", paths.secrets_file.display());
            println!("data_dir={}", paths.data_dir.display());
            println!("cache_dir={}", paths.cache_dir.display());
            println!("sessions_dir={}", paths.sessions_dir(config).display());
            Ok(())
        }
        ConfigCommand::Show => {
            let text = toml::to_string_pretty(config).map_err(|err| {
                AppError::new(EXIT_CONFIG, format!("failed to render config: {err}"))
            })?;
            print!("{text}");
            Ok(())
        }
        ConfigCommand::Get { key } => {
            println!("{}", render_config_value(config, &key)?);
            Ok(())
        }
        ConfigCommand::Set { key, value } => {
            set_config_value(config, &key, &value)?;
            save_config(paths, config)?;
            println!("updated {key}");
            Ok(())
        }
        ConfigCommand::Validate => {
            let issues = validate_config(config);
            if issues.is_empty() {
                println!("config valid");
                Ok(())
            } else {
                for issue in issues {
                    eprintln!("{issue}");
                }
                Err(AppError::new(EXIT_CONFIG, "config validation failed"))
            }
        }
        ConfigCommand::Provider { command } => {
            handle_provider_command(paths, config, secrets, command).await
        }
        ConfigCommand::Model { command } => handle_model_command(paths, config, command),
        ConfigCommand::Auth { command } => handle_auth_command(paths, config, secrets, command),
    }
}

async fn handle_provider_command(
    paths: &AppPaths,
    config: &mut AppConfig,
    secrets: &SecretsConfig,
    command: ProviderCommand,
) -> AppResult<()> {
    match command {
        ProviderCommand::Set(args) => {
            let provider = ProviderConfig {
                kind: args.kind,
                base_url: args.base_url,
                api_key_env: args.api_key_env,
                headers: parse_headers(&args.headers)?,
                org: args.org,
                project: args.project,
                default_model: args.default_model,
                timeout: args.timeout,
            };
            config.providers.insert(args.id.clone(), provider);
            save_config(paths, config)?;
            println!("saved provider {}", args.id);
            Ok(())
        }
        ProviderCommand::List => {
            for (id, provider) in &config.providers {
                println!(
                    "{} kind={} base_url={}",
                    id,
                    provider.kind,
                    provider.base_url.as_deref().unwrap_or("")
                );
            }
            Ok(())
        }
        ProviderCommand::Get { id } => {
            let provider = config.providers.get(&id).ok_or_else(|| {
                AppError::new(EXIT_PROVIDER, format!("provider `{id}` does not exist"))
            })?;
            let text = toml::to_string_pretty(provider).map_err(|err| {
                AppError::new(
                    EXIT_CONFIG,
                    format!("failed to render provider `{id}`: {err}"),
                )
            })?;
            print!("{text}");
            Ok(())
        }
        ProviderCommand::Remove { id } => {
            if config.providers.remove(&id).is_none() {
                return Err(AppError::new(
                    EXIT_PROVIDER,
                    format!("provider `{id}` does not exist"),
                ));
            }
            if config.defaults.provider.as_deref() == Some(id.as_str()) {
                config.defaults.provider = None;
            }
            if config
                .defaults
                .model
                .as_ref()
                .and_then(|model_id| config.models.get(model_id))
                .is_some_and(|model| model.provider == id)
            {
                config.defaults.model = None;
            }
            save_config(paths, config)?;
            println!("removed provider {id}");
            Ok(())
        }
        ProviderCommand::Test { id } => {
            let provider = config.providers.get(&id).ok_or_else(|| {
                AppError::new(EXIT_PROVIDER, format!("provider `{id}` does not exist"))
            })?;
            let api_key = resolve_api_key(&id, provider, secrets)?;
            test_provider(&id, provider, &api_key, &config.models).await?;
            println!("provider {id} ok");
            Ok(())
        }
    }
}

fn handle_model_command(
    paths: &AppPaths,
    config: &mut AppConfig,
    command: ModelCommand,
) -> AppResult<()> {
    match command {
        ModelCommand::Set(ModelSetArgs {
            id,
            provider,
            remote_name,
            display_name,
            context_window,
            max_output_tokens,
            capabilities,
            temperature,
            reasoning_effort,
            patch_system_to_user,
        }) => {
            if !config.providers.contains_key(&provider) {
                return Err(AppError::new(
                    EXIT_PROVIDER,
                    format!("provider `{provider}` does not exist"),
                ));
            }
            config.models.insert(
                id.clone(),
                ModelConfig {
                    provider,
                    remote_name,
                    display_name,
                    context_window,
                    max_output_tokens,
                    capabilities,
                    temperature,
                    reasoning_effort,
                    patches: ModelPatchConfig {
                        system_to_user: patch_system_to_user.then_some(true),
                    },
                },
            );
            save_config(paths, config)?;
            println!("saved model {id}");
            Ok(())
        }
        ModelCommand::List { provider } => {
            for model in config.models.values() {
                if provider.as_ref().is_some_and(|p| p != &model.provider) {
                    continue;
                }
                println!("{}", format_model_list_entry(model));
            }
            Ok(())
        }
        ModelCommand::Get { id } => {
            let model = config
                .models
                .get(&id)
                .ok_or_else(|| AppError::new(EXIT_MODEL, format!("model `{id}` does not exist")))?;
            let text = toml::to_string_pretty(model).map_err(|err| {
                AppError::new(EXIT_CONFIG, format!("failed to render model `{id}`: {err}"))
            })?;
            print!("{text}");
            Ok(())
        }
        ModelCommand::Use { target } => {
            let (provider_id, model_id) = resolve_model_use_target(config, &target)?;
            config.defaults.model = Some(model_id.clone());
            config.defaults.provider = Some(provider_id.clone());
            save_config(paths, config)?;
            println!("default model set to {} provider={}", model_id, provider_id);
            Ok(())
        }
        ModelCommand::Remove { id } => {
            if config.models.remove(&id).is_none() {
                return Err(AppError::new(
                    EXIT_MODEL,
                    format!("model `{id}` does not exist"),
                ));
            }
            if config.defaults.model.as_deref() == Some(id.as_str()) {
                config.defaults.model = None;
            }
            save_config(paths, config)?;
            println!("removed model {id}");
            Ok(())
        }
    }
}

fn handle_auth_command(
    paths: &AppPaths,
    config: &mut AppConfig,
    secrets: &mut SecretsConfig,
    command: AuthCommand,
) -> AppResult<()> {
    match command {
        AuthCommand::Set(args) => handle_auth_set(paths, config, secrets, args),
        AuthCommand::Status { provider_id } => {
            if let Some(provider_id) = provider_id {
                print_auth_status(config, secrets, &provider_id)?;
            } else {
                for provider_id in config.providers.keys() {
                    print_auth_status(config, secrets, provider_id)?;
                }
            }
            Ok(())
        }
        AuthCommand::Remove { provider_id } => {
            let mut removed = false;
            if let Some(secret) = secrets.providers.get_mut(&provider_id) {
                secret.api_key = None;
                removed = true;
            }
            if removed {
                save_secrets(paths, secrets)?;
            }
            println!("removed auth for {provider_id}");
            Ok(())
        }
    }
}

fn handle_auth_set(
    paths: &AppPaths,
    config: &mut AppConfig,
    secrets: &mut SecretsConfig,
    args: AuthSetArgs,
) -> AppResult<()> {
    if !config.providers.contains_key(&args.provider_id) {
        return Err(AppError::new(
            EXIT_PROVIDER,
            format!("provider `{}` does not exist", args.provider_id),
        ));
    }
    if let Some(env_name) = args.env {
        let provider = config
            .providers
            .get_mut(&args.provider_id)
            .expect("checked above");
        provider.api_key_env = Some(env_name.clone());
        save_config(paths, config)?;
        println!(
            "provider {} now reads API key from env {}",
            args.provider_id, env_name
        );
        return Ok(());
    }

    let value = if let Some(value) = args.value {
        value
    } else if args.stdin {
        read_stdin_all()?
    } else {
        return Err(AppError::new(
            EXIT_ARGS,
            "config auth set requires --value, --stdin, or --env",
        ));
    };
    secrets.providers.insert(
        args.provider_id.clone(),
        ProviderSecret {
            api_key: Some(value.trim().to_string()),
        },
    );
    save_secrets(paths, secrets)?;
    println!("stored auth for {}", args.provider_id);
    Ok(())
}

fn print_auth_status(
    config: &AppConfig,
    secrets: &SecretsConfig,
    provider_id: &str,
) -> AppResult<()> {
    let provider = config.providers.get(provider_id).ok_or_else(|| {
        AppError::new(
            EXIT_PROVIDER,
            format!("provider `{provider_id}` does not exist"),
        )
    })?;
    let env_name = provider.api_key_env.clone().unwrap_or_default();
    let env_present = provider
        .api_key_env
        .as_ref()
        .is_some_and(|name| std::env::var(name).is_ok());
    let file_present = secrets
        .providers
        .get(provider_id)
        .and_then(|secret| secret.api_key.as_ref())
        .is_some();
    println!(
        "{} env={} env_present={} file_present={}",
        provider_id, env_name, env_present as u8, file_present as u8
    );
    Ok(())
}

fn handle_session(paths: &AppPaths, config: &AppConfig, command: SessionCommand) -> AppResult<()> {
    match command {
        SessionCommand::List => {
            let current_session = load_state(paths)?.current_session;
            for summary in list_session_summaries(paths, config, current_session.as_deref())? {
                println!("{}", format_session_list_entry(&summary));
            }
            Ok(())
        }
        SessionCommand::Current => {
            let state = load_state(paths)?;
            match state.current_session {
                Some(id) => {
                    let temp_tag = if state.is_temp.unwrap_or(false) {
                        " [temp]"
                    } else {
                        ""
                    };
                    println!("{}{}", short_id(&id), temp_tag);
                }
                None => println!("no active session"),
            }
            Ok(())
        }
        SessionCommand::Switch { id } => {
            let resolved = resolve_session_id(paths, config, &id)?;
            let temp = is_temp_session(&resolved);
            set_current_session(paths, config, Some(&resolved), temp)?;
            println!("switched to {}", short_id(&resolved));
            Ok(())
        }
        SessionCommand::New { temp } => {
            let session_id = if temp {
                generate_temp_session_id()
            } else {
                generate_session_id()
            };
            set_current_session(paths, config, Some(&session_id), temp)?;
            println!("{}", short_id(&session_id));
            Ok(())
        }
        SessionCommand::Show { id } => {
            let resolved = resolve_session_or_current(paths, config, id.as_deref(), "show")?;
            let events = read_events(paths, config, &resolved)?;
            let mut messages = 0;
            let mut responses = 0;
            let mut audits = 0;
            for event in &events {
                match event {
                    SessionEvent::Meta(meta) => println!(
                        "session_id={} created_at={}",
                        meta.session_id, meta.created_at
                    ),
                    SessionEvent::Message(message) => {
                        messages += 1;
                        let tool_call_count = message.tool_calls.len();
                        let tool_call_id = message.tool_call_id.clone().unwrap_or_default();
                        let name = message.name.clone().unwrap_or_default();
                        println!(
                            "message role={} tool_calls={} tool_call_id={} name={} content={}",
                            message.role,
                            tool_call_count,
                            serde_json::to_string(&tool_call_id).unwrap_or_default(),
                            serde_json::to_string(&name).unwrap_or_default(),
                            serde_json::to_string(&message.content).unwrap_or_default()
                        );
                    }
                    SessionEvent::Response(response) => {
                        responses += 1;
                        println!(
                            "response provider={} model={} finish_reason={} latency_ms={}",
                            response.provider,
                            response.model,
                            response.finish_reason,
                            response.latency_ms
                        );
                    }
                    SessionEvent::Audit(audit) => {
                        audits += 1;
                        println!(
                            "audit provider={} model={} tool_name={} tool_call_id={} verdict={} summary={}",
                            audit.provider,
                            audit.model,
                            serde_json::to_string(audit.tool_name.as_deref().unwrap_or_default())
                                .unwrap_or_default(),
                            serde_json::to_string(
                                audit.tool_call_id.as_deref().unwrap_or_default()
                            )
                            .unwrap_or_default(),
                            serde_json::to_string(&audit.verdict).unwrap_or_default(),
                            serde_json::to_string(&audit.summary).unwrap_or_default()
                        );
                    }
                }
            }
            println!(
                "summary messages={} responses={} audits={}",
                messages, responses, audits
            );
            Ok(())
        }
        SessionCommand::Render { id, last, all } => {
            if last == Some(0) {
                return Err(AppError::new(
                    crate::error::EXIT_SESSION,
                    "--last must be greater than 0",
                ));
            }
            let resolved = resolve_session_or_current(paths, config, id.as_deref(), "render")?;
            let events = read_events(paths, config, &resolved)?;
            let turns = session_turns_from_events(&events);
            if turns.is_empty() {
                return Err(AppError::new(
                    crate::error::EXIT_SESSION,
                    format!("session `{resolved}` has no renderable messages"),
                ));
            }
            let render_limit = if all { None } else { Some(last.unwrap_or(1)) };
            println!("{}", render_session_turns(config, &turns, render_limit));
            Ok(())
        }
        SessionCommand::Export { id } => {
            let resolved = resolve_session_id(paths, config, &id)?;
            let path = crate::session::session_file(paths, config, &resolved);
            let text = fs::read_to_string(&path).map_err(|err| {
                AppError::new(
                    crate::error::EXIT_SESSION,
                    format!("failed to read session file `{}`: {}", path.display(), err),
                )
            })?;
            print!("{text}");
            Ok(())
        }
        SessionCommand::Delete { id } => {
            let resolved = resolve_session_id(paths, config, &id)?;
            delete_session(paths, config, &resolved)?;
            let state = load_state(paths)?;
            if state.current_session.as_deref() == Some(resolved.as_str()) {
                set_current_session(paths, config, None, false)?;
            }
            println!("deleted session {}", short_id(&resolved));
            Ok(())
        }
        SessionCommand::Clear { all } => {
            let removed = clear_sessions(paths, config, all)?;
            if all {
                set_current_session(paths, config, None, false)?;
            }
            println!("cleared {} sessions", removed);
            Ok(())
        }
        SessionCommand::Gc => {
            let removed = gc_sessions(paths, config)?;
            println!("removed {}", removed);
            Ok(())
        }
    }
}

fn format_session_list_entry(summary: &crate::session::SessionSummary) -> String {
    let marker = if summary.is_current { "* " } else { "  " };
    let temp_tag = if summary.is_temp { " [temp]" } else { "" };
    let prompt = serde_json::to_string(summary.first_prompt.as_deref().unwrap_or(""))
        .unwrap_or_else(|_| "\"\"".to_string());
    let updated = summary.updated_at.unwrap_or(0);
    format!(
        "{marker}{}{temp_tag} {prompt} {DIM}· {}u/{}a · updated={updated}{RESET}",
        short_id(&summary.session_id),
        summary.user_messages,
        summary.assistant_messages,
    )
}

fn resolve_session_or_current(
    paths: &AppPaths,
    config: &AppConfig,
    requested: Option<&str>,
    command: &str,
) -> AppResult<String> {
    if let Some(id) = requested {
        return resolve_session_id(paths, config, id);
    }
    load_state(paths)?.current_session.ok_or_else(|| {
        AppError::new(
            crate::error::EXIT_SESSION,
            format!("no active session; use `chat session {command} <id>` or switch to one first"),
        )
    })
}

fn session_message_has_renderable_payload(message: &SessionMessage) -> bool {
    !message.content.is_empty()
        || !message.images.is_empty()
        || !message.tool_calls.is_empty()
        || message.tool_call_id.is_some()
}

fn session_turns_from_events(events: &[SessionEvent]) -> Vec<Vec<SessionMessage>> {
    let mut turns = Vec::new();
    let mut current = Vec::new();

    for event in events {
        let SessionEvent::Message(message) = event else {
            continue;
        };
        if !session_message_has_renderable_payload(message) {
            continue;
        }

        if message.role == "user" && !current.is_empty() {
            turns.push(current);
            current = Vec::new();
        }
        current.push(message.clone());
    }

    if !current.is_empty() {
        turns.push(current);
    }

    turns
}

fn filter_session_turns_for_repl(turns: &[Vec<SessionMessage>]) -> Vec<Vec<SessionMessage>> {
    turns
        .iter()
        .map(|turn| {
            turn.iter()
                .filter(|message| message.role != "system")
                .cloned()
                .collect::<Vec<_>>()
        })
        .filter(|turn| !turn.is_empty())
        .collect()
}

fn render_session_turns(
    config: &AppConfig,
    turns: &[Vec<SessionMessage>],
    last: Option<usize>,
) -> String {
    render_session_turns_with_width(config, turns, last, None)
}

fn render_session_turns_with_width(
    config: &AppConfig,
    turns: &[Vec<SessionMessage>],
    last: Option<usize>,
    width: Option<usize>,
) -> String {
    let slice = match last {
        Some(last) => {
            let start = turns.len().saturating_sub(last);
            &turns[start..]
        }
        None => turns,
    };
    slice
        .iter()
        .map(|turn| {
            turn.iter()
                .map(|message| render_session_message(config, message, width))
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .collect::<Vec<_>>()
        .join(&format!("\n\n{DIM}{}{RESET}\n\n", "─".repeat(56)))
}

fn render_repl_session_history(
    paths: &AppPaths,
    config: &AppConfig,
    session_id: &str,
    width: usize,
) -> AppResult<String> {
    let events = read_events(paths, config, session_id)?;
    let turns = filter_session_turns_for_repl(&session_turns_from_events(&events));
    Ok(render_session_turns_with_width(
        config,
        &turns,
        None,
        Some(width),
    ))
}

fn render_repl_transcript(
    paths: &AppPaths,
    config: &AppConfig,
    session_id: &str,
    width: usize,
    panels: &[ReplRuntimePanel],
    transient_panel: Option<&ReplRuntimePanel>,
) -> String {
    let history = render_repl_session_history(paths, config, session_id, width).unwrap_or_default();
    let mut rendered_panels = panels
        .iter()
        .map(render_repl_runtime_panel)
        .collect::<Vec<_>>();
    if let Some(panel) = transient_panel {
        rendered_panels.push(render_repl_runtime_panel(panel));
    }
    let panel_text = rendered_panels.join(&format!("\n\n{DIM}{}{RESET}\n\n", "─".repeat(56)));
    match (history.trim().is_empty(), panel_text.trim().is_empty()) {
        (true, true) => String::new(),
        (false, true) => history,
        (true, false) => panel_text,
        (false, false) => format!(
            "{history}\n\n{DIM}{}{RESET}\n\n{panel_text}",
            "─".repeat(56)
        ),
    }
}

fn render_repl_runtime_panel(panel: &ReplRuntimePanel) -> String {
    match panel.kind {
        ReplRuntimePanelKind::User => render_user_message_block(&panel.title, &panel.body),
        ReplRuntimePanelKind::Status => render_runtime_panel(&panel.title, &panel.body, CYAN),
        ReplRuntimePanelKind::Error => render_runtime_panel(&panel.title, &panel.body, RED),
        ReplRuntimePanelKind::Todo => render_compact_runtime_panel(&panel.title, &panel.body, CYAN),
    }
}

fn render_runtime_panel(title: &str, body: &str, accent: &str) -> String {
    let lines = if body.trim().is_empty() {
        vec![String::new()]
    } else {
        body.lines()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
    };
    let rendered = lines
        .iter()
        .map(|line| {
            if line.is_empty() {
                format!("{accent}│{RESET}")
            } else {
                format!("{accent}│{RESET} {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{accent}{title}{RESET}\n{rendered}")
}

fn render_compact_runtime_panel(title: &str, body: &str, accent: &str) -> String {
    let mut lines = vec![format!("{accent}• {title}{RESET}")];
    let body_lines = if body.trim().is_empty() {
        vec!["(empty)".to_string()]
    } else {
        body.lines()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
    };
    for (index, line) in body_lines.iter().enumerate() {
        let prefix = if index == 0 { "└ " } else { "  " };
        lines.push(format!("{accent}{prefix}{RESET}{line}"));
    }
    lines.join("\n")
}

fn render_repl_todo_lines(todos: &[TodoUiItem]) -> Vec<String> {
    if todos.is_empty() {
        return vec!["└ (empty)".to_string()];
    }
    let details = todos
        .iter()
        .find(|todo| matches!(todo.status, TodoUiStatus::InProgress))
        .map(|todo| todo.details.trim())
        .or_else(|| todos.first().map(|todo| todo.details.trim()))
        .unwrap_or("");
    let mut lines = vec![details.to_string()];
    lines.extend(todos.iter().map(|todo| {
        let marker = match todo.status {
            TodoUiStatus::Completed => "✔",
            TodoUiStatus::Pending | TodoUiStatus::InProgress => "□",
        };
        format!("{marker} {}", todo.title.trim())
    }));
    lines
}

fn push_repl_panel(state: &mut ReplState, panel: ReplRuntimePanel) {
    state.panels.push(panel);
    if state.panels.len() > 12 {
        let drop_count = state.panels.len() - 12;
        state.panels.drain(..drop_count);
    }
}

fn set_repl_transient_panel(state: &mut ReplState, panel: ReplRuntimePanel) {
    state.transient_panel = Some(panel);
}

fn clear_repl_transient_panel(state: &mut ReplState) {
    state.transient_panel = None;
}

fn build_repl_status_panel(
    cli: &Cli,
    config: &AppConfig,
    session_id: &str,
    state: &ReplState,
) -> ReplRuntimePanel {
    let provider_id =
        repl_effective_provider_id(cli, config, state).unwrap_or_else(|| "-".to_string());
    let model_id = repl_effective_model_id(cli, config, state).unwrap_or_else(|| "-".to_string());
    let mut sections = vec![
        format!("session: {}", short_id(session_id)),
        format!("provider: {provider_id}"),
        format!("model: {model_id}"),
        format!("stream: {}", if state.stream { "on" } else { "off" }),
        format!("tools: {}", if state.tools { "on" } else { "off" }),
        format!("context_status: {}", state.context_status.as_str()),
    ];
    sections.push(String::new());
    sections.push("quick actions:".to_string());
    sections.push("/model       choose model".to_string());
    sections.push("/sessions    switch session".to_string());
    sections.push("/audit       toggle audit checks".to_string());
    sections.push("/tool-search toggle progressive loading".to_string());
    sections.push("/clear       clear current session history".to_string());
    sections.push("/new         create a new session".to_string());
    ReplRuntimePanel {
        kind: ReplRuntimePanelKind::Status,
        title: "Status".to_string(),
        body: sections.join("\n"),
    }
}

fn build_repl_error_panel(message: &str) -> ReplRuntimePanel {
    ReplRuntimePanel {
        kind: ReplRuntimePanelKind::Error,
        title: "Error".to_string(),
        body: message.to_string(),
    }
}

fn build_repl_user_panel(
    config: &AppConfig,
    prompt: &str,
    images: &[MessageImage],
) -> ReplRuntimePanel {
    let inline = join_inline_prompt_segments(&[
        (!images.is_empty()).then(|| image_badges(images.len())),
        (!prompt.trim().is_empty()).then(|| prompt.to_string()),
    ]);
    let body = if inline.is_empty() {
        String::new()
    } else {
        render_markdown(&inline, config.defaults.collapse_thinking.unwrap_or(false))
    };
    ReplRuntimePanel {
        kind: ReplRuntimePanelKind::User,
        title: format!("{GREEN}User{RESET}"),
        body,
    }
}

fn render_repl_submission_preview(
    stdout: &mut io::Stdout,
    paths: &AppPaths,
    config: &AppConfig,
    session_id: &str,
    width: usize,
    prompt: &str,
    images: &[MessageImage],
) -> AppResult<()> {
    let history = render_repl_session_history(paths, config, session_id, width).unwrap_or_default();
    let inline = join_inline_prompt_segments(&[
        (!images.is_empty()).then(|| image_badges(images.len())),
        (!prompt.trim().is_empty()).then(|| prompt.to_string()),
    ]);
    let body = if inline.is_empty() {
        String::new()
    } else {
        render_markdown_with_width(
            &inline,
            config.defaults.collapse_thinking.unwrap_or(false),
            width,
        )
    };
    let preview = render_user_message_block(&format!("{GREEN}User{RESET}"), &body);
    if !history.trim().is_empty() {
        writeln!(stdout, "{history}").map_err(|err| {
            AppError::new(EXIT_ARGS, format!("failed to write REPL history: {err}"))
        })?;
        writeln!(stdout).map_err(|err| {
            AppError::new(EXIT_ARGS, format!("failed to write REPL spacing: {err}"))
        })?;
    }
    writeln!(stdout, "{preview}")
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to write REPL preview: {err}")))?;
    stdout
        .flush()
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to flush REPL preview: {err}")))?;
    Ok(())
}

fn render_session_message(
    config: &AppConfig,
    message: &SessionMessage,
    width: Option<usize>,
) -> String {
    let label = session_message_label(message);
    let body = render_session_message_body(config, message, width);

    match message.role.as_str() {
        "user" => render_user_message_block(&label, &body),
        _ if body.is_empty() => label,
        _ => format!("{label}\n{body}"),
    }
}

fn session_message_label(message: &SessionMessage) -> String {
    match message.role.as_str() {
        "user" => format!("{GREEN}User{RESET}"),
        "assistant" => format!("{CYAN}Assistant{RESET}"),
        "tool" => {
            let name = message.name.as_deref().unwrap_or("Tool");
            format!("{YELLOW}Tool:{RESET} {name}")
        }
        "system" => format!("{MAGENTA}System{RESET}"),
        other => format!("{DIM}{other}{RESET}"),
    }
}

fn render_session_message_body(
    config: &AppConfig,
    message: &SessionMessage,
    width: Option<usize>,
) -> String {
    let mut sections = Vec::new();
    let collapse = config.defaults.collapse_thinking.unwrap_or(false);

    if !message.content.is_empty() {
        sections.push(match width {
            Some(width) => render_markdown_with_width(&message.content, collapse, width),
            None => render_markdown(&message.content, collapse),
        });
    }

    if !message.tool_calls.is_empty() {
        let tool_lines = message
            .tool_calls
            .iter()
            .map(|raw_call| format!("{DIM}•{RESET} {}", summarize_tool_call_activity(raw_call)))
            .collect::<Vec<_>>()
            .join("\n");
        sections.push(format!("{DIM}tool calls{RESET}\n{tool_lines}"));
    }

    if !message.images.is_empty() {
        sections.push(format!(
            "{DIM}[{} image(s) attached]{RESET}",
            message.images.len()
        ));
    }

    if sections.is_empty() && message.tool_call_id.is_some() {
        sections.push(format!("{DIM}[empty tool result]{RESET}"));
    }

    sections.join("\n")
}

fn render_user_message_block(label: &str, body: &str) -> String {
    let lines = if body.is_empty() {
        vec![String::new()]
    } else {
        body.lines()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
    };
    let rendered_body = lines
        .iter()
        .map(|line| {
            if line.is_empty() {
                format!("{GREEN}│{RESET}")
            } else {
                format!("{GREEN}│{RESET} {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{label}\n{rendered_body}")
}

async fn handle_doctor(
    paths: &AppPaths,
    config: &AppConfig,
    secrets: &SecretsConfig,
) -> AppResult<()> {
    let mut doctor_code: Option<i32> = None;
    println!("config_file={}", paths.config_file.display());
    println!("secrets_file={}", paths.secrets_file.display());
    println!("sessions_dir={}", paths.sessions_dir(config).display());
    let issues = validate_config(config);
    if issues.is_empty() {
        println!("config=ok");
    } else {
        println!("config=invalid");
        doctor_code.get_or_insert(EXIT_CONFIG);
        for issue in issues {
            println!(
                "issue={}",
                serde_json::to_string(&issue).unwrap_or_default()
            );
        }
    }
    println!(
        "default_provider={} default_model={}",
        config.defaults.provider.clone().unwrap_or_default(),
        config.defaults.model.clone().unwrap_or_default()
    );

    for (provider_id, provider) in &config.providers {
        print_auth_status(config, secrets, provider_id)?;
        match resolve_api_key(provider_id, provider, secrets) {
            Ok(api_key) => {
                match test_provider(provider_id, provider, &api_key, &config.models).await {
                    Ok(()) => println!("provider_test={} ok=1", provider_id),
                    Err(err) => {
                        doctor_code.get_or_insert(err.code);
                        println!(
                            "provider_test={} ok=0 error={}",
                            provider_id,
                            serde_json::to_string(&err.message).unwrap_or_default()
                        );
                    }
                }
            }
            Err(err) => {
                doctor_code.get_or_insert(err.code);
                println!(
                    "provider_test={} ok=0 error={}",
                    provider_id,
                    serde_json::to_string(&err.message).unwrap_or_default()
                );
            }
        }
    }

    if let Some(code) = doctor_code {
        return Err(AppError::new(code, "doctor found issues"));
    }
    Ok(())
}

async fn handle_ask(
    cli: &Cli,
    paths: &AppPaths,
    config: &AppConfig,
    secrets: &SecretsConfig,
    args: AskArgs,
) -> AppResult<()> {
    let use_tools = args.tools || config.defaults.tools.unwrap_or(true);
    if use_tools {
        let output_fmt = if args.stream {
            Some(OutputFormat::Text)
        } else {
            cli.output.clone()
        };
        let result =
            execute_ask_with_tools(cli, paths, config, secrets, &args, output_fmt, true).await?;
        if !args.stream {
            let rendered = format_final_ask_output(config, &result, args.raw_provider_response)?;
            println!("{rendered}");
        }
        return Ok(());
    }
    if args.stream {
        execute_ask_stream(cli, paths, config, secrets, &args, cli.output.clone(), true).await?;
        return Ok(());
    }
    let result = execute_ask(cli, paths, config, secrets, &args, cli.output.clone()).await?;
    let rendered = format_final_ask_output(config, &result, args.raw_provider_response)?;
    println!("{rendered}");
    Ok(())
}

fn format_final_ask_output(
    config: &AppConfig,
    result: &AskExecution,
    raw_provider_response: bool,
) -> AppResult<String> {
    if result.format == OutputFormat::Text && !raw_provider_response {
        return Ok(render_markdown(
            &result.output.message.content,
            config.defaults.collapse_thinking.unwrap_or(false),
        ));
    }
    render_ask_output(result.format.clone(), &result.output, raw_provider_response)
}

fn should_stream_tool_round(_request: &ChatRequest, requested_stream: bool, _round: usize) -> bool {
    requested_stream
}

fn update_stream_status(status: &Option<StreamStatus>, phase: StreamPhase) -> AppResult<()> {
    if let Some(status) = status {
        status.set_phase(phase).map_err(|err| {
            AppError::new(EXIT_ARGS, format!("failed to update status line: {err}"))
        })?;
    }
    Ok(())
}

fn write_stream_output(
    stdout: &mut io::Stdout,
    status: &Option<StreamStatus>,
    text: &str,
) -> AppResult<()> {
    if text.is_empty() {
        return Ok(());
    }

    if let Some(status) = status {
        status
            .write_output(text)
            .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to write stdout: {err}")))?;
    } else {
        write!(stdout, "{text}")
            .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to write stdout: {err}")))?;
        stdout
            .flush()
            .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to flush stdout: {err}")))?;
    }

    Ok(())
}

fn stop_stream_status(status: &mut Option<StreamStatus>) -> AppResult<()> {
    if let Some(mut status_bar) = status.take() {
        status_bar.stop().map_err(|err| {
            AppError::new(EXIT_ARGS, format!("failed to clear status line: {err}"))
        })?;
    }
    Ok(())
}

#[cfg(test)]
fn execute_tool_as_message(
    raw_call: &Value,
    auto_confirm: bool,
    config: &AppConfig,
) -> ChatMessage {
    let fallback_id = raw_call["id"]
        .as_str()
        .unwrap_or("invalid_tool_call")
        .to_string();
    let fallback_name = raw_call["function"]["name"]
        .as_str()
        .unwrap_or("unknown_tool")
        .to_string();

    match parse_tool_call(raw_call) {
        Ok(call) => match execute_tool(&call, auto_confirm, config) {
            Ok(result) => ChatMessage {
                role: "tool".to_string(),
                content: result.content,
                images: result.images,
                tool_calls: None,
                tool_call_id: Some(result.tool_call_id),
                name: Some(call.name),
            },
            Err(err) => ChatMessage {
                role: "tool".to_string(),
                content: format!("tool execution error: {}", err.message),
                images: Vec::new(),
                tool_calls: None,
                tool_call_id: Some(call.id),
                name: Some(call.name),
            },
        },
        Err(err) => ChatMessage {
            role: "tool".to_string(),
            content: format!("tool invocation error: {}", err.message),
            images: Vec::new(),
            tool_calls: None,
            tool_call_id: Some(fallback_id),
            name: Some(fallback_name),
        },
    }
}

fn execute_tool_as_message_with_context(
    raw_call: &Value,
    auto_confirm: bool,
    paths: &AppPaths,
    config: &AppConfig,
    transcript: &[ChatMessage],
) -> ChatMessage {
    let fallback_id = raw_call["id"]
        .as_str()
        .unwrap_or("invalid_tool_call")
        .to_string();
    let fallback_name = raw_call["function"]["name"]
        .as_str()
        .unwrap_or("unknown_tool")
        .to_string();

    match parse_tool_call(raw_call) {
        Ok(call) => {
            if let Err(err) = call_requires_prior_read(transcript, &call) {
                return ChatMessage {
                    role: "tool".to_string(),
                    content: format!("tool execution error: {}", err.message),
                    images: Vec::new(),
                    tool_calls: None,
                    tool_call_id: Some(call.id),
                    name: Some(call.name),
                };
            }
            match execute_tool_with_context_and_paths(
                &call,
                auto_confirm,
                config,
                Some(paths),
                transcript,
            ) {
                Ok(result) => ChatMessage {
                    role: "tool".to_string(),
                    content: result.content,
                    images: result.images,
                    tool_calls: None,
                    tool_call_id: Some(result.tool_call_id),
                    name: Some(call.name),
                },
                Err(err) => ChatMessage {
                    role: "tool".to_string(),
                    content: format!("tool execution error: {}", err.message),
                    images: Vec::new(),
                    tool_calls: None,
                    tool_call_id: Some(call.id),
                    name: Some(call.name),
                },
            }
        }
        Err(err) => ChatMessage {
            role: "tool".to_string(),
            content: format!("tool invocation error: {}", err.message),
            images: Vec::new(),
            tool_calls: None,
            tool_call_id: Some(fallback_id),
            name: Some(fallback_name),
        },
    }
}

fn chat_message_has_payload(message: &ChatMessage) -> bool {
    !message.content.is_empty()
        || !message.images.is_empty()
        || message
            .tool_calls
            .as_ref()
            .is_some_and(|calls| !calls.is_empty())
        || message.tool_call_id.is_some()
}

fn session_message_from_chat_message(message: &ChatMessage) -> SessionMessage {
    SessionMessage {
        role: message.role.clone(),
        content: message.content.clone(),
        images: message.images.clone(),
        tool_calls: message.tool_calls.clone().unwrap_or_default(),
        tool_call_id: message.tool_call_id.clone(),
        name: message.name.clone(),
        created_at: now_rfc3339(),
    }
}

fn append_session_message_events(events: &mut Vec<SessionEvent>, messages: &[ChatMessage]) {
    events.extend(
        messages
            .iter()
            .filter(|message| chat_message_has_payload(message))
            .map(|message| SessionEvent::Message(session_message_from_chat_message(message))),
    );
}

fn audit_preview_text(value: &str, max_chars: usize) -> String {
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

fn audit_preview_value(value: &Value, max_chars: usize) -> String {
    audit_preview_text(&serde_json::to_string(value).unwrap_or_default(), max_chars)
}

fn tool_side_effects_label(name: &str) -> &'static str {
    match lookup_tool_spec(name).map(|spec| spec.side_effects) {
        Some(crate::tool::ToolSideEffects::ReadOnly) => "read_only",
        Some(crate::tool::ToolSideEffects::Mutating) => "mutating",
        Some(crate::tool::ToolSideEffects::External) => "external",
        None => "unknown",
    }
}

fn tool_call_side_effects_label(call: &crate::tool::ToolCall) -> &'static str {
    match tool_call_side_effects(call) {
        crate::tool::ToolSideEffects::ReadOnly => "read_only",
        crate::tool::ToolSideEffects::Mutating => "mutating",
        crate::tool::ToolSideEffects::External => "external",
    }
}

fn tool_parallelism_label(name: &str) -> &'static str {
    match lookup_tool_spec(name).map(|spec| spec.parallelism) {
        Some(crate::tool::ToolParallelism::ParallelSafe) => "parallel_safe",
        Some(crate::tool::ToolParallelism::SequentialOnly) => "sequential_only",
        None => "unknown",
    }
}

fn summarize_tool_call(raw_call: &Value) -> Value {
    match parse_tool_call(raw_call) {
        Ok(call) => {
            let spec = lookup_tool_spec(&call.name);
            json!({
                "id": call.id,
                "name": call.name,
                "arguments_preview": audit_preview_value(&call.arguments, 600),
                "side_effects": tool_call_side_effects_label(&call),
                "parallelism": spec.map(|tool| tool_parallelism_label(tool.name)).unwrap_or("unknown"),
                "requires_confirmation": tool_call_requires_confirmation(&call),
            })
        }
        Err(err) => json!({
            "id": raw_call["id"].as_str().unwrap_or_default(),
            "name": raw_call["function"]["name"].as_str().unwrap_or("unknown_tool"),
            "parse_error": err.message,
            "raw_preview": audit_preview_value(raw_call, 600),
        }),
    }
}

fn truncate_tool_call_id(id: &str) -> String {
    let tail = id.rsplit('_').next().unwrap_or(id);
    if tail.chars().count() <= 8 {
        tail.to_string()
    } else {
        tail.chars().take(8).collect()
    }
}

fn summarize_tool_call_activity(raw_call: &Value) -> String {
    match parse_tool_call(raw_call) {
        Ok(call) => match call.name.as_str() {
            "ToolSearch" | "tool_search" => format!(
                "ToolSearch: {}",
                audit_preview_text(call.arguments["query"].as_str().unwrap_or(""), 32)
            ),
            "bash" => format!(
                "bash: {}",
                audit_preview_text(call.arguments["command"].as_str().unwrap_or(""), 40)
            ),
            "Bash" => format!(
                "Bash: {}",
                audit_preview_text(call.arguments["command"].as_str().unwrap_or(""), 40)
            ),
            "Write" | "write" | "Edit" | "edit" => format!(
                "Edit: {}",
                audit_preview_text(
                    call.arguments["file_path"]
                        .as_str()
                        .or_else(|| call.arguments["path"].as_str())
                        .unwrap_or(""),
                    32
                )
            ),
            "read" => format!(
                "read: {}",
                audit_preview_text(call.arguments["path"].as_str().unwrap_or(""), 32)
            ),
            "Read" => format!(
                "Read: {}",
                audit_preview_text(
                    call.arguments["file_path"]
                        .as_str()
                        .or_else(|| call.arguments["path"].as_str())
                        .unwrap_or(""),
                    32
                )
            ),
            "fetch" => format!(
                "fetch: {}",
                audit_preview_text(call.arguments["url"].as_str().unwrap_or(""), 40)
            ),
            "WebFetch" | "web_fetch" => format!(
                "WebFetch: {}",
                audit_preview_text(call.arguments["url"].as_str().unwrap_or(""), 40)
            ),
            "grep" => format!(
                "grep: {}",
                audit_preview_text(call.arguments["pattern"].as_str().unwrap_or(""), 24)
            ),
            "Grep" => format!(
                "Grep: {}",
                audit_preview_text(call.arguments["pattern"].as_str().unwrap_or(""), 24)
            ),
            "Glob" | "glob" => format!(
                "Glob: {}",
                audit_preview_text(call.arguments["pattern"].as_str().unwrap_or(""), 24)
            ),
            "skill_read" => format!(
                "skill_read: {}",
                audit_preview_text(call.arguments["name"].as_str().unwrap_or(""), 24)
            ),
            "SkillRead" => format!(
                "SkillRead: {}",
                audit_preview_text(call.arguments["name"].as_str().unwrap_or(""), 24)
            ),
            "skills_list" => "skills_list".to_string(),
            "SkillsList" => "SkillsList".to_string(),
            "todo" | "todo_write" | "TodoWrite" => {
                let count = call.arguments["todos"]
                    .as_array()
                    .map(|items| items.len())
                    .unwrap_or(0);
                format!("TodoWrite: {count} item(s)")
            }
            other => other.to_string(),
        },
        Err(_) => "unknown_tool".to_string(),
    }
}

fn render_tool_call_summary(round: usize, raw_calls: &[Value], width: usize) -> String {
    let header = format!("{DIM}[tools {round}]{RESET}");
    if raw_calls.is_empty() {
        return header;
    }

    let bullet_prefix = format!("{DIM}  •{RESET} ");
    let bullet_prefix_width = visible_width("  • ");
    let continuation_prefix = "    ";
    let line_width = width.max(bullet_prefix_width + 8);
    let content_width = line_width.saturating_sub(bullet_prefix_width).max(8);
    let mut lines = vec![header];

    for summary in raw_calls.iter().map(summarize_tool_call_activity) {
        let wrapped = wrap_ansi_to_width(&summary, content_width);
        for (index, line) in wrapped.into_iter().enumerate() {
            if index == 0 {
                lines.push(format!("{bullet_prefix}{line}"));
            } else {
                lines.push(format!("{continuation_prefix}{line}"));
            }
        }
    }

    lines.join("\n")
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TodoUiStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, Deserialize)]
struct TodoUiItem {
    title: String,
    details: String,
    status: TodoUiStatus,
}

fn is_todo_tool_name(name: &str) -> bool {
    matches!(name, "todo" | "todo_write" | "TodoWrite")
}

fn todo_items_from_raw_call(raw_call: &Value) -> Option<Vec<TodoUiItem>> {
    let call = parse_tool_call(raw_call).ok()?;
    if !is_todo_tool_name(&call.name) {
        return None;
    }
    serde_json::from_value(call.arguments.get("todos")?.clone()).ok()
}

fn latest_todo_items_from_events(events: &[SessionEvent]) -> Option<Vec<TodoUiItem>> {
    for event in events.iter().rev() {
        let SessionEvent::Message(message) = event else {
            continue;
        };
        for raw_call in message.tool_calls.iter().rev() {
            if let Some(todos) = todo_items_from_raw_call(raw_call) {
                return Some(todos);
            }
        }
    }
    None
}

fn build_repl_todo_panel(todos: &[TodoUiItem]) -> ReplRuntimePanel {
    ReplRuntimePanel {
        kind: ReplRuntimePanelKind::Todo,
        title: "Updated Plan".to_string(),
        body: render_repl_todo_lines(todos).join("\n"),
    }
}

fn sync_repl_todo_panel(
    paths: &AppPaths,
    config: &AppConfig,
    session_id: &str,
    state: &mut ReplState,
) {
    let todos = read_events(paths, config, session_id)
        .ok()
        .and_then(|events| latest_todo_items_from_events(&events))
        .filter(|items| !items.is_empty());

    state
        .panels
        .retain(|panel| panel.kind != ReplRuntimePanelKind::Todo);
    if state
        .transient_panel
        .as_ref()
        .is_some_and(|panel| panel.kind == ReplRuntimePanelKind::Todo)
    {
        state.transient_panel = None;
    }
    if let Some(todos) = todos {
        push_repl_panel(state, build_repl_todo_panel(&todos));
    }
}

fn should_print_tool_result_ui(
    format: OutputFormat,
    show_static_status_bar: bool,
    message: &ChatMessage,
) -> bool {
    show_static_status_bar
        && format == OutputFormat::Text
        && message.role == "tool"
        && message.name.as_deref().is_some_and(is_todo_tool_name)
        && !message.content.trim().is_empty()
}

fn print_tool_result_ui(
    stdout: &mut io::Stdout,
    config: &AppConfig,
    message: &ChatMessage,
) -> AppResult<()> {
    let rendered = render_markdown(
        &message.content,
        config.defaults.collapse_thinking.unwrap_or(false),
    );
    if rendered.trim().is_empty() {
        return Ok(());
    }
    writeln!(stdout, "{rendered}")
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to write Todo UI: {err}")))?;
    stdout
        .flush()
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to flush Todo UI: {err}")))?;
    Ok(())
}

fn normalize_tool_path(path: &str) -> String {
    let candidate = std::path::Path::new(path);
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

fn call_file_path(call: &crate::tool::ToolCall) -> Option<String> {
    match call.name.as_str() {
        "Read" | "Edit" | "Write" | "read" | "edit" | "write" => call.arguments["file_path"]
            .as_str()
            .or_else(|| call.arguments["path"].as_str())
            .map(normalize_tool_path),
        _ => None,
    }
}

fn transcript_has_read_for_path(transcript: &[ChatMessage], target_path: &str) -> bool {
    transcript
        .iter()
        .filter_map(|message| message.tool_calls.as_ref())
        .flatten()
        .filter_map(|raw_call| parse_tool_call(raw_call).ok())
        .any(|call| {
            matches!(call.name.as_str(), "Read" | "read")
                && call_file_path(&call).as_deref() == Some(target_path)
        })
}

fn call_requires_prior_read(
    transcript: &[ChatMessage],
    call: &crate::tool::ToolCall,
) -> AppResult<()> {
    let Some(path) = call_file_path(call) else {
        return Ok(());
    };
    if !matches!(call.name.as_str(), "Edit" | "Write" | "edit" | "write") {
        return Ok(());
    }
    if !std::path::Path::new(&path).exists() {
        return Ok(());
    }
    if call.arguments["old_string"]
        .as_str()
        .is_some_and(str::is_empty)
        && std::fs::read_to_string(&path)
            .map(|content| content.trim().is_empty())
            .unwrap_or(false)
    {
        return Ok(());
    }
    if transcript_has_read_for_path(transcript, &path) {
        return Ok(());
    }
    Err(AppError::new(
        EXIT_ARGS,
        format!(
            "{}: file must be read first with Read before modifying `{}`",
            call.name, path
        ),
    ))
}

fn discovered_tool_names_from_search(config: &AppConfig, raw_call: &Value) -> Vec<String> {
    let Ok(call) = parse_tool_call(raw_call) else {
        return Vec::new();
    };
    if call.name != "ToolSearch" && call.name != "tool_search" {
        return Vec::new();
    }
    let query = call.arguments["query"].as_str().unwrap_or_default();
    let max_results = call.arguments["max_results"].as_u64().unwrap_or(5) as usize;
    tool_search_matches(config, query, max_results)
        .into_iter()
        .filter_map(|item| item["name"].as_str().map(str::to_string))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum AuditPromptKind {
    Default,
    Bash,
    Edit,
}

fn audit_prompt_kind(call: &crate::tool::ToolCall) -> AuditPromptKind {
    match call.name.as_str() {
        "Bash" | "bash" => AuditPromptKind::Bash,
        "Edit" | "edit" | "Write" | "write" => AuditPromptKind::Edit,
        _ => AuditPromptKind::Default,
    }
}

fn audit_prompt_path<'a>(config: &'a AppConfig, kind: AuditPromptKind) -> Option<&'a str> {
    match kind {
        AuditPromptKind::Default => config.audit.default_prompt_file.as_deref(),
        AuditPromptKind::Bash => config.audit.bash_prompt_file.as_deref(),
        AuditPromptKind::Edit => config.audit.edit_prompt_file.as_deref(),
    }
}

fn audit_prompt_label(kind: AuditPromptKind) -> &'static str {
    match kind {
        AuditPromptKind::Default => "audit.default_prompt_file",
        AuditPromptKind::Bash => "audit.bash_prompt_file",
        AuditPromptKind::Edit => "audit.edit_prompt_file",
    }
}

fn load_audit_prompt(config: &AppConfig, kind: AuditPromptKind) -> AppResult<String> {
    let label = audit_prompt_label(kind);
    let configured_path = audit_prompt_path(config, kind).ok_or_else(|| {
        AppError::new(
            EXIT_CONFIG,
            format!("missing configured audit prompt path `{label}`"),
        )
    })?;
    let expanded = crate::config::expand_tilde(configured_path);
    fs::read_to_string(&expanded).map_err(|err| {
        AppError::new(
            EXIT_CONFIG,
            format!(
                "failed to read audit prompt file `{}`: {err}",
                expanded.display()
            ),
        )
    })
}

fn build_tool_review_payload(
    session_id: &str,
    transcript: &[ChatMessage],
    calls: &[crate::tool::ToolCall],
) -> Value {
    let transcript = transcript
        .iter()
        .rev()
        .take(8)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|message| {
            let mut item = serde_json::Map::new();
            item.insert("role".to_string(), json!(message.role));
            if let Some(name) = &message.name {
                item.insert("name".to_string(), json!(name));
                item.insert(
                    "side_effects".to_string(),
                    json!(tool_side_effects_label(name)),
                );
                item.insert(
                    "parallelism".to_string(),
                    json!(tool_parallelism_label(name)),
                );
            }
            if let Some(tool_call_id) = &message.tool_call_id {
                item.insert("tool_call_id".to_string(), json!(tool_call_id));
            }
            if let Some(tool_calls) = &message.tool_calls {
                item.insert(
                    "tool_calls".to_string(),
                    Value::Array(tool_calls.iter().map(summarize_tool_call).collect()),
                );
            }
            if !message.content.is_empty() {
                let limit = if message.role == "tool" { 1200 } else { 2000 };
                item.insert(
                    "content_preview".to_string(),
                    json!(audit_preview_text(&message.content, limit)),
                );
            }
            Value::Object(item)
        })
        .collect::<Vec<_>>();

    json!({
        "session_id": session_id,
        "tool_calls": calls.iter().map(|call| {
            json!({
                "id": call.id,
                "name": call.name,
                "arguments": call.arguments,
                "side_effects": tool_call_side_effects_label(call),
                "parallelism": tool_parallelism_label(&call.name),
                "requires_confirmation": tool_call_requires_confirmation(call),
            })
        }).collect::<Vec<_>>(),
        "transcript": transcript,
    })
}

fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end >= start).then_some(&text[start..=end])
}

fn normalize_audit_verdict(verdict: Option<&str>) -> String {
    match verdict
        .unwrap_or("warning")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "pass" => "pass".to_string(),
        "warning" => "warning".to_string(),
        "block" => "block".to_string(),
        other if !other.is_empty() => other.to_string(),
        _ => "warning".to_string(),
    }
}

fn parse_batch_audit_report(
    response: ChatResponse,
    calls: &[crate::tool::ToolCall],
) -> BatchAuditDecision {
    let parsed = serde_json::from_str::<AuditAgentResponse>(&response.content)
        .ok()
        .or_else(|| {
            extract_json_object(&response.content)
                .and_then(|content| serde_json::from_str::<AuditAgentResponse>(content).ok())
        });

    let provider = response.provider_id;
    let model = response.model_id;
    let latency_ms = response.latency_ms;
    let usage = response.usage;

    let reports = match parsed {
        Some(parsed) => calls
            .iter()
            .map(|call| {
                let item = parsed.results.iter().find(|item| item.id == call.id);
                let report = AuditReport {
                    provider: provider.clone(),
                    model: model.clone(),
                    verdict: item
                        .map(|item| normalize_audit_verdict(item.verdict.as_deref()))
                        .unwrap_or_else(|| "warning".to_string()),
                    message: item
                        .and_then(|item| item.message.as_ref())
                        .filter(|message| !message.trim().is_empty())
                        .map(|message| audit_preview_text(message, 72))
                        .unwrap_or_else(|| "需人工确认".to_string()),
                    latency_ms,
                    usage: usage.clone(),
                };
                (call.id.clone(), report)
            })
            .collect(),
        None => calls
            .iter()
            .map(|call| {
                (
                    call.id.clone(),
                    AuditReport {
                        provider: provider.clone(),
                        model: model.clone(),
                        verdict: "warning".to_string(),
                        message: "审核返回异常".to_string(),
                        latency_ms,
                        usage: usage.clone(),
                    },
                )
            })
            .collect(),
    };

    BatchAuditDecision { reports }
}

fn unavailable_batch_audit_decision(
    provider: String,
    model: String,
    calls: &[crate::tool::ToolCall],
    message: String,
) -> BatchAuditDecision {
    let reports = calls
        .iter()
        .map(|call| {
            (
                call.id.clone(),
                AuditReport {
                    provider: provider.clone(),
                    model: model.clone(),
                    verdict: "unavailable".to_string(),
                    message: audit_preview_text(&message, 72),
                    latency_ms: 0,
                    usage: crate::session::Usage::default(),
                },
            )
        })
        .collect();
    BatchAuditDecision { reports }
}

fn resolve_audit_target(
    config: &AppConfig,
    secrets: &SecretsConfig,
    fallback_request: &ChatRequest,
) -> AppResult<ResolvedChatTarget> {
    let audit_model_id = config
        .audit
        .model
        .as_deref()
        .unwrap_or(&fallback_request.model_id);
    if audit_model_id == fallback_request.model_id {
        return Ok(ResolvedChatTarget {
            provider_id: fallback_request.provider_id.clone(),
            provider: fallback_request.provider.clone(),
            model_id: fallback_request.model_id.clone(),
            model: fallback_request.model.clone(),
            api_key: fallback_request.api_key.clone(),
        });
    }

    let model = config.models.get(audit_model_id).ok_or_else(|| {
        AppError::new(
            EXIT_MODEL,
            format!("audit.model `{audit_model_id}` does not exist"),
        )
    })?;
    let provider = config.providers.get(&model.provider).ok_or_else(|| {
        AppError::new(
            EXIT_PROVIDER,
            format!(
                "audit.model `{audit_model_id}` references missing provider `{}`",
                model.provider
            ),
        )
    })?;
    let api_key = resolve_api_key(&model.provider, provider, secrets)?;
    Ok(ResolvedChatTarget {
        provider_id: model.provider.clone(),
        provider: provider.clone(),
        model_id: audit_model_id.to_string(),
        model: model.clone(),
        api_key,
    })
}

fn tool_requires_agent_review(config: &AppConfig, call: &crate::tool::ToolCall) -> bool {
    config.audit.enabled.unwrap_or(false)
        && matches!(
            tool_call_side_effects(call),
            crate::tool::ToolSideEffects::Mutating
        )
}

async fn review_tool_call_batch(
    target: &ResolvedChatTarget,
    fallback_request: &ChatRequest,
    session_id: &str,
    transcript: &[ChatMessage],
    calls: &[crate::tool::ToolCall],
    prompt: String,
) -> BatchAuditDecision {
    let payload = build_tool_review_payload(session_id, transcript, calls);
    let request = ChatRequest {
        provider_id: target.provider_id.clone(),
        provider: target.provider.clone(),
        model_id: target.model_id.clone(),
        model: target.model.clone(),
        api_key: target.api_key.clone(),
        messages: vec![
            ChatMessage {
                role: "system".to_string(),
                content: prompt,
                images: Vec::new(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: serde_json::to_string_pretty(&payload)
                    .unwrap_or_else(|_| payload.to_string()),
                images: Vec::new(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
        ],
        temperature: Some(0.0),
        max_output_tokens: Some(target.model.max_output_tokens.unwrap_or(800).min(800)),
        params: BTreeMap::new(),
        timeout_secs: fallback_request.timeout_secs,
        tools: Vec::new(),
    };

    match send_chat(request).await {
        Ok(response) => parse_batch_audit_report(response, calls),
        Err(err) => unavailable_batch_audit_decision(
            target.provider_id.clone(),
            target.model_id.clone(),
            calls,
            format!("audit agent failed: {}", err.message),
        ),
    }
}

async fn maybe_review_tool_call(
    config: &AppConfig,
    secrets: &SecretsConfig,
    fallback_request: &ChatRequest,
    session_id: &str,
    transcript: &[ChatMessage],
    calls: &[crate::tool::ToolCall],
) -> Option<BatchAuditDecision> {
    if calls.is_empty() {
        return None;
    }

    let target = match resolve_audit_target(config, secrets, fallback_request) {
        Ok(target) => target,
        Err(err) => {
            return Some(unavailable_batch_audit_decision(
                fallback_request.provider_id.clone(),
                config
                    .audit
                    .model
                    .clone()
                    .unwrap_or_else(|| fallback_request.model_id.clone()),
                calls,
                format!("audit agent could not be prepared: {}", err.message),
            ));
        }
    };

    let mut grouped_calls = BTreeMap::<AuditPromptKind, Vec<crate::tool::ToolCall>>::new();
    for call in calls {
        grouped_calls
            .entry(audit_prompt_kind(call))
            .or_default()
            .push(call.clone());
    }

    let mut reports = BTreeMap::new();
    for (kind, group) in grouped_calls {
        let prompt = match load_audit_prompt(config, kind) {
            Ok(prompt) => prompt,
            Err(err) => {
                reports.extend(
                    unavailable_batch_audit_decision(
                        target.provider_id.clone(),
                        target.model_id.clone(),
                        &group,
                        format!("audit prompt could not be loaded: {}", err.message),
                    )
                    .reports,
                );
                continue;
            }
        };

        reports.extend(
            review_tool_call_batch(
                &target,
                fallback_request,
                session_id,
                transcript,
                &group,
                prompt,
            )
            .await
            .reports,
        );
    }

    Some(BatchAuditDecision { reports })
}

fn print_audit_start(call_count: usize, model_id: &str) {
    eprintln!(
        "  {DIM}{CYAN}audit{RESET} {CYAN}{call_count} tool(s){RESET} {DIM}· {model_id}{RESET}"
    );
}

fn append_audit_event(
    paths: &AppPaths,
    config: &AppConfig,
    args: &AskArgs,
    session_id: &str,
    tool_name: Option<&str>,
    tool_call_id: Option<&str>,
    audit: &AuditReport,
) -> AppResult<()> {
    let auto_save = config.defaults.auto_save_session.unwrap_or(true);
    if args.ephemeral || !auto_save {
        return Ok(());
    }

    append_events(
        paths,
        config,
        session_id,
        &[SessionEvent::Audit(SessionAudit {
            provider: audit.provider.clone(),
            model: audit.model.clone(),
            tool_name: tool_name.map(|value| value.to_string()),
            tool_call_id: tool_call_id.map(|value| value.to_string()),
            verdict: audit.verdict.clone(),
            summary: audit.message.clone(),
            findings: Vec::new(),
            recommendations: Vec::new(),
            latency_ms: audit.latency_ms,
            usage: audit.usage.clone(),
            created_at: now_rfc3339(),
        })],
    )
}

fn print_audit_warning(tool_name: &str, tool_call_id: &str, audit: &AuditReport) {
    let short_id = truncate_tool_call_id(tool_call_id);
    eprintln!(
        "  {RED}audit{RESET} {RED}{tool_name}{RESET} {DIM}{short_id}{RESET} {RED}{}{RESET}",
        audit.message
    );
}

fn print_audit_pass(tool_name: &str, tool_call_id: &str, audit: &AuditReport) {
    let short_id = truncate_tool_call_id(tool_call_id);
    eprintln!(
        "  {GREEN}audit{RESET} {GREEN}{tool_name}{RESET} {DIM}{short_id}{RESET} {GREEN}{}{RESET}",
        audit.message
    );
}

fn persist_partial_tool_turn(
    paths: &AppPaths,
    config: &AppConfig,
    args: &AskArgs,
    session_id: &str,
    session_preamble: &[ChatMessage],
    messages: &[ChatMessage],
) -> AppResult<()> {
    let auto_save = config.defaults.auto_save_session.unwrap_or(true);
    if args.ephemeral || !auto_save {
        return Ok(());
    }

    let mut events = Vec::new();
    append_session_message_events(&mut events, session_preamble);
    append_session_message_events(&mut events, messages);

    if !events.is_empty() {
        append_events(paths, config, session_id, &events)?;
    }
    let temp = is_temp_session(session_id) || args.temp;
    set_current_session(paths, config, Some(session_id), temp)
}

fn select_session_id(
    paths: &AppPaths,
    config: &AppConfig,
    session: Option<&str>,
    new_session: bool,
    temp: bool,
    ephemeral: bool,
) -> AppResult<String> {
    if let Some(session_id) = session {
        return resolve_or_allow_unsaved_session_id(paths, config, session_id);
    }
    if let Some(session_id) = requested_session_id(new_session, temp, ephemeral) {
        return Ok(session_id);
    }
    if let Some(current_session) = load_state(paths)?.current_session {
        return Ok(current_session);
    }
    Ok(generate_session_id())
}

fn requested_session_id(new_session: bool, temp: bool, ephemeral: bool) -> Option<String> {
    if temp {
        Some(generate_temp_session_id())
    } else if new_session || ephemeral {
        Some(generate_session_id())
    } else {
        None
    }
}

fn resolve_or_allow_unsaved_session_id(
    paths: &AppPaths,
    config: &AppConfig,
    input: &str,
) -> AppResult<String> {
    match resolve_session_id(paths, config, input) {
        Ok(session_id) => Ok(session_id),
        Err(err) if err.message.starts_with("no session matching `") => {
            let state = load_state(paths)?;
            let current = state.current_session.unwrap_or_default();
            if current == input {
                return Ok(current);
            }
            let bare = current
                .strip_prefix("sess_")
                .or_else(|| current.strip_prefix("tmp_"))
                .unwrap_or(&current);
            if current.starts_with(input) || bare.starts_with(input) {
                return Ok(current);
            }
            Err(err)
        }
        Err(err) => Err(err),
    }
}

fn ensure_repl_session_id(
    paths: &AppPaths,
    config: &AppConfig,
    session_id: &mut String,
    temp: bool,
) -> AppResult<bool> {
    let current_exists = crate::session::session_file(paths, config, session_id).exists();
    let state = load_state(paths)?;
    let state_matches = state.current_session.as_deref() == Some(session_id.as_str());

    if current_exists || state_matches {
        if !state_matches {
            set_current_session(paths, config, Some(session_id), temp)?;
        }
        return Ok(false);
    }

    let replacement = state.current_session.unwrap_or_else(|| {
        if temp {
            generate_temp_session_id()
        } else {
            generate_session_id()
        }
    });
    let replacement_temp = is_temp_session(&replacement) || temp;
    set_current_session(paths, config, Some(&replacement), replacement_temp)?;
    *session_id = replacement;
    Ok(true)
}

#[derive(Default)]
struct ReplState {
    stream: bool,
    tools: bool,
    context_status: ContextStatusMode,
    provider_override: Option<String>,
    model_override: Option<String>,
    panels: Vec<ReplRuntimePanel>,
    transient_panel: Option<ReplRuntimePanel>,
}

struct ReplInput {
    prompt: String,
    images: Vec<MessageImage>,
}

#[derive(Default, Debug, Clone)]
struct CollapsedPaste {
    text: String,
    line_count: usize,
}

#[derive(Default)]
struct ReplEditorDraft {
    buffer: String,
    images: Vec<MessageImage>,
    collapsed_pastes: Vec<CollapsedPaste>,
    status: Option<String>,
    cursor: usize,
    transcript_scroll: usize,
    esc_pending: bool,
    popup_selected: usize,
    popup_signature: Option<String>,
    slash_mode: Option<ReplSlashMode>,
}

enum ReplDirective {
    Continue,
    Exit,
    Submit(ReplInput),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplSlashCommand {
    Model,
    Session,
    Audit,
    ToolSearch,
    Status,
    New,
    Clear,
    Bash,
    Quit,
    Exit,
}

impl ReplSlashCommand {
    fn command(self) -> &'static str {
        match self {
            ReplSlashCommand::Model => "model",
            ReplSlashCommand::Session => "sessions",
            ReplSlashCommand::Audit => "audit",
            ReplSlashCommand::ToolSearch => "tool-search",
            ReplSlashCommand::Status => "status",
            ReplSlashCommand::New => "new",
            ReplSlashCommand::Clear => "clear",
            ReplSlashCommand::Bash => "bash",
            ReplSlashCommand::Quit => "quit",
            ReplSlashCommand::Exit => "exit",
        }
    }

    fn usage(self) -> &'static str {
        match self {
            ReplSlashCommand::Model => "/model [list [provider]|reset|<id>|<provider>/<model>]",
            ReplSlashCommand::Session => "/sessions [list|current|switch <id>]",
            ReplSlashCommand::Audit => "/audit [on|off|status]",
            ReplSlashCommand::ToolSearch => "/tool-search [on|off|status]",
            ReplSlashCommand::Status => "/status",
            ReplSlashCommand::New => "/new [temp]",
            ReplSlashCommand::Clear => "/clear",
            ReplSlashCommand::Bash => "/bash ...",
            ReplSlashCommand::Quit => "/quit",
            ReplSlashCommand::Exit => "/exit",
        }
    }

    fn description(self) -> &'static str {
        match self {
            ReplSlashCommand::Model => "choose the model for new turns",
            ReplSlashCommand::Session => "list or switch chat sessions",
            ReplSlashCommand::Audit => "toggle audit checks for dangerous tools",
            ReplSlashCommand::ToolSearch => "toggle progressive tool loading",
            ReplSlashCommand::Status => "show current REPL session and runtime config",
            ReplSlashCommand::New => "start a new chat session",
            ReplSlashCommand::Clear => "clear the current session history",
            ReplSlashCommand::Bash => "inspect or continue interactive bash sessions",
            ReplSlashCommand::Quit | ReplSlashCommand::Exit => "exit the REPL",
        }
    }
}

const REPL_SLASH_COMMANDS: &[ReplSlashCommand] = &[
    ReplSlashCommand::Model,
    ReplSlashCommand::Session,
    ReplSlashCommand::Audit,
    ReplSlashCommand::ToolSearch,
    ReplSlashCommand::Status,
    ReplSlashCommand::New,
    ReplSlashCommand::Clear,
    ReplSlashCommand::Bash,
    ReplSlashCommand::Quit,
    ReplSlashCommand::Exit,
];

async fn handle_repl(
    cli: &Cli,
    paths: &AppPaths,
    config: &mut AppConfig,
    secrets: &SecretsConfig,
    args: ReplArgs,
) -> AppResult<()> {
    if args.new_session && args.session.is_some() {
        return Err(AppError::new(
            EXIT_ARGS,
            "--new-session cannot be used together with --session",
        ));
    }
    let mut session_id = select_session_id(
        paths,
        config,
        args.session.as_deref(),
        args.new_session,
        args.temp,
        args.ephemeral,
    )?;
    let mut session_temp = is_temp_session(&session_id) || args.temp;
    set_current_session(paths, config, Some(&session_id), session_temp)?;
    let mut first_turn = true;
    let mut repl_state = ReplState {
        stream: !args.no_stream || args.stream,
        tools: args.tools || config.defaults.tools.unwrap_or(true),
        context_status: crate::context::resolve_context_status_mode(
            args.context_status,
            config.defaults.context_status,
        ),
        provider_override: None,
        model_override: None,
        panels: Vec::new(),
        transient_panel: None,
    };
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    loop {
        if ensure_repl_session_id(paths, config, &mut session_id, session_temp)? {
            session_temp = is_temp_session(&session_id) || args.temp;
        }
        sync_repl_todo_panel(paths, config, &session_id, &mut repl_state);
        let input = read_repl_input(
            &stdin,
            &mut stdout,
            args.multiline,
            cli,
            paths,
            config,
            &session_id,
            &mut repl_state,
        )?;
        match handle_repl_directive(
            input,
            &mut repl_state,
            &mut stdout,
            cli,
            paths,
            config,
            &mut session_id,
            &mut first_turn,
            args.temp,
        )? {
            ReplDirective::Continue => continue,
            ReplDirective::Exit => break,
            ReplDirective::Submit(input) => {
                if ensure_repl_session_id(paths, config, &mut session_id, session_temp)? {
                    session_temp = is_temp_session(&session_id) || args.temp;
                }
                render_repl_submission_preview(
                    &mut stdout,
                    paths,
                    config,
                    &session_id,
                    terminal::size()
                        .map(|(cols, _)| cols.max(1) as usize)
                        .unwrap_or(80),
                    &input.prompt,
                    &input.images,
                )?;
                let submitted_prompt = input.prompt.clone();
                let submitted_images = input.images.clone();
                let ask_args = AskArgs {
                    prompt: Some(input.prompt),
                    stdin: false,
                    system: if first_turn {
                        args.system.clone()
                    } else {
                        None
                    },
                    attachments: Vec::new(),
                    images: Vec::new(),
                    clipboard_image: false,
                    preloaded_images: input.images,
                    session: Some(session_id.clone()),
                    new_session: false,
                    ephemeral: args.ephemeral,
                    temp: args.temp,
                    tools: repl_state.tools,
                    yes: args.yes,
                    stream: repl_state.stream,
                    temperature: None,
                    max_output_tokens: None,
                    params: Vec::new(),
                    timeout: None,
                    raw_provider_response: false,
                    context_status: Some(repl_state.context_status),
                };
                let use_tools = ask_args.tools;
                let turn_cli = repl_runtime_cli(cli, config, &repl_state);
                let execution: AppResult<()> = if use_tools {
                    match execute_ask_with_tools(
                        &turn_cli,
                        paths,
                        config,
                        secrets,
                        &ask_args,
                        Some(OutputFormat::Text),
                        false,
                    )
                    .await
                    {
                        Ok(result) => {
                            if !repl_state.stream {
                                println!("{}", format_final_ask_output(config, &result, false)?);
                            }
                            Ok(())
                        }
                        Err(err) => Err(err),
                    }
                } else if repl_state.stream {
                    execute_ask_stream(
                        &turn_cli,
                        paths,
                        config,
                        secrets,
                        &ask_args,
                        Some(OutputFormat::Text),
                        false,
                    )
                    .await
                } else {
                    match execute_ask(
                        &turn_cli,
                        paths,
                        config,
                        secrets,
                        &ask_args,
                        Some(OutputFormat::Text),
                    )
                    .await
                    {
                        Ok(result) => {
                            println!("{}", format_final_ask_output(config, &result, false)?);
                            Ok(())
                        }
                        Err(err) => Err(err),
                    }
                };
                match execution {
                    Ok(()) => {
                        first_turn = false;
                    }
                    Err(err) => {
                        push_repl_panel(
                            &mut repl_state,
                            build_repl_user_panel(config, &submitted_prompt, &submitted_images),
                        );
                        push_repl_panel(&mut repl_state, build_repl_error_panel(&err.message));
                    }
                }
            }
        }
    }
    Ok(())
}

fn read_repl_input(
    stdin: &io::Stdin,
    stdout: &mut io::Stdout,
    multiline: bool,
    cli: &Cli,
    paths: &AppPaths,
    config: &AppConfig,
    session_id: &str,
    state: &mut ReplState,
) -> AppResult<ReplInput> {
    if stdin.is_terminal() && stdout.is_terminal() {
        return read_repl_tui_input(stdout, cli, paths, config, session_id, state);
    }
    Ok(ReplInput {
        prompt: read_repl_prompt(stdin, stdout, multiline)?,
        images: Vec::new(),
    })
}

fn read_repl_prompt(
    stdin: &io::Stdin,
    stdout: &mut io::Stdout,
    multiline: bool,
) -> AppResult<String> {
    if !multiline {
        return read_single_repl_line(stdin, stdout, "> ");
    }

    let mut lines = Vec::new();
    loop {
        let prompt = if lines.is_empty() { "> " } else { "| " };
        let line = read_single_repl_line(stdin, stdout, prompt)?;
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if lines.is_empty() && (trimmed == "/exit" || trimmed == "/quit") {
            return Ok(trimmed.to_string());
        }
        if trimmed == "." {
            break;
        }
        lines.push(trimmed.to_string());
    }
    Ok(lines.join("\n"))
}

struct ReplTerminalGuard {
    supports_keyboard_enhancement: bool,
}

#[derive(Debug, Clone)]
struct ReplScreenView {
    transcript: String,
    provider_id: String,
    model_id: String,
    cwd_label: String,
    session_short: String,
    stream: bool,
    tools: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplRuntimePanelKind {
    Status,
    Error,
    User,
    Todo,
}

#[derive(Debug, Clone)]
struct ReplRuntimePanel {
    kind: ReplRuntimePanelKind,
    title: String,
    body: String,
}

#[derive(Debug, Clone, Copy)]
struct ReplLayout {
    transcript_height: u16,
    popup_top: u16,
    popup_height: u16,
    composer_top: u16,
    composer_body_height: u16,
    status_row: u16,
}

#[derive(Debug, Clone)]
struct WrappedInput {
    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
}

#[derive(Debug, Clone)]
struct ReplComposerView {
    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
}

#[derive(Debug, Clone)]
struct ReplSlashPopupView {
    lines: Vec<String>,
}

#[derive(Debug, Clone)]
enum ReplPopupAction {
    EnterSlashMode(ReplSlashMode),
    SubmitPrompt(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplSlashMode {
    Commands,
    Model,
    Session,
    Audit,
    ToolSearch,
    Bash,
}

impl ReplSlashMode {
    fn command(self) -> Option<&'static str> {
        match self {
            Self::Commands => None,
            Self::Model => Some("model"),
            Self::Session => Some("sessions"),
            Self::Audit => Some("audit"),
            Self::ToolSearch => Some("tool-search"),
            Self::Bash => Some("bash"),
        }
    }

    fn label(self) -> &'static str {
        self.command().unwrap_or("commands")
    }

    fn placeholder(self) -> &'static str {
        match self {
            Self::Commands => "Filter slash commands. Press Enter to apply.",
            Self::Model => "Filter models. Press Enter to apply.",
            Self::Session => "Filter sessions. Press Enter to switch.",
            Self::Audit => "Choose an audit setting. Press Enter to apply.",
            Self::ToolSearch => "Choose a tool search setting. Press Enter to apply.",
            Self::Bash => "Choose a bash subcommand. Press Enter to apply.",
        }
    }
}

#[derive(Debug, Clone)]
struct ReplPopupItem {
    label: String,
    detail: String,
    action: ReplPopupAction,
}

#[derive(Debug, Clone)]
struct ReplPopupModel {
    signature: String,
    items: Vec<ReplPopupItem>,
}

const REPL_MIN_TRANSCRIPT_HEIGHT: u16 = 3;
const REPL_MAX_VISIBLE_POPUP_LINES: usize = 8;

impl ReplTerminalGuard {
    fn enter(stdout: &mut io::Stdout) -> AppResult<Self> {
        terminal::enable_raw_mode()
            .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to enable raw mode: {err}")))?;
        let supports_keyboard_enhancement =
            matches!(terminal::supports_keyboard_enhancement(), Ok(true));
        execute!(stdout, EnableBracketedPaste, EnableMouseCapture).map_err(|err| {
            AppError::new(
                EXIT_ARGS,
                format!("failed to initialize REPL terminal mode: {err}"),
            )
        })?;
        if supports_keyboard_enhancement {
            execute!(
                stdout,
                PushKeyboardEnhancementFlags(
                    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                        | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                )
            )
            .map_err(|err| {
                AppError::new(
                    EXIT_ARGS,
                    format!("failed to enable keyboard enhancement flags: {err}"),
                )
            })?;
        }
        Ok(Self {
            supports_keyboard_enhancement,
        })
    }
}

impl Drop for ReplTerminalGuard {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        if self.supports_keyboard_enhancement {
            let _ = queue!(stdout, PopKeyboardEnhancementFlags);
        }
        let _ = execute!(
            stdout,
            DisableMouseCapture,
            DisableBracketedPaste,
            cursor::Show
        );
        let _ = terminal::disable_raw_mode();
    }
}

fn read_repl_tui_input(
    stdout: &mut io::Stdout,
    cli: &Cli,
    paths: &AppPaths,
    config: &AppConfig,
    session_id: &str,
    state: &mut ReplState,
) -> AppResult<ReplInput> {
    let _guard = ReplTerminalGuard::enter(stdout)?;
    let (cols, _) = terminal::size()
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to read terminal size: {err}")))?;
    let view = build_repl_screen_view(cli, paths, config, session_id, cols, state);
    let mut draft = ReplEditorDraft {
        transcript_scroll: usize::MAX,
        ..ReplEditorDraft::default()
    };

    loop {
        let popup_model = repl_popup_model(&draft, cli, paths, config, state);
        sync_repl_popup_selection(&mut draft, popup_model.as_ref());
        render_repl_editor(stdout, &view, &draft, popup_model.as_ref())?;
        let event = event::read().map_err(|err| {
            AppError::new(EXIT_ARGS, format!("failed to read terminal event: {err}"))
        })?;
        match event {
            Event::Resize(_, _) => {}
            Event::Paste(text) => {
                clear_repl_transient_panel(state);
                draft.esc_pending = false;
                handle_pasted_text(&mut draft, text);
            }
            Event::Mouse(mouse) => {
                clear_repl_transient_panel(state);
                draft.esc_pending = false;
                match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        draft.transcript_scroll =
                            transcript_scroll_page_up(&view.transcript, &draft, 3)?;
                    }
                    MouseEventKind::ScrollDown => {
                        draft.transcript_scroll =
                            transcript_scroll_page_down(&view.transcript, &draft, 3)?;
                    }
                    _ => {}
                }
            }
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                clear_repl_transient_panel(state);
                let modifiers = key.modifiers;
                match key.code {
                    KeyCode::Char('/')
                        if modifiers.is_empty()
                            && draft.slash_mode.is_none()
                            && draft.buffer.is_empty()
                            && draft.images.is_empty()
                            && draft.collapsed_pastes.is_empty() =>
                    {
                        draft.esc_pending = false;
                        enter_repl_slash_mode(&mut draft, ReplSlashMode::Commands);
                    }
                    KeyCode::Up if modifiers.is_empty() && popup_model.is_some() => {
                        draft.esc_pending = false;
                        move_repl_popup_selection(&mut draft, popup_model.as_ref(), -1);
                    }
                    KeyCode::Down if modifiers.is_empty() && popup_model.is_some() => {
                        draft.esc_pending = false;
                        move_repl_popup_selection(&mut draft, popup_model.as_ref(), 1);
                    }
                    KeyCode::Tab if modifiers.is_empty() && popup_model.is_some() => {
                        draft.esc_pending = false;
                        if let Some(action) =
                            accept_repl_popup_selection(&draft, popup_model.as_ref())
                        {
                            match action {
                                ReplPopupAction::EnterSlashMode(mode) => {
                                    enter_repl_slash_mode(&mut draft, mode);
                                    draft.status = None;
                                }
                                ReplPopupAction::SubmitPrompt(prompt) => {
                                    clear_repl_surface(stdout)?;
                                    return Ok(ReplInput {
                                        prompt,
                                        images: Vec::new(),
                                    });
                                }
                            }
                        }
                    }
                    KeyCode::Enter if modifiers.is_empty() => {
                        if let Some(action) =
                            accept_repl_popup_selection(&draft, popup_model.as_ref())
                        {
                            match action {
                                ReplPopupAction::EnterSlashMode(mode) => {
                                    enter_repl_slash_mode(&mut draft, mode);
                                    draft.status = None;
                                    continue;
                                }
                                ReplPopupAction::SubmitPrompt(prompt) => {
                                    clear_repl_surface(stdout)?;
                                    return Ok(ReplInput {
                                        prompt,
                                        images: Vec::new(),
                                    });
                                }
                            }
                        }
                        if let Some(prompt) = manual_repl_slash_prompt(&draft) {
                            clear_repl_surface(stdout)?;
                            return Ok(ReplInput {
                                prompt,
                                images: Vec::new(),
                            });
                        }
                        if draft.slash_mode.is_some() {
                            draft.status = Some("No matching slash command.".to_string());
                            continue;
                        }
                        if draft.buffer.trim().is_empty()
                            && draft.images.is_empty()
                            && draft.collapsed_pastes.is_empty()
                        {
                            draft.status = Some(
                                "Input is empty. Press Ctrl+V to attach a clipboard image."
                                    .to_string(),
                            );
                            continue;
                        }
                        clear_repl_surface(stdout)?;
                        return Ok(ReplInput {
                            prompt: materialize_repl_prompt(&draft),
                            images: draft.images,
                        });
                    }
                    KeyCode::Enter
                        if modifiers.contains(KeyModifiers::CONTROL)
                            || modifiers.contains(KeyModifiers::SHIFT) =>
                    {
                        draft.esc_pending = false;
                        insert_text_at_cursor(&mut draft, "\n");
                        draft.status = None;
                    }
                    KeyCode::Char('j') | KeyCode::Char('J')
                        if modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        draft.esc_pending = false;
                        insert_text_at_cursor(&mut draft, "\n");
                        draft.status = None;
                    }
                    KeyCode::PageUp => {
                        draft.esc_pending = false;
                        let page = transcript_page_step()?;
                        draft.transcript_scroll =
                            transcript_scroll_page_up(&view.transcript, &draft, page)?;
                    }
                    KeyCode::PageDown => {
                        draft.esc_pending = false;
                        let page = transcript_page_step()?;
                        draft.transcript_scroll =
                            transcript_scroll_page_down(&view.transcript, &draft, page)?;
                    }
                    KeyCode::Up if modifiers.contains(KeyModifiers::CONTROL) => {
                        draft.esc_pending = false;
                        draft.transcript_scroll =
                            transcript_scroll_page_up(&view.transcript, &draft, 3)?;
                    }
                    KeyCode::Down if modifiers.contains(KeyModifiers::CONTROL) => {
                        draft.esc_pending = false;
                        draft.transcript_scroll =
                            transcript_scroll_page_down(&view.transcript, &draft, 3)?;
                    }
                    KeyCode::Char('c') | KeyCode::Char('C')
                        if modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        clear_repl_surface(stdout)?;
                        return Ok(ReplInput {
                            prompt: "/exit".to_string(),
                            images: Vec::new(),
                        });
                    }
                    KeyCode::Char('v') | KeyCode::Char('V')
                        if modifiers.contains(KeyModifiers::CONTROL)
                            && modifiers.contains(KeyModifiers::SHIFT) =>
                    {
                        draft.esc_pending = false;
                        match read_clipboard_text() {
                            Ok(text) => handle_pasted_text(&mut draft, text),
                            Err(err) => draft.status = Some(err.message),
                        }
                    }
                    KeyCode::Char('v') | KeyCode::Char('V')
                        if modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        draft.esc_pending = false;
                        match read_clipboard_image() {
                            Ok(image) => {
                                draft.images.push(image);
                                draft.status = None;
                            }
                            Err(err) => draft.status = Some(err.message),
                        }
                    }
                    KeyCode::Backspace => {
                        draft.esc_pending = false;
                        if delete_prev_char_at_cursor(&mut draft) {
                            draft.status = None;
                        } else if draft.slash_mode.take().is_some() {
                            draft.status = None;
                            draft.popup_selected = 0;
                            draft.popup_signature = None;
                        } else if draft.collapsed_pastes.pop().is_some() {
                            draft.status = Some(format!(
                                "Removed one pasted text block. {} remaining.",
                                draft.collapsed_pastes.len()
                            ));
                        } else if draft.images.pop().is_some() {
                            draft.status = Some(format!(
                                "Removed one image. {} remaining.",
                                draft.images.len()
                            ));
                        }
                    }
                    KeyCode::Delete => {
                        draft.esc_pending = false;
                        if delete_char_at_cursor(&mut draft) {
                            draft.status = None;
                        }
                    }
                    KeyCode::Left => {
                        draft.esc_pending = false;
                        move_cursor_left(&mut draft);
                    }
                    KeyCode::Right => {
                        draft.esc_pending = false;
                        move_cursor_right(&mut draft);
                    }
                    KeyCode::Home => {
                        draft.esc_pending = false;
                        move_cursor_line_start(&mut draft);
                    }
                    KeyCode::End => {
                        draft.esc_pending = false;
                        move_cursor_line_end(&mut draft);
                    }
                    KeyCode::Tab => {
                        draft.esc_pending = false;
                        if draft.slash_mode.is_none() {
                            insert_text_at_cursor(&mut draft, "    ");
                            draft.status = None;
                        }
                    }
                    KeyCode::Esc => {
                        if draft.esc_pending {
                            clear_repl_text_input(&mut draft);
                            draft.status = Some("Input cleared.".to_string());
                        } else {
                            draft.esc_pending = true;
                            if draft.slash_mode.is_some() {
                                draft.status =
                                    Some("Press Esc again to leave slash mode.".to_string());
                            } else {
                                draft.status =
                                    Some("Press Esc again to clear the input.".to_string());
                            }
                        }
                    }
                    KeyCode::Char(ch) if !modifiers.contains(KeyModifiers::CONTROL) => {
                        draft.esc_pending = false;
                        insert_char_at_cursor(&mut draft, ch);
                        draft.status = None;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

fn build_repl_screen_view(
    cli: &Cli,
    paths: &AppPaths,
    config: &AppConfig,
    session_id: &str,
    cols: u16,
    state: &ReplState,
) -> ReplScreenView {
    let provider_id =
        repl_effective_provider_id(cli, config, state).unwrap_or_else(|| "-".to_string());
    let model_id = repl_effective_model_id(cli, config, state).unwrap_or_else(|| "-".to_string());
    let cwd_label = std::env::current_dir()
        .ok()
        .and_then(|cwd| {
            cwd.file_name()
                .map(|name| name.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| ".".to_string());
    let transcript = render_repl_transcript(
        paths,
        config,
        session_id,
        cols.max(1) as usize,
        &state.panels,
        state.transient_panel.as_ref(),
    );
    ReplScreenView {
        transcript,
        provider_id,
        model_id,
        cwd_label,
        session_short: short_id(session_id),
        stream: state.stream,
        tools: state.tools,
    }
}

fn render_repl_editor(
    stdout: &mut io::Stdout,
    view: &ReplScreenView,
    draft: &ReplEditorDraft,
    popup_model: Option<&ReplPopupModel>,
) -> AppResult<()> {
    let (cols, rows) = terminal::size()
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to read terminal size: {err}")))?;
    if cols == 0 || rows == 0 {
        return Ok(());
    }

    let composer_body_height = repl_composer_body_height(cols, rows, draft);
    let popup_max_height = max_repl_popup_height(rows, composer_body_height) as usize;
    let popup_view = repl_slash_popup_view(popup_model, draft, cols, popup_max_height);
    let layout = compute_repl_layout(rows, composer_body_height, popup_view.lines.len() as u16);
    let transcript_lines = clipped_repl_transcript_lines(
        &view.transcript,
        cols,
        layout.transcript_height,
        draft.transcript_scroll,
    );
    let composer_view = repl_composer_view(draft, cols, layout.composer_body_height);
    let status_bar = build_repl_status_bar(view, draft, cols);

    execute!(stdout, MoveTo(0, 0), Clear(ClearType::All))
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to draw REPL editor: {err}")))?;

    for (row, line) in transcript_lines.iter().enumerate() {
        write_repl_line(stdout, row as u16, line)?;
    }

    draw_repl_slash_popup(stdout, cols, &layout, &popup_view)?;
    draw_repl_composer(stdout, cols, &layout, &composer_view)?;
    write_repl_line(stdout, layout.status_row, &status_bar)?;

    let cursor = repl_cursor_position(cols, &layout, &composer_view);
    execute!(stdout, MoveTo(cursor.0, cursor.1), cursor::Show)
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to place REPL cursor: {err}")))?;
    stdout
        .flush()
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to flush REPL editor: {err}")))?;
    Ok(())
}

fn repl_composer_body_height(cols: u16, rows: u16, draft: &ReplEditorDraft) -> u16 {
    let composer_view = repl_composer_view(draft, cols, u16::MAX);
    let max_body = min(8usize, rows.saturating_sub(4) as usize).max(3);
    min(composer_view.lines.len(), max_body).max(3) as u16
}

fn max_repl_popup_height(rows: u16, composer_body_height: u16) -> u16 {
    rows.saturating_sub(composer_body_height + 3 + REPL_MIN_TRANSCRIPT_HEIGHT)
}

fn compute_repl_layout(rows: u16, composer_body_height: u16, popup_height: u16) -> ReplLayout {
    let status_row = rows.saturating_sub(1);
    let composer_top = rows.saturating_sub(composer_body_height + 3 + popup_height);
    let popup_top = composer_top + composer_body_height + 2;
    let transcript_height = composer_top;
    ReplLayout {
        transcript_height,
        popup_top,
        popup_height,
        composer_top,
        composer_body_height,
        status_row,
    }
}

fn repl_composer_view(draft: &ReplEditorDraft, cols: u16, max_lines: u16) -> ReplComposerView {
    let inner_width = cols.saturating_sub(4).max(8) as usize;
    let mut lines = Vec::new();

    let badges = repl_attachment_badges(draft);
    let slash_badge = draft
        .slash_mode
        .and_then(|mode| {
            mode.command()
                .map(|command| format!("{DIM}/{command}{RESET} "))
        })
        .unwrap_or_default();
    let prompt_prefix = if badges.is_empty() {
        format!("❯ {slash_badge}")
    } else if slash_badge.is_empty() {
        format!("❯ {badges} ")
    } else {
        format!("❯ {badges} {slash_badge}")
    };
    let prompt_prefix = if prompt_prefix.trim().is_empty() {
        "❯ ".to_string()
    } else {
        prompt_prefix
    };
    let prefix_width = display_width(&prompt_prefix);
    let wrapped_input = wrap_editor_input(
        &draft.buffer,
        draft.cursor.min(draft.buffer.len()),
        inner_width.saturating_sub(prefix_width).max(1),
    );
    let fixed_lines = lines.len();
    let (cursor_row, cursor_col) = if draft.buffer.is_empty() {
        let placeholder = draft
            .slash_mode
            .map(ReplSlashMode::placeholder)
            .unwrap_or("Type a message. Press /commands for help.");
        lines.push(format!("{prompt_prefix}{DIM}{placeholder}{RESET}"));
        (fixed_lines, prefix_width)
    } else {
        for (index, line) in wrapped_input.lines.iter().enumerate() {
            let prefix = if index == 0 {
                prompt_prefix.as_str()
            } else {
                "  "
            };
            lines.push(format!("{prefix}{line}"));
        }
        (
            fixed_lines + wrapped_input.cursor_row,
            prefix_width + wrapped_input.cursor_col,
        )
    };

    let max_lines = max_lines as usize;
    if lines.len() > max_lines {
        let start = cursor_row.saturating_sub(max_lines.saturating_sub(1));
        let end = min(start + max_lines, lines.len());
        ReplComposerView {
            lines: lines[start..end].to_vec(),
            cursor_row: cursor_row.saturating_sub(start),
            cursor_col,
        }
    } else {
        ReplComposerView {
            lines,
            cursor_row,
            cursor_col,
        }
    }
}

fn repl_slash_popup_view(
    popup_model: Option<&ReplPopupModel>,
    draft: &ReplEditorDraft,
    cols: u16,
    max_lines: usize,
) -> ReplSlashPopupView {
    let width = cols.saturating_sub(4).max(12) as usize;
    let Some(popup_model) = popup_model else {
        return ReplSlashPopupView { lines: Vec::new() };
    };
    if popup_model.items.is_empty() || max_lines == 0 {
        return ReplSlashPopupView {
            lines: vec![format!("{DIM}  no matching options{RESET}")],
        };
    }

    let visible_count = popup_model
        .items
        .len()
        .min(max_lines)
        .min(REPL_MAX_VISIBLE_POPUP_LINES);
    let start = popup_window_start(draft.popup_selected, popup_model.items.len(), visible_count);
    let mut lines = Vec::new();
    for (index, item) in popup_model
        .items
        .iter()
        .enumerate()
        .skip(start)
        .take(visible_count)
    {
        let selected = index == draft.popup_selected;
        let line = if selected {
            format!(
                "\x1b[48;5;240m\x1b[38;5;231m› {:<14}{}\x1b[0m",
                item.label, item.detail
            )
        } else {
            format!(
                "{CYAN}  {:<12}{RESET} {DIM}{}{RESET}",
                item.label, item.detail
            )
        };
        lines.push(ansi_truncate(&line, width));
    }
    ReplSlashPopupView { lines }
}

fn popup_window_start(selected: usize, total_items: usize, visible_count: usize) -> usize {
    if total_items <= visible_count || visible_count == 0 {
        return 0;
    }
    selected
        .saturating_sub(visible_count / 2)
        .min(total_items - visible_count)
}

fn repl_popup_model(
    draft: &ReplEditorDraft,
    _cli: &Cli,
    paths: &AppPaths,
    config: &AppConfig,
    _state: &ReplState,
) -> Option<ReplPopupModel> {
    if let Some(mode) = draft.slash_mode {
        let query = draft.buffer.trim();
        let items = match mode {
            ReplSlashMode::Commands => filtered_repl_slash_commands(query)
                .into_iter()
                .map(|command| ReplPopupItem {
                    label: format!("/{}", command.command()),
                    detail: command.description().to_string(),
                    action: match command {
                        ReplSlashCommand::Model => {
                            ReplPopupAction::EnterSlashMode(ReplSlashMode::Model)
                        }
                        ReplSlashCommand::Session => {
                            ReplPopupAction::EnterSlashMode(ReplSlashMode::Session)
                        }
                        ReplSlashCommand::Audit => {
                            ReplPopupAction::EnterSlashMode(ReplSlashMode::Audit)
                        }
                        ReplSlashCommand::ToolSearch => {
                            ReplPopupAction::EnterSlashMode(ReplSlashMode::ToolSearch)
                        }
                        ReplSlashCommand::Bash => {
                            ReplPopupAction::EnterSlashMode(ReplSlashMode::Bash)
                        }
                        _ => ReplPopupAction::SubmitPrompt(format!("/{}", command.command())),
                    },
                })
                .collect::<Vec<_>>(),
            ReplSlashMode::Model => build_repl_model_popup_items(config, query),
            ReplSlashMode::Session => build_repl_session_popup_items(paths, config, query),
            ReplSlashMode::Audit => {
                build_repl_toggle_popup_items("audit", config.audit.enabled.unwrap_or(false), query)
            }
            ReplSlashMode::ToolSearch => build_repl_toggle_popup_items(
                "tool-search",
                config.tools.progressive_loading.unwrap_or(false),
                query,
            ),
            ReplSlashMode::Bash => build_repl_bash_popup_items(query),
        };
        if items.is_empty() {
            return None;
        }
        return Some(ReplPopupModel {
            signature: format!("slash:{mode:?}:{query}"),
            items,
        });
    }

    let first_line = draft.buffer.lines().next().unwrap_or_default();
    let stripped = first_line.strip_prefix('/')?;
    let items = filtered_repl_slash_commands(stripped)
        .into_iter()
        .map(|command| ReplPopupItem {
            label: format!("/{}", command.command()),
            detail: command.description().to_string(),
            action: match command {
                ReplSlashCommand::Model => ReplPopupAction::EnterSlashMode(ReplSlashMode::Model),
                ReplSlashCommand::Session => {
                    ReplPopupAction::EnterSlashMode(ReplSlashMode::Session)
                }
                ReplSlashCommand::Audit => ReplPopupAction::EnterSlashMode(ReplSlashMode::Audit),
                ReplSlashCommand::ToolSearch => {
                    ReplPopupAction::EnterSlashMode(ReplSlashMode::ToolSearch)
                }
                ReplSlashCommand::Bash => ReplPopupAction::EnterSlashMode(ReplSlashMode::Bash),
                _ => ReplPopupAction::SubmitPrompt(format!("/{}", command.command())),
            },
        })
        .collect::<Vec<_>>();
    if items.is_empty() {
        return None;
    }
    Some(ReplPopupModel {
        signature: first_line.to_string(),
        items,
    })
}

fn build_repl_session_popup_items(
    paths: &AppPaths,
    config: &AppConfig,
    query: &str,
) -> Vec<ReplPopupItem> {
    let current = load_state(paths)
        .ok()
        .and_then(|state| state.current_session);
    let mut items = vec![ReplPopupItem {
        label: "list".to_string(),
        detail: "show saved sessions".to_string(),
        action: ReplPopupAction::SubmitPrompt("/sessions list".to_string()),
    }];
    if let Some(current_session) = current.as_deref() {
        items.push(ReplPopupItem {
            label: "current".to_string(),
            detail: format!("active {}", short_id(current_session)),
            action: ReplPopupAction::SubmitPrompt("/sessions current".to_string()),
        });
    }
    let summaries = list_session_summaries(paths, config, current.as_deref()).unwrap_or_default();
    for summary in summaries {
        let short = short_id(&summary.session_id);
        if !matches_popup_query(&short, query)
            && !summary
                .first_prompt
                .as_deref()
                .is_some_and(|prompt| matches_popup_query(prompt, query))
        {
            continue;
        }
        let detail = summary
            .first_prompt
            .as_deref()
            .map(|prompt| truncate_plain_to_width(prompt, 36))
            .unwrap_or_else(|| "(empty)".to_string());
        items.push(ReplPopupItem {
            label: short.clone(),
            detail,
            action: ReplPopupAction::SubmitPrompt(format!("/sessions switch {short}")),
        });
    }
    items
}

fn build_repl_bash_popup_items(query: &str) -> Vec<ReplPopupItem> {
    [
        ("help", "show bash REPL subcommands"),
        ("sessions", "list interactive bash sessions"),
    ]
    .into_iter()
    .filter(|(label, _)| matches_popup_query(label, query))
    .map(|(label, detail)| ReplPopupItem {
        label: label.to_string(),
        detail: detail.to_string(),
        action: ReplPopupAction::SubmitPrompt(format!("/bash {label}")),
    })
    .collect()
}

fn build_repl_model_popup_items(config: &AppConfig, query: &str) -> Vec<ReplPopupItem> {
    let mut items = vec![ReplPopupItem {
        label: "reset".to_string(),
        detail: "use CLI/config/default provider model".to_string(),
        action: ReplPopupAction::SubmitPrompt("/model reset".to_string()),
    }];
    let mut models = config.models.iter().collect::<Vec<_>>();
    models.sort_by(|left, right| {
        format_model_list_entry(left.1)
            .cmp(&format_model_list_entry(right.1))
            .then_with(|| left.0.cmp(right.0))
    });
    for (model_id, model) in models {
        let label = format!("{}/{}", model.provider, model.remote_name);
        if !matches_popup_query(&label, query) && !matches_popup_query(model_id, query) {
            continue;
        }
        items.push(ReplPopupItem {
            label,
            detail: format!("id={model_id}"),
            action: ReplPopupAction::SubmitPrompt(format!("/model {model_id}")),
        });
    }
    items
}

fn build_repl_toggle_popup_items(name: &str, current: bool, query: &str) -> Vec<ReplPopupItem> {
    ["status", "on", "off"]
        .into_iter()
        .filter(|value| matches_popup_query(value, query))
        .map(|value| ReplPopupItem {
            label: value.to_string(),
            detail: match value {
                "status" => format!("current {}", if current { "on" } else { "off" }),
                "on" if current => "current".to_string(),
                "off" if !current => "current".to_string(),
                _ => "update config".to_string(),
            },
            action: ReplPopupAction::SubmitPrompt(format!("/{name} {value}")),
        })
        .collect()
}

fn matches_popup_query(candidate: &str, query: &str) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    let candidate = candidate.to_ascii_lowercase();
    let query = query.to_ascii_lowercase();
    candidate.starts_with(&query) || candidate.contains(&query)
}

fn sync_repl_popup_selection(draft: &mut ReplEditorDraft, popup_model: Option<&ReplPopupModel>) {
    let Some(popup_model) = popup_model else {
        draft.popup_selected = 0;
        draft.popup_signature = None;
        return;
    };
    if draft.popup_signature.as_deref() != Some(popup_model.signature.as_str()) {
        draft.popup_selected = 0;
        draft.popup_signature = Some(popup_model.signature.clone());
    }
    if draft.popup_selected >= popup_model.items.len() {
        draft.popup_selected = 0;
    }
}

fn move_repl_popup_selection(
    draft: &mut ReplEditorDraft,
    popup_model: Option<&ReplPopupModel>,
    delta: isize,
) {
    let Some(popup_model) = popup_model else {
        return;
    };
    let len = popup_model.items.len();
    if len == 0 {
        draft.popup_selected = 0;
        return;
    }
    let current = draft.popup_selected % len;
    let next = ((current as isize + delta).rem_euclid(len as isize)) as usize;
    draft.popup_selected = next;
}

fn accept_repl_popup_selection(
    draft: &ReplEditorDraft,
    popup_model: Option<&ReplPopupModel>,
) -> Option<ReplPopupAction> {
    let popup_model = popup_model?;
    popup_model
        .items
        .get(draft.popup_selected)
        .map(|item| item.action.clone())
}

fn clipped_repl_transcript_lines(
    transcript: &str,
    cols: u16,
    height: u16,
    scroll_top: usize,
) -> Vec<String> {
    if height == 0 {
        return Vec::new();
    }
    let lines = transcript_screen_lines(transcript, cols);
    let top = resolve_transcript_scroll(lines.len(), height as usize, scroll_top);
    lines.into_iter().skip(top).take(height as usize).collect()
}

fn write_repl_line(stdout: &mut io::Stdout, row: u16, text: &str) -> AppResult<()> {
    execute!(stdout, MoveTo(0, row), Clear(ClearType::CurrentLine))
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to position REPL line: {err}")))?;
    write!(stdout, "{text}")
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to write REPL line: {err}")))?;
    Ok(())
}

fn clear_repl_surface(stdout: &mut io::Stdout) -> AppResult<()> {
    execute!(stdout, MoveTo(0, 0), Clear(ClearType::All))
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to clear REPL surface: {err}")))?;
    stdout
        .flush()
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to flush REPL surface: {err}")))?;
    Ok(())
}

fn draw_repl_composer(
    stdout: &mut io::Stdout,
    cols: u16,
    layout: &ReplLayout,
    composer_view: &ReplComposerView,
) -> AppResult<()> {
    let inner_width = cols.saturating_sub(2) as usize;
    let top_border = full_width_rule(cols);
    let bottom_border = full_width_rule(cols);
    write_repl_line(stdout, layout.composer_top, &top_border)?;

    for index in 0..layout.composer_body_height as usize {
        let content = composer_view.lines.get(index).cloned().unwrap_or_default();
        let truncated = ansi_truncate(&content, inner_width);
        let padding = inner_width.saturating_sub(visible_width(&truncated));
        let row = layout.composer_top + 1 + index as u16;
        let line = format!(" {truncated}{}", " ".repeat(padding));
        write_repl_line(stdout, row, &line)?;
    }
    write_repl_line(
        stdout,
        layout.composer_top + layout.composer_body_height + 1,
        &bottom_border,
    )?;
    Ok(())
}

fn full_width_rule(cols: u16) -> String {
    let width = cols.max(1) as usize;
    format!("{DIM}{}{RESET}", "─".repeat(width))
}

fn draw_repl_slash_popup(
    stdout: &mut io::Stdout,
    cols: u16,
    layout: &ReplLayout,
    popup_view: &ReplSlashPopupView,
) -> AppResult<()> {
    if popup_view.lines.is_empty() || layout.popup_height == 0 {
        return Ok(());
    }
    let inner_width = cols.saturating_sub(2) as usize;
    for index in 0..layout.popup_height as usize {
        let content = popup_view.lines.get(index).cloned().unwrap_or_default();
        let truncated = ansi_truncate(&content, inner_width);
        let padding = inner_width.saturating_sub(visible_width(&truncated));
        let row = layout.popup_top + index as u16;
        let line = format!(" {truncated}{}", " ".repeat(padding));
        write_repl_line(stdout, row, &line)?;
    }
    Ok(())
}

fn repl_cursor_position(
    cols: u16,
    layout: &ReplLayout,
    composer_view: &ReplComposerView,
) -> (u16, u16) {
    let max_x = cols.saturating_sub(2);
    (
        (1 + composer_view.cursor_col as u16).min(max_x),
        layout.composer_top + 1 + composer_view.cursor_row as u16,
    )
}

fn build_repl_status_bar(view: &ReplScreenView, draft: &ReplEditorDraft, cols: u16) -> String {
    let prompt_chars = materialize_repl_prompt(draft).chars().count();
    let left = format!(
        "{} │ {} │ {} │ {}",
        view.model_id, view.cwd_label, view.provider_id, view.session_short
    );
    let mode_text = draft
        .slash_mode
        .map(|mode| format!("slash /{}", mode.label()))
        .unwrap_or_default();
    let mut right_parts = Vec::new();
    if let Some(status) = &draft.status {
        right_parts.push(status.clone());
    }
    if !mode_text.is_empty() {
        right_parts.push(mode_text);
    }
    right_parts.push(format!(
        "{} images │ {} paste(s) │ {} chars │ stream {} │ tools {}",
        draft.images.len(),
        draft.collapsed_pastes.len(),
        prompt_chars,
        if view.stream { "on" } else { "off" },
        if view.tools { "on" } else { "off" }
    ));
    let right_text = right_parts.join(" │ ");
    let content = pad_status_bar_plain(&left, &right_text, cols as usize);
    format!("\x1b[48;5;236m\x1b[38;5;252m{content}\x1b[0m")
}

fn pad_status_bar_plain(left: &str, right: &str, width: usize) -> String {
    let left = format!(" {left} ");
    let right = format!(" {right} ");
    let left_width = display_width(&left);
    let right_width = display_width(&right);
    if left_width + right_width >= width {
        return truncate_plain_to_width(&left, width);
    }
    format!(
        "{left}{}{}",
        " ".repeat(width - left_width - right_width),
        right
    )
}

fn image_badges(count: usize) -> String {
    (1..=count)
        .map(|index| format!("[Image #{index}]"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn paste_badges(pastes: &[CollapsedPaste], image_offset: usize) -> String {
    pastes
        .iter()
        .enumerate()
        .map(|(index, paste)| {
            format!(
                "[Pasted text #{} +{} lines]",
                image_offset + index + 1,
                paste.line_count.saturating_sub(1)
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn repl_attachment_badges(draft: &ReplEditorDraft) -> String {
    let mut parts = Vec::new();
    if !draft.images.is_empty() {
        parts.push(image_badges(draft.images.len()));
    }
    if !draft.collapsed_pastes.is_empty() {
        parts.push(paste_badges(&draft.collapsed_pastes, draft.images.len()));
    }
    parts.join(" ")
}

fn join_inline_prompt_segments(segments: &[Option<String>]) -> String {
    segments
        .iter()
        .filter_map(|segment| segment.as_deref())
        .filter(|segment| !segment.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn ansi_truncate(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut rendered = String::new();
    let mut visible = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            rendered.push(ch);
            while let Some(next) = chars.next() {
                rendered.push(next);
                if next == 'm' {
                    break;
                }
            }
            continue;
        }
        let ch_width = display_width_char(ch);
        if visible + ch_width > width {
            break;
        }
        rendered.push(ch);
        visible += ch_width;
    }
    if rendered.contains('\x1b') && !rendered.ends_with(RESET) {
        rendered.push_str(RESET);
    }
    rendered
}

fn visible_width(text: &str) -> usize {
    let mut width = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            while let Some(next) = chars.next() {
                if next == 'm' {
                    break;
                }
            }
            continue;
        }
        width += display_width_char(ch);
    }
    width
}

fn wrap_editor_input(buffer: &str, cursor: usize, width: usize) -> WrappedInput {
    let mut lines = vec![String::new()];
    let mut row = 0usize;
    let mut col = 0usize;
    let mut cursor_row = 0usize;
    let mut cursor_col = 0usize;
    let mut cursor_set = cursor == 0;

    for (index, ch) in buffer.char_indices() {
        if !cursor_set && index == cursor {
            cursor_row = row;
            cursor_col = col;
            cursor_set = true;
        }

        if ch == '\n' {
            row += 1;
            col = 0;
            lines.push(String::new());
            continue;
        }

        let ch_width = display_width_char(ch);
        if !lines[row].is_empty() && col + ch_width > width {
            row += 1;
            col = 0;
            lines.push(String::new());
        }

        lines[row].push(ch);
        col += ch_width;

        let next = index + ch.len_utf8();
        if !cursor_set && next == cursor {
            cursor_row = row;
            cursor_col = col;
            cursor_set = true;
        }
    }

    if !cursor_set {
        cursor_row = row;
        cursor_col = col;
    }

    WrappedInput {
        lines,
        cursor_row,
        cursor_col,
    }
}

fn transcript_screen_lines(transcript: &str, cols: u16) -> Vec<String> {
    let width = cols.max(1) as usize;
    if transcript.trim().is_empty() {
        return vec![format!(
            "{DIM}No conversation yet. Start with a prompt below.{RESET}"
        )];
    }
    transcript
        .lines()
        .flat_map(|line| wrap_ansi_to_width(line, width))
        .collect()
}

fn resolve_transcript_scroll(total_lines: usize, height: usize, requested: usize) -> usize {
    let max_top = total_lines.saturating_sub(height);
    if requested == usize::MAX {
        max_top
    } else {
        requested.min(max_top)
    }
}

fn transcript_page_step() -> AppResult<usize> {
    let (_, rows) = terminal::size()
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to read terminal size: {err}")))?;
    Ok(rows.saturating_sub(8).max(1) as usize)
}

fn transcript_scroll_page_up(
    transcript: &str,
    draft: &ReplEditorDraft,
    amount: usize,
) -> AppResult<usize> {
    let (cols, rows) = terminal::size()
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to read terminal size: {err}")))?;
    let total = transcript_screen_lines(transcript, cols).len();
    let composer_body_height = repl_composer_body_height(cols, rows, draft);
    let height = compute_repl_layout(rows, composer_body_height, 0).transcript_height as usize;
    let current = resolve_transcript_scroll(total, height, draft.transcript_scroll);
    Ok(current.saturating_sub(amount.max(1)))
}

fn transcript_scroll_page_down(
    transcript: &str,
    draft: &ReplEditorDraft,
    amount: usize,
) -> AppResult<usize> {
    let (cols, rows) = terminal::size()
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to read terminal size: {err}")))?;
    let total = transcript_screen_lines(transcript, cols).len();
    let composer_body_height = repl_composer_body_height(cols, rows, draft);
    let height = compute_repl_layout(rows, composer_body_height, 0).transcript_height as usize;
    let current = resolve_transcript_scroll(total, height, draft.transcript_scroll);
    Ok(current
        .saturating_add(amount.max(1))
        .min(total.saturating_sub(height)))
}

fn handle_pasted_text(draft: &mut ReplEditorDraft, text: String) {
    if should_collapse_pasted_text(&text) {
        let line_count = text.lines().count().max(1);
        draft
            .collapsed_pastes
            .push(CollapsedPaste { text, line_count });
        draft.status = None;
    } else {
        insert_text_at_cursor(draft, &text);
        draft.status = None;
    }
}

fn should_collapse_pasted_text(text: &str) -> bool {
    let line_count = text.lines().count();
    line_count > 8 || text.chars().count() > 320
}

fn materialize_repl_prompt(draft: &ReplEditorDraft) -> String {
    let mut prompt = draft.buffer.clone();
    for paste in &draft.collapsed_pastes {
        if !prompt.is_empty() && !prompt.ends_with('\n') {
            prompt.push_str("\n\n");
        }
        prompt.push_str(&paste.text);
    }
    prompt
}

fn clear_repl_text_input(draft: &mut ReplEditorDraft) {
    draft.buffer.clear();
    draft.cursor = 0;
    draft.collapsed_pastes.clear();
    draft.esc_pending = false;
    draft.popup_selected = 0;
    draft.popup_signature = None;
    draft.slash_mode = None;
}

fn enter_repl_slash_mode(draft: &mut ReplEditorDraft, mode: ReplSlashMode) {
    draft.slash_mode = Some(mode);
    draft.buffer.clear();
    draft.cursor = 0;
    draft.popup_selected = 0;
    draft.popup_signature = None;
}

fn manual_repl_slash_prompt(draft: &ReplEditorDraft) -> Option<String> {
    let mode = draft.slash_mode?;
    let query = draft.buffer.trim();
    match mode {
        ReplSlashMode::Commands => {
            if query.is_empty() {
                None
            } else {
                Some(format!("/{query}"))
            }
        }
        ReplSlashMode::Audit | ReplSlashMode::ToolSearch | ReplSlashMode::Bash
            if query.is_empty() =>
        {
            None
        }
        _ => mode.command().map(|command| {
            if query.is_empty() {
                format!("/{command}")
            } else {
                format!("/{command} {query}")
            }
        }),
    }
}

fn insert_text_at_cursor(draft: &mut ReplEditorDraft, text: &str) {
    draft.buffer.insert_str(draft.cursor, text);
    draft.cursor += text.len();
}

fn insert_char_at_cursor(draft: &mut ReplEditorDraft, ch: char) {
    draft.buffer.insert(draft.cursor, ch);
    draft.cursor += ch.len_utf8();
}

fn delete_prev_char_at_cursor(draft: &mut ReplEditorDraft) -> bool {
    if draft.cursor == 0 {
        return false;
    }
    let prev = previous_char_boundary(&draft.buffer, draft.cursor);
    draft.buffer.replace_range(prev..draft.cursor, "");
    draft.cursor = prev;
    true
}

fn delete_char_at_cursor(draft: &mut ReplEditorDraft) -> bool {
    if draft.cursor >= draft.buffer.len() {
        return false;
    }
    let next = next_char_boundary(&draft.buffer, draft.cursor);
    draft.buffer.replace_range(draft.cursor..next, "");
    true
}

fn move_cursor_left(draft: &mut ReplEditorDraft) {
    if draft.cursor > 0 {
        draft.cursor = previous_char_boundary(&draft.buffer, draft.cursor);
    }
}

fn move_cursor_right(draft: &mut ReplEditorDraft) {
    if draft.cursor < draft.buffer.len() {
        draft.cursor = next_char_boundary(&draft.buffer, draft.cursor);
    }
}

fn move_cursor_line_start(draft: &mut ReplEditorDraft) {
    draft.cursor = line_start_boundary(&draft.buffer, draft.cursor);
}

fn move_cursor_line_end(draft: &mut ReplEditorDraft) {
    draft.cursor = line_end_boundary(&draft.buffer, draft.cursor);
}

fn previous_char_boundary(text: &str, index: usize) -> usize {
    text[..index]
        .char_indices()
        .last()
        .map(|(offset, _)| offset)
        .unwrap_or(0)
}

fn next_char_boundary(text: &str, index: usize) -> usize {
    text[index..]
        .chars()
        .next()
        .map(|ch| index + ch.len_utf8())
        .unwrap_or(index)
}

fn line_start_boundary(text: &str, index: usize) -> usize {
    text[..index]
        .rfind('\n')
        .map(|offset| offset + 1)
        .unwrap_or(0)
}

fn line_end_boundary(text: &str, index: usize) -> usize {
    text[index..]
        .find('\n')
        .map(|offset| index + offset)
        .unwrap_or(text.len())
}

fn truncate_plain_to_width(text: &str, width: usize) -> String {
    let mut rendered = String::new();
    let mut current_width = 0usize;
    for ch in text.chars() {
        let ch_width = display_width_char(ch);
        if current_width + ch_width > width {
            break;
        }
        rendered.push(ch);
        current_width += ch_width;
    }
    rendered
}

fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn display_width_char(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

fn handle_repl_directive(
    input: ReplInput,
    state: &mut ReplState,
    _stdout: &mut io::Stdout,
    cli: &Cli,
    paths: &AppPaths,
    config: &AppConfig,
    session_id: &mut String,
    first_turn: &mut bool,
    temp: bool,
) -> AppResult<ReplDirective> {
    let trimmed = input.prompt.trim();
    if trimmed.is_empty() && input.images.is_empty() {
        return Ok(ReplDirective::Continue);
    }
    if !input.images.is_empty() {
        return Ok(ReplDirective::Submit(input));
    }

    if let Some((command, rest)) = parse_repl_slash_command(trimmed) {
        match command {
            ReplSlashCommand::Exit | ReplSlashCommand::Quit => {
                return Ok(ReplDirective::Exit);
            }
            ReplSlashCommand::Clear => {
                handle_repl_clear_command(paths, config, session_id, first_turn, temp)?;
                return Ok(ReplDirective::Continue);
            }
            ReplSlashCommand::Model => {
                handle_repl_model_command(cli, config, state, rest)?;
                return Ok(ReplDirective::Continue);
            }
            ReplSlashCommand::Session => {
                handle_repl_session_command(paths, config, session_id, first_turn, temp, rest)?;
                return Ok(ReplDirective::Continue);
            }
            ReplSlashCommand::Audit => {
                handle_repl_audit_command(paths, rest)?;
                return Ok(ReplDirective::Continue);
            }
            ReplSlashCommand::ToolSearch => {
                handle_repl_tool_search_command(paths, rest)?;
                return Ok(ReplDirective::Continue);
            }
            ReplSlashCommand::Bash => {
                handle_repl_bash_command(rest)?;
                return Ok(ReplDirective::Continue);
            }
            ReplSlashCommand::Status => {
                set_repl_transient_panel(
                    state,
                    build_repl_status_panel(cli, config, session_id, state),
                );
                return Ok(ReplDirective::Continue);
            }
            ReplSlashCommand::New => {
                handle_repl_new_session(paths, config, session_id, first_turn, temp, rest)?;
                return Ok(ReplDirective::Continue);
            }
        }
    }
    Ok(ReplDirective::Submit(input))
}

fn handle_repl_bash_command(rest: &str) -> AppResult<()> {
    match rest {
        "" | "help" => {
            println!("/bash sessions");
            println!("/bash read <session_id>");
            println!("/bash input <session_id> <text>");
            println!("/bash close <session_id>");
        }
        "sessions" => {
            let sessions = list_bash_sessions();
            if sessions.is_empty() {
                println!("no interactive bash sessions");
            } else {
                for session in sessions {
                    println!("{} {}", session.session_id, session.command);
                }
            }
        }
        _ if rest.starts_with("read ") => {
            let session_id = rest["read ".len()..].trim();
            println!("{}", continue_bash_session(session_id, None, false)?);
        }
        _ if rest.starts_with("close ") => {
            let session_id = rest["close ".len()..].trim();
            println!("{}", continue_bash_session(session_id, None, true)?);
        }
        _ if rest.starts_with("input ") => {
            let args = rest["input ".len()..].trim();
            let (session_id, text) = args.split_once(' ').ok_or_else(|| {
                AppError::new(EXIT_ARGS, "usage: /bash input <session_id> <text>")
            })?;
            println!("{}", continue_bash_session(session_id, Some(text), false)?);
        }
        _ => println!("unknown /bash command; use /bash help"),
    }
    Ok(())
}

fn parse_repl_slash_command(input: &str) -> Option<(ReplSlashCommand, &str)> {
    let stripped = input.strip_prefix('/')?;
    let (name, rest) = match stripped.find(char::is_whitespace) {
        Some(index) => (&stripped[..index], stripped[index..].trim()),
        None => (stripped, ""),
    };
    let command = match name {
        "" | "commands" => return None,
        "model" => ReplSlashCommand::Model,
        "sessions" | "session" => ReplSlashCommand::Session,
        "audit" => ReplSlashCommand::Audit,
        "tool-search" | "toolsearch" => ReplSlashCommand::ToolSearch,
        "status" => ReplSlashCommand::Status,
        "new" => ReplSlashCommand::New,
        "clear" => ReplSlashCommand::Clear,
        "bash" => ReplSlashCommand::Bash,
        "quit" => ReplSlashCommand::Quit,
        "exit" => ReplSlashCommand::Exit,
        _ => return None,
    };
    Some((command, rest))
}

fn visible_repl_slash_commands() -> Vec<ReplSlashCommand> {
    REPL_SLASH_COMMANDS.to_vec()
}

fn filtered_repl_slash_commands(query: &str) -> Vec<ReplSlashCommand> {
    let query = query.trim().to_ascii_lowercase();
    visible_repl_slash_commands()
        .into_iter()
        .filter(|command| {
            if query.is_empty() {
                return true;
            }
            let name = command.command().to_ascii_lowercase();
            name.starts_with(&query) || name.contains(&query)
        })
        .collect()
}

fn handle_repl_clear_command(
    paths: &AppPaths,
    config: &AppConfig,
    session_id: &mut String,
    first_turn: &mut bool,
    temp: bool,
) -> AppResult<()> {
    let current = session_id.clone();
    let next = if temp {
        generate_temp_session_id()
    } else {
        generate_session_id()
    };
    set_current_session(paths, config, Some(&next), temp)?;
    delete_session(paths, config, &current)?;
    *session_id = next.clone();
    *first_turn = true;
    println!(
        "cleared session {}; new session {}",
        short_id(&current),
        short_id(&next)
    );
    Ok(())
}

fn handle_repl_session_command(
    paths: &AppPaths,
    config: &AppConfig,
    session_id: &mut String,
    first_turn: &mut bool,
    temp: bool,
    rest: &str,
) -> AppResult<()> {
    match rest {
        "" | "list" => {
            let current = load_state(paths)?.current_session;
            for summary in list_session_summaries(paths, config, current.as_deref())? {
                println!("{}", format_session_list_entry(&summary));
            }
            Ok(())
        }
        "current" => {
            println!("{}", short_id(session_id));
            Ok(())
        }
        _ if rest.starts_with("switch ") => {
            let id = rest["switch ".len()..].trim();
            let resolved = resolve_session_id(paths, config, id)?;
            let session_temp = is_temp_session(&resolved) || temp;
            set_current_session(paths, config, Some(&resolved), session_temp)?;
            *session_id = resolved.clone();
            *first_turn = true;
            println!("switched to {}", short_id(&resolved));
            Ok(())
        }
        _ => {
            println!("usage: {}", ReplSlashCommand::Session.usage());
            Ok(())
        }
    }
}

fn handle_repl_audit_command(paths: &AppPaths, rest: &str) -> AppResult<()> {
    let mut config = load_config(paths)?;
    apply_runtime_config_defaults(paths, &mut config);
    match rest {
        "" | "status" => {
            println!(
                "audit: {}",
                if config.audit.enabled.unwrap_or(false) {
                    "on"
                } else {
                    "off"
                }
            );
        }
        "on" => {
            config.audit.enabled = Some(true);
            save_config(paths, &config)?;
            println!("audit enabled");
        }
        "off" => {
            config.audit.enabled = Some(false);
            save_config(paths, &config)?;
            println!("audit disabled");
        }
        _ => println!("usage: {}", ReplSlashCommand::Audit.usage()),
    }
    Ok(())
}

fn handle_repl_tool_search_command(paths: &AppPaths, rest: &str) -> AppResult<()> {
    let mut config = load_config(paths)?;
    apply_runtime_config_defaults(paths, &mut config);
    match rest {
        "" | "status" => {
            println!(
                "tool-search: {}",
                if config.tools.progressive_loading.unwrap_or(false) {
                    "on"
                } else {
                    "off"
                }
            );
        }
        "on" => {
            config.tools.progressive_loading = Some(true);
            save_config(paths, &config)?;
            println!("tool-search enabled");
        }
        "off" => {
            config.tools.progressive_loading = Some(false);
            save_config(paths, &config)?;
            println!("tool-search disabled");
        }
        _ => println!("usage: {}", ReplSlashCommand::ToolSearch.usage()),
    }
    Ok(())
}

fn handle_repl_model_command(
    cli: &Cli,
    config: &AppConfig,
    state: &mut ReplState,
    rest: &str,
) -> AppResult<()> {
    match rest {
        "" => {
            print_repl_model_choices(cli, config, state, None);
            Ok(())
        }
        "reset" => {
            state.model_override = None;
            println!(
                "model reset to {}",
                repl_effective_model_id(cli, config, state).unwrap_or_else(|| "-".to_string())
            );
            Ok(())
        }
        "list" => {
            print_repl_model_choices(cli, config, state, None);
            Ok(())
        }
        _ if rest.starts_with("list ") => {
            let provider_id = rest["list ".len()..].trim();
            print_repl_model_choices(cli, config, state, Some(provider_id));
            Ok(())
        }
        target => {
            let (provider_id, model_id) = resolve_model_use_target(config, target)?;
            state.provider_override = Some(provider_id.clone());
            state.model_override = Some(model_id.clone());
            println!("model set to {model_id} provider={provider_id}");
            Ok(())
        }
    }
}

fn print_repl_model_choices(
    cli: &Cli,
    config: &AppConfig,
    state: &ReplState,
    provider_filter: Option<&str>,
) {
    let current_provider = provider_filter
        .map(|value| value.to_string())
        .or_else(|| repl_effective_provider_id(cli, config, state));
    let current_model = repl_effective_model_id(cli, config, state);
    println!(
        "model {}",
        current_model.clone().unwrap_or_else(|| "-".to_string())
    );
    if let Some(provider_id) = &current_provider {
        println!("provider {provider_id}");
    }
    let mut entries = config
        .models
        .iter()
        .filter(|(_, model)| {
            current_provider
                .as_deref()
                .is_none_or(|provider_id| model.provider == provider_id)
        })
        .map(|(id, model)| (id.clone(), format_model_list_entry(model)))
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));
    if entries.is_empty() {
        println!("no models available");
        return;
    }
    for (id, label) in entries {
        let marker = if current_model.as_deref() == Some(id.as_str()) {
            "*"
        } else {
            " "
        };
        println!("{marker} {label} [id={id}]");
    }
}

fn repl_effective_provider_id(cli: &Cli, config: &AppConfig, state: &ReplState) -> Option<String> {
    state
        .provider_override
        .clone()
        .or_else(|| cli.provider.clone())
        .or_else(|| config.defaults.provider.clone())
}

fn repl_effective_model_id(cli: &Cli, config: &AppConfig, state: &ReplState) -> Option<String> {
    let provider_id = repl_effective_provider_id(cli, config, state);
    let selected = state
        .model_override
        .clone()
        .or_else(|| cli.model.clone())
        .or_else(|| config.defaults.model.clone());
    if let Some(model_id) = selected {
        if provider_id.as_deref().is_none_or(|provider_id| {
            config
                .models
                .get(&model_id)
                .is_some_and(|model| model.provider == provider_id)
        }) {
            return Some(model_id);
        }
    }
    provider_id.and_then(|provider_id| {
        config
            .providers
            .get(&provider_id)
            .and_then(|provider| provider.default_model.clone())
    })
}

fn repl_runtime_cli(cli: &Cli, config: &AppConfig, state: &ReplState) -> Cli {
    let mut runtime = cli.clone();
    runtime.provider = repl_effective_provider_id(cli, config, state);
    runtime.model = repl_effective_model_id(cli, config, state);
    runtime
}

fn handle_repl_new_session(
    paths: &AppPaths,
    config: &AppConfig,
    session_id: &mut String,
    first_turn: &mut bool,
    temp: bool,
    args: &str,
) -> AppResult<()> {
    let new_temp = match args {
        "" => false,
        "temp" => true,
        _ => {
            println!("usage: {}", ReplSlashCommand::New.usage());
            return Ok(());
        }
    };
    let next = if new_temp {
        generate_temp_session_id()
    } else if temp {
        generate_temp_session_id()
    } else {
        generate_session_id()
    };
    set_current_session(paths, config, Some(&next), new_temp || temp)?;
    *session_id = next.clone();
    *first_turn = true;
    println!("new session {}", short_id(&next));
    Ok(())
}

fn read_single_repl_line(
    stdin: &io::Stdin,
    stdout: &mut io::Stdout,
    prompt: &str,
) -> AppResult<String> {
    write!(stdout, "{prompt}")
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to write prompt: {err}")))?;
    stdout
        .flush()
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to flush stdout: {err}")))?;
    let mut line = String::new();
    stdin
        .read_line(&mut line)
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to read stdin: {err}")))?;
    Ok(line)
}

struct AskExecution {
    format: OutputFormat,
    output: AskOutput,
}

#[derive(Debug, Clone)]
struct AuditReport {
    provider: String,
    model: String,
    verdict: String,
    message: String,
    latency_ms: u64,
    usage: crate::session::Usage,
}

#[derive(Debug, Deserialize)]
struct AuditAgentItem {
    id: String,
    verdict: Option<String>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AuditAgentResponse {
    #[serde(default)]
    results: Vec<AuditAgentItem>,
}

#[derive(Debug, Clone)]
struct BatchAuditDecision {
    reports: BTreeMap<String, AuditReport>,
}

#[derive(Clone)]
struct ResolvedChatTarget {
    provider_id: String,
    provider: ProviderConfig,
    model_id: String,
    model: ModelConfig,
    api_key: String,
}

#[derive(Clone)]
struct BuiltInput {
    prompt: String,
    images: Vec<MessageImage>,
}

#[derive(Clone)]
struct PreparedAsk {
    format: OutputFormat,
    persisted_user_content: String,
    user_images: Vec<MessageImage>,
    session_id: String,
    session_preamble: Vec<ChatMessage>,
    request: ChatRequest,
    context_status_mode: ContextStatusMode,
    mcp_warmup: Option<McpWarmupHandle>,
}

async fn execute_ask(
    cli: &Cli,
    paths: &AppPaths,
    config: &AppConfig,
    secrets: &SecretsConfig,
    args: &AskArgs,
    output_override: Option<OutputFormat>,
) -> AppResult<AskExecution> {
    let prepared = prepare_ask(cli, paths, config, secrets, args, output_override)?;
    let response = send_chat(prepared.request.clone()).await?;
    persist_session(
        paths,
        config,
        args,
        &prepared.session_preamble,
        &prepared.persisted_user_content,
        &prepared.user_images,
        &prepared.session_id,
        &response,
    )?;

    Ok(AskExecution {
        format: prepared.format,
        output: AskOutput {
            ok: true,
            provider: response.provider_id,
            model: response.model_id,
            session_id: prepared.session_id,
            message: AssistantMessage {
                role: "assistant".to_string(),
                content: response.content,
            },
            usage: response.usage,
            finish_reason: response.finish_reason,
            latency_ms: response.latency_ms,
            raw_provider_response: if args.raw_provider_response {
                Some(response.raw)
            } else {
                None
            },
        },
    })
}

async fn execute_ask_with_tools(
    cli: &Cli,
    paths: &AppPaths,
    config: &AppConfig,
    secrets: &SecretsConfig,
    args: &AskArgs,
    output_override: Option<OutputFormat>,
    show_static_status_bar: bool,
) -> AppResult<AskExecution> {
    let mut prepared = prepare_ask(cli, paths, config, secrets, args, output_override)?;
    let mut loaded_tool_names = BTreeSet::new();
    prepared.request.tools = initial_tool_definitions(config);
    let max_rounds = config.tools.max_rounds.unwrap_or(20) as usize;
    let turn_start_index = prepared.request.messages.len().saturating_sub(1);
    let response_result: AppResult<ChatResponse> = async {
        let mut final_response: Option<ChatResponse> = None;

        for round in 0..max_rounds {
            prepared.request.tools = tool_definitions_for_names(
                config,
                &loaded_tool_names.iter().cloned().collect::<Vec<_>>(),
            );
            let use_stream = should_stream_tool_round(&prepared.request, args.stream, round);
            let mut stdout = io::stdout();
            let collapse = config.defaults.collapse_thinking.unwrap_or(false);
            let mut renderer = StreamRenderer::new(collapse);
            let mut status = None;

            // Status bar on first round
            if round == 0 && use_stream && show_static_status_bar {
                print_status_bar(
                    &prepared.request.provider_id,
                    &prepared.request.model_id,
                    &prepared.session_id,
                )
                .map_err(|err| {
                    AppError::new(EXIT_ARGS, format!("failed to write stdout: {err}"))
                })?;
                status = Some(StreamStatus::start(StreamPhase::Waiting));
            } else if round == 0 && use_stream {
                status = Some(StreamStatus::start(StreamPhase::Waiting));
            }

            let response = if use_stream {
                stream_chat(prepared.request.clone(), |chunk| {
                    if !chunk.delta.is_empty() {
                        let rendered = renderer.push(&chunk.delta);
                        for phase in renderer.drain_phase_transitions() {
                            update_stream_status(&status, phase)?;
                        }
                        if !rendered.is_empty() {
                            write_stream_output(&mut stdout, &status, &rendered)?;
                        }
                    }
                    Ok(())
                })
                .await?
            } else {
                send_chat(prepared.request.clone()).await?
            };

            if response.tool_calls.is_empty() {
                if use_stream {
                    let remaining = renderer.flush();
                    for phase in renderer.drain_phase_transitions() {
                        update_stream_status(&status, phase)?;
                    }
                    write_stream_output(&mut stdout, &status, &remaining)?;
                    if !remaining.is_empty() && !remaining.ends_with('\n') {
                        write_stream_output(&mut stdout, &status, "\n")?;
                    }
                    stop_stream_status(&mut status)?;
                } else if args.stream {
                    let rendered = render_markdown(&response.content, collapse);
                    if !rendered.is_empty() {
                        writeln!(stdout, "{rendered}").map_err(|err| {
                            AppError::new(EXIT_ARGS, format!("failed to write stdout: {err}"))
                        })?;
                    }
                }
                final_response = Some(response);
                break;
            }

            // Model wants to call tools — flush renderer and newline
            if use_stream {
                let remaining = renderer.flush();
                for phase in renderer.drain_phase_transitions() {
                    update_stream_status(&status, phase)?;
                }
                write_stream_output(&mut stdout, &status, &remaining)?;
                if !remaining.is_empty() && !remaining.ends_with('\n') {
                    write_stream_output(&mut stdout, &status, "\n")?;
                }
                stop_stream_status(&mut status)?;
            } else if args.stream && !response.content.is_empty() {
                let rendered = render_markdown(&response.content, collapse);
                if !rendered.is_empty() {
                    writeln!(stdout, "{rendered}").map_err(|err| {
                        AppError::new(EXIT_ARGS, format!("failed to write stdout: {err}"))
                    })?;
                }
            }

            let tool_summary_width = terminal::size()
                .map(|(cols, _)| cols.max(1) as usize)
                .unwrap_or(80);
            eprintln!(
                "{}",
                render_tool_call_summary(round + 1, &response.tool_calls, tool_summary_width)
            );

            prepared.request.messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: response.content.clone(),
                images: Vec::new(),
                tool_calls: Some(response.tool_calls.clone()),
                tool_call_id: None,
                name: None,
            });

            let dangerous_calls = response
                .tool_calls
                .iter()
                .filter_map(|raw_call| parse_tool_call(raw_call).ok())
                .filter(|call| tool_requires_agent_review(config, call))
                .collect::<Vec<_>>();
            let audit_batch = if dangerous_calls.is_empty() {
                None
            } else {
                let audit_model_id = config
                    .audit
                    .model
                    .as_deref()
                    .unwrap_or(&prepared.request.model_id)
                    .to_string();
                print_audit_start(dangerous_calls.len(), &audit_model_id);
                let batch = maybe_review_tool_call(
                    config,
                    secrets,
                    &prepared.request,
                    &prepared.session_id,
                    &prepared.request.messages[turn_start_index..],
                    &dangerous_calls,
                )
                .await;
                if let Some(batch) = &batch {
                    for call in &dangerous_calls {
                        if let Some(report) = batch.reports.get(&call.id) {
                            append_audit_event(
                                paths,
                                config,
                                args,
                                &prepared.session_id,
                                Some(&call.name),
                                Some(&call.id),
                                report,
                            )?;
                            if report.verdict == "pass" {
                                print_audit_pass(&call.name, &call.id, report);
                            } else {
                                print_audit_warning(&call.name, &call.id, report);
                            }
                        }
                    }
                }
                batch
            };

            for raw_call in &response.tool_calls {
                if config.tools.progressive_loading.unwrap_or(false) {
                    for tool_name in discovered_tool_names_from_search(config, raw_call) {
                        loaded_tool_names.insert(tool_name);
                    }
                }
                if let Some(call) = parse_tool_call(raw_call)
                    .ok()
                    .filter(|call| call.name.starts_with("mcp__"))
                {
                    if !has_cached_mcp_tool(config, &call.name)
                        && let Some(warmup) = &prepared.mcp_warmup
                    {
                        match wait_for_mcp_warmup(warmup, crate::mcp::MCP_WARMUP_WAIT_SECS) {
                            Some(Ok(_)) => {}
                            Some(Err(err)) => eprintln!("warning: {}", err.message),
                            None => {
                                let warning = warmup_timeout_warning(warmup);
                                eprintln!("warning: {}", warning.message);
                            }
                        }
                    }
                }
                let auto_confirm = parse_tool_call(raw_call)
                    .ok()
                    .and_then(|call| {
                        audit_batch
                            .as_ref()
                            .and_then(|batch| batch.reports.get(&call.id))
                            .map(|report| report.verdict == "pass")
                    })
                    .unwrap_or(false)
                    || args.yes;
                let tool_message = execute_tool_as_message_with_context(
                    raw_call,
                    auto_confirm,
                    paths,
                    config,
                    &prepared.request.messages,
                );
                if should_print_tool_result_ui(
                    prepared.format.clone(),
                    show_static_status_bar,
                    &tool_message,
                ) {
                    print_tool_result_ui(&mut stdout, config, &tool_message)?;
                }
                prepared.request.messages.push(tool_message);
            }
        }

        final_response.ok_or_else(|| {
            AppError::new(
                EXIT_ARGS,
                format!("max tool calling rounds ({max_rounds}) exceeded"),
            )
        })
    }
    .await;

    let response = match response_result {
        Ok(response) => response,
        Err(err) => {
            let persisted_turn_messages = persisted_turn_messages_for_session(
                &prepared,
                &prepared.request.messages[turn_start_index..],
            );
            persist_partial_tool_turn(
                paths,
                config,
                args,
                &prepared.session_id,
                &prepared.session_preamble,
                &persisted_turn_messages,
            )?;
            return Err(err);
        }
    };

    let persisted_turn_messages = persisted_turn_messages_for_session(
        &prepared,
        &prepared.request.messages[turn_start_index..],
    );
    persist_tool_session(
        paths,
        config,
        args,
        &prepared.session_preamble,
        &prepared.session_id,
        &persisted_turn_messages,
        &response,
    )?;

    Ok(AskExecution {
        format: prepared.format,
        output: AskOutput {
            ok: true,
            provider: response.provider_id,
            model: response.model_id,
            session_id: prepared.session_id,
            message: AssistantMessage {
                role: "assistant".to_string(),
                content: response.content,
            },
            usage: response.usage,
            finish_reason: response.finish_reason,
            latency_ms: response.latency_ms,
            raw_provider_response: if args.raw_provider_response {
                Some(response.raw)
            } else {
                None
            },
        },
    })
}

async fn execute_ask_stream(
    cli: &Cli,
    paths: &AppPaths,
    config: &AppConfig,
    secrets: &SecretsConfig,
    args: &AskArgs,
    output_override: Option<OutputFormat>,
    show_static_status_bar: bool,
) -> AppResult<()> {
    let prepared = prepare_ask(cli, paths, config, secrets, args, output_override)?;
    let format = prepared.format.clone();
    let session_id = prepared.session_id.clone();
    let mut stdout = io::stdout();

    if format == OutputFormat::Ndjson {
        write_stream_json(
            &mut stdout,
            &json!({
                "type": "response.started",
                "session_id": session_id,
                "provider": prepared.request.provider_id,
                "model": prepared.request.model_id,
            }),
        )?;
    }

    // Status bar
    if format == OutputFormat::Text && show_static_status_bar {
        print_status_bar(
            &prepared.request.provider_id,
            &prepared.request.model_id,
            &session_id,
        )
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to write stdout: {err}")))?;
    }

    let collapse = config.defaults.collapse_thinking.unwrap_or(false);
    let mut renderer = StreamRenderer::new(collapse);
    let mut status = if format == OutputFormat::Text {
        Some(StreamStatus::start(StreamPhase::Waiting))
    } else {
        None
    };

    let response = match stream_chat(prepared.request.clone(), |chunk| {
        match format {
            OutputFormat::Text => {
                if !chunk.delta.is_empty() {
                    let rendered = renderer.push(&chunk.delta);
                    for phase in renderer.drain_phase_transitions() {
                        update_stream_status(&status, phase)?;
                    }
                    if !rendered.is_empty() {
                        write_stream_output(&mut stdout, &status, &rendered)?;
                    }
                }
            }
            OutputFormat::Ndjson => {
                if !chunk.delta.is_empty() {
                    write_stream_json(
                        &mut stdout,
                        &json!({
                            "type": "response.delta",
                            "delta": chunk.delta,
                        }),
                    )?;
                }
            }
            _ => {}
        }
        Ok(())
    })
    .await
    {
        Ok(response) => response,
        Err(err) => {
            if format == OutputFormat::Ndjson {
                write_stream_json(
                    &mut stdout,
                    &json!({
                        "type": "response.error",
                        "message": err.message,
                    }),
                )?;
            }
            return Err(err);
        }
    };

    match format {
        OutputFormat::Text => {
            let remaining = renderer.flush();
            for phase in renderer.drain_phase_transitions() {
                update_stream_status(&status, phase)?;
            }
            write_stream_output(&mut stdout, &status, &remaining)?;
            if !remaining.is_empty() && !remaining.ends_with('\n') {
                write_stream_output(&mut stdout, &status, "\n")?;
            }
            stop_stream_status(&mut status)?;
        }
        OutputFormat::Ndjson => {
            write_stream_json(
                &mut stdout,
                &json!({
                    "type": "response.completed",
                    "session_id": prepared.session_id,
                    "provider": response.provider_id,
                    "model": response.model_id,
                    "finish_reason": response.finish_reason,
                    "usage": response.usage,
                    "latency_ms": response.latency_ms,
                }),
            )?;
        }
        _ => {}
    }

    persist_session(
        paths,
        config,
        args,
        &prepared.session_preamble,
        &prepared.persisted_user_content,
        &prepared.user_images,
        &session_id,
        &response,
    )?;
    Ok(())
}

fn prepare_ask(
    cli: &Cli,
    paths: &AppPaths,
    config: &AppConfig,
    secrets: &SecretsConfig,
    args: &AskArgs,
    output_override: Option<OutputFormat>,
) -> AppResult<PreparedAsk> {
    if args.stream && args.raw_provider_response {
        return Err(AppError::new(
            EXIT_ARGS,
            "--raw-provider-response is not supported with --stream",
        ));
    }
    if args.new_session && args.session.is_some() {
        return Err(AppError::new(
            EXIT_ARGS,
            "--new-session cannot be used together with --session",
        ));
    }

    let input = build_input(args)?;
    let provider_id = cli
        .provider
        .clone()
        .or_else(|| config.defaults.provider.clone())
        .ok_or_else(|| AppError::new(EXIT_PROVIDER, "no provider selected"))?;
    let provider = config.providers.get(&provider_id).ok_or_else(|| {
        AppError::new(
            EXIT_PROVIDER,
            format!("provider `{provider_id}` does not exist"),
        )
    })?;
    let model_id = cli
        .model
        .clone()
        .or_else(|| config.defaults.model.clone())
        .or_else(|| provider.default_model.clone())
        .ok_or_else(|| AppError::new(EXIT_MODEL, "no model selected"))?;
    let model = config
        .models
        .get(&model_id)
        .ok_or_else(|| AppError::new(EXIT_MODEL, format!("model `{model_id}` does not exist")))?;
    if model.provider != provider_id {
        return Err(AppError::new(
            EXIT_MODEL,
            format!(
                "model `{model_id}` belongs to provider `{}`, not `{provider_id}`",
                model.provider
            ),
        ));
    }
    let mut format = output_override
        .or(cli.output.clone())
        .or_else(|| config.defaults.output.clone())
        .unwrap_or(OutputFormat::Line);
    if args.stream {
        format = match format {
            OutputFormat::Line => OutputFormat::Text,
            OutputFormat::Text => OutputFormat::Text,
            OutputFormat::Ndjson => OutputFormat::Ndjson,
            OutputFormat::Json => {
                return Err(AppError::new(
                    EXIT_ARGS,
                    "--stream only supports --output text or --output ndjson",
                ));
            }
        };
    } else if format == OutputFormat::Ndjson {
        return Err(AppError::new(
            EXIT_ARGS,
            "--output ndjson currently requires --stream",
        ));
    }

    let session_id = select_session_id(
        paths,
        config,
        args.session.as_deref(),
        args.new_session,
        args.temp,
        args.ephemeral,
    )?;
    let mut messages = Vec::new();
    let mut session_preamble = Vec::new();
    if let Ok(history) = read_events(paths, config, &session_id) {
        for event in history {
            if let SessionEvent::Message(message) = event {
                let has_payload = !message.content.is_empty()
                    || !message.images.is_empty()
                    || !message.tool_calls.is_empty()
                    || message.tool_call_id.is_some();
                if !has_payload {
                    continue;
                }
                messages.push(ChatMessage {
                    role: message.role,
                    content: message.content,
                    images: message.images,
                    tool_calls: (!message.tool_calls.is_empty()).then_some(message.tool_calls),
                    tool_call_id: message.tool_call_id,
                    name: message.name,
                });
            }
        }
    }
    let is_new_session = messages.is_empty();
    let _ = ensure_mcp_daemon_started(paths, config);
    let _ = hydrate_cached_mcp_tools(paths, config);
    let mcp_live_tools = list_mcp_tools_from_ready_daemon(paths, config).unwrap_or_default();
    if !mcp_live_tools.is_empty() {
        crate::mcp::merge_cached_mcp_tools(config, mcp_live_tools);
    }
    let mcp_warmup = start_mcp_warmup(paths, config);
    if is_new_session {
        // Build system prompt from CLI args and config file
        let cli_system = read_system_prompt(&args.system)?;
        let file_system = config
            .defaults
            .system_prompt_file
            .as_ref()
            .and_then(|path| {
                let expanded = crate::config::expand_tilde(path);
                std::fs::read_to_string(&expanded).ok()
            });
        let mode = config
            .defaults
            .system_prompt_mode
            .as_deref()
            .unwrap_or("append");

        let final_system = match (cli_system, file_system, mode) {
            (Some(_cli), Some(file), "override") => Some(file),
            (Some(cli), Some(file), _) => Some(format!("{cli}\n\n{file}")),
            (Some(cli), None, _) => Some(cli),
            (None, Some(file), _) => Some(file),
            (None, None, _) => None,
        };

        if let Some(system) = final_system {
            let system_message = ChatMessage {
                role: "system".to_string(),
                content: system,
                images: Vec::new(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            };
            session_preamble.push(system_message.clone());
            messages.push(system_message);
        }
    }
    let context_status_mode = crate::context::resolve_context_status_mode(
        args.context_status,
        config.defaults.context_status,
    );
    if is_new_session && context_status_mode == ContextStatusMode::SystemOnce {
        let status_message = ChatMessage {
            role: "system".to_string(),
            content: crate::context::collect_context_status(),
            images: Vec::new(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        };
        session_preamble.push(status_message.clone());
        messages.push(status_message);
    }
    let user_content = match context_status_mode {
        ContextStatusMode::Always | ContextStatusMode::Latest => {
            prepend_context_status(&input.prompt)
        }
        ContextStatusMode::Off | ContextStatusMode::SystemOnce => input.prompt.clone(),
    };
    let persisted_user_content = match context_status_mode {
        ContextStatusMode::Always => user_content.clone(),
        ContextStatusMode::Off | ContextStatusMode::Latest | ContextStatusMode::SystemOnce => {
            input.prompt.clone()
        }
    };
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: user_content,
        images: input.images.clone(),
        tool_calls: None,
        tool_call_id: None,
        name: None,
    });
    if messages.iter().any(|message| !message.images.is_empty()) {
        if !model_supports_capability(model, "vision") {
            return Err(AppError::new(
                EXIT_MODEL,
                format!(
                    "model `{model_id}` does not support image input; add capability `vision` or select a vision-capable model"
                ),
            ));
        }
        match provider.kind.as_str() {
            "openai_compatible" | "anthropic" => {}
            other => {
                return Err(AppError::new(
                    EXIT_PROVIDER,
                    format!("provider kind `{other}` does not support image input yet"),
                ));
            }
        }
    }

    let temperature = args.temperature.or(model.temperature);
    let max_output_tokens = args.max_output_tokens.or(model.max_output_tokens);
    let api_key = resolve_api_key(&provider_id, provider, secrets)?;
    let params = parse_params(&args.params)?;

    Ok(PreparedAsk {
        format,
        persisted_user_content,
        user_images: input.images,
        session_id,
        session_preamble,
        context_status_mode,
        mcp_warmup,
        request: ChatRequest {
            provider_id,
            provider: provider.clone(),
            model_id,
            model: model.clone(),
            api_key,
            messages,
            temperature,
            max_output_tokens,
            params,
            timeout_secs: args.timeout,
            tools: Vec::new(),
        },
    })
}

fn persisted_turn_messages_for_session(
    prepared: &PreparedAsk,
    turn_messages: &[ChatMessage],
) -> Vec<ChatMessage> {
    let mut persisted = turn_messages.to_vec();
    if prepared.context_status_mode == ContextStatusMode::Latest
        && let Some(first_message) = persisted.first_mut()
        && first_message.role == "user"
    {
        first_message.content = prepared.persisted_user_content.clone();
    }
    persisted
}

fn persist_session(
    paths: &AppPaths,
    config: &AppConfig,
    args: &AskArgs,
    session_preamble: &[ChatMessage],
    prompt: &str,
    user_images: &[MessageImage],
    session_id: &str,
    response: &ChatResponse,
) -> AppResult<()> {
    let auto_save = config.defaults.auto_save_session.unwrap_or(true);
    if args.ephemeral || !auto_save {
        return Ok(());
    }

    let mut events = Vec::new();
    append_session_message_events(&mut events, session_preamble);
    events.push(SessionEvent::Message(SessionMessage {
        role: "user".to_string(),
        content: prompt.to_string(),
        images: user_images.to_vec(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        name: None,
        created_at: now_rfc3339(),
    }));
    if !response.content.is_empty() {
        events.push(SessionEvent::Message(SessionMessage {
            role: "assistant".to_string(),
            content: response.content.clone(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
            created_at: now_rfc3339(),
        }));
    }
    events.push(SessionEvent::Response(SessionResponse {
        provider: response.provider_id.clone(),
        model: response.model_id.clone(),
        finish_reason: response.finish_reason.clone(),
        latency_ms: response.latency_ms,
        usage: response.usage.clone(),
        created_at: now_rfc3339(),
    }));
    append_events(paths, config, session_id, &events)?;
    let temp = is_temp_session(session_id) || args.temp;
    set_current_session(paths, config, Some(session_id), temp)
}

fn persist_tool_session(
    paths: &AppPaths,
    config: &AppConfig,
    args: &AskArgs,
    session_preamble: &[ChatMessage],
    session_id: &str,
    turn_messages: &[ChatMessage],
    response: &ChatResponse,
) -> AppResult<()> {
    let auto_save = config.defaults.auto_save_session.unwrap_or(true);
    if args.ephemeral || !auto_save {
        return Ok(());
    }

    let mut events = Vec::new();
    append_session_message_events(&mut events, session_preamble);
    append_session_message_events(&mut events, turn_messages);
    if !response.content.is_empty() {
        events.push(SessionEvent::Message(SessionMessage {
            role: "assistant".to_string(),
            content: response.content.clone(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
            created_at: now_rfc3339(),
        }));
    }
    events.push(SessionEvent::Response(SessionResponse {
        provider: response.provider_id.clone(),
        model: response.model_id.clone(),
        finish_reason: response.finish_reason.clone(),
        latency_ms: response.latency_ms,
        usage: response.usage.clone(),
        created_at: now_rfc3339(),
    }));
    append_events(paths, config, session_id, &events)?;
    let temp = is_temp_session(session_id) || args.temp;
    set_current_session(paths, config, Some(session_id), temp)
}

fn write_stream_json(stdout: &mut io::Stdout, value: &Value) -> AppResult<()> {
    let line = serde_json::to_string(value)
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to render JSON: {err}")))?;
    writeln!(stdout, "{line}")
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to write stdout: {err}")))?;
    stdout
        .flush()
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to flush stdout: {err}")))?;
    Ok(())
}

fn build_input(args: &AskArgs) -> AppResult<BuiltInput> {
    let mut parts = Vec::new();
    if let Some(prompt) = &args.prompt {
        parts.push(prompt.clone());
    }
    if args.stdin {
        let stdin = read_stdin_all()?;
        if !stdin.trim().is_empty() {
            parts.push(stdin);
        }
    }
    for attachment in &args.attachments {
        let content = fs::read_to_string(attachment).map_err(|err| {
            AppError::new(
                EXIT_ARGS,
                format!(
                    "failed to read attachment `{}`: {}",
                    attachment.display(),
                    err
                ),
            )
        })?;
        parts.push(format!("File: {}\n{}", attachment.display(), content));
    }
    let mut images = read_image_inputs(&args.images, args.clipboard_image)?;
    images.extend(args.preloaded_images.clone());
    if parts.is_empty() && images.is_empty() {
        return Err(AppError::new(
            EXIT_ARGS,
            "chat ask requires PROMPT, --stdin, --attach, --image, or --clipboard-image",
        ));
    }
    Ok(BuiltInput {
        prompt: parts.join("\n\n"),
        images,
    })
}

fn model_supports_capability(model: &ModelConfig, capability: &str) -> bool {
    model.capabilities.iter().any(|item| item == capability)
}

fn resolve_api_key(
    provider_id: &str,
    provider: &ProviderConfig,
    secrets: &SecretsConfig,
) -> AppResult<String> {
    if let Some(env_name) = &provider.api_key_env {
        if let Ok(value) = std::env::var(env_name) {
            if !value.trim().is_empty() {
                return Ok(value);
            }
        }
    }
    if let Some(secret) = secrets
        .providers
        .get(provider_id)
        .and_then(|s| s.api_key.clone())
    {
        if !secret.trim().is_empty() {
            return Ok(secret);
        }
    }
    if provider_allows_missing_api_key(provider) {
        return Ok(String::new());
    }
    Err(AppError::new(
        EXIT_AUTH,
        format!(
            "missing API key for provider `{provider_id}`; use `chat config auth set {provider_id} --value ...` or configure provider.api_key_env"
        ),
    ))
}

fn provider_allows_missing_api_key(provider: &ProviderConfig) -> bool {
    if provider.kind == "ollama" {
        return true;
    }
    if provider.kind != "openai_compatible" {
        return false;
    }
    let Some(base_url) = &provider.base_url else {
        return false;
    };
    let normalized = base_url.to_ascii_lowercase();
    normalized.starts_with("http://localhost")
        || normalized.starts_with("http://127.0.0.1")
        || normalized.starts_with("http://0.0.0.0")
}

fn parse_params(items: &[String]) -> AppResult<BTreeMap<String, Value>> {
    let mut params = BTreeMap::new();
    for item in items {
        let (key, raw_value) = item.split_once('=').ok_or_else(|| {
            AppError::new(
                EXIT_ARGS,
                format!("invalid --param `{item}`, expected KEY=VALUE"),
            )
        })?;
        let value = serde_json::from_str(raw_value)
            .unwrap_or_else(|_| Value::String(raw_value.to_string()));
        params.insert(key.to_string(), value);
    }
    Ok(params)
}

fn resolve_model_use_target(config: &AppConfig, target: &str) -> AppResult<(String, String)> {
    if let Some((provider_id, model_name)) = target.split_once('/') {
        let provider = config.providers.get(provider_id).ok_or_else(|| {
            AppError::new(
                EXIT_PROVIDER,
                format!("provider `{provider_id}` does not exist"),
            )
        })?;
        let model_entry = config
            .models
            .iter()
            .find(|(_, model)| model.provider == provider_id && model.remote_name == model_name)
            .map(|(id, _)| id.clone())
            .or_else(|| {
                config
                    .models
                    .iter()
                    .find(|(id, model)| model.provider == provider_id && *id == model_name)
                    .map(|(id, _)| id.clone())
            })
            .or_else(|| {
                provider
                    .default_model
                    .as_ref()
                    .filter(|default_id| *default_id == model_name)
                    .cloned()
            })
            .ok_or_else(|| {
                AppError::new(
                    EXIT_MODEL,
                    format!("model `{model_name}` does not exist under provider `{provider_id}`"),
                )
            })?;
        return Ok((provider_id.to_string(), model_entry));
    }

    let model = config
        .models
        .get(target)
        .ok_or_else(|| AppError::new(EXIT_MODEL, format!("model `{target}` does not exist")))?;
    Ok((model.provider.clone(), target.to_string()))
}

fn format_model_list_entry(model: &ModelConfig) -> String {
    format!("{}/{}", model.provider, model.remote_name)
}

fn read_stdin_all() -> AppResult<String> {
    let mut buffer = String::new();
    io::stdin()
        .read_to_string(&mut buffer)
        .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to read stdin: {err}")))?;
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Usage;
    use std::fs;
    use std::path::PathBuf;

    fn test_paths() -> (AppPaths, PathBuf) {
        let base = std::env::temp_dir().join(format!("chat-cli-test-{}", ulid::Ulid::new()));
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

    fn test_cli() -> Cli {
        Cli {
            provider: Some("cpap".to_string()),
            model: Some("team-gpt-5-4".to_string()),
            mode: "auto".to_string(),
            output: None,
            config_dir: None,
            data_dir: None,
            no_color: false,
            verbose: false,
            quiet: false,
            command: Commands::Thinking,
        }
    }

    fn test_config() -> AppConfig {
        let mut config = AppConfig::default();
        config.providers.insert(
            "cpap".to_string(),
            ProviderConfig {
                kind: "openai_compatible".to_string(),
                default_model: Some("team-gpt-5-4".to_string()),
                ..ProviderConfig::default()
            },
        );
        config.providers.insert(
            "deepseek".to_string(),
            ProviderConfig {
                kind: "openai_compatible".to_string(),
                default_model: Some("deepseek-reasoner-search".to_string()),
                ..ProviderConfig::default()
            },
        );
        config.models.insert(
            "team-gpt-5-4".to_string(),
            ModelConfig {
                provider: "cpap".to_string(),
                remote_name: "team/gpt-5.4".to_string(),
                display_name: None,
                context_window: None,
                max_output_tokens: None,
                capabilities: vec!["chat".to_string(), "reasoning".to_string()],
                temperature: None,
                reasoning_effort: None,
                patches: ModelPatchConfig {
                    system_to_user: Some(true),
                },
            },
        );
        config.models.insert(
            "deepseek-reasoner-search".to_string(),
            ModelConfig {
                provider: "deepseek".to_string(),
                remote_name: "deepseek-reasoner-search".to_string(),
                display_name: None,
                context_window: None,
                max_output_tokens: None,
                capabilities: vec!["chat".to_string(), "reasoning".to_string()],
                temperature: None,
                reasoning_effort: None,
                patches: ModelPatchConfig::default(),
            },
        );
        config
    }

    fn ask_args(prompt: &str) -> AskArgs {
        AskArgs {
            prompt: Some(prompt.to_string()),
            stdin: false,
            system: None,
            attachments: Vec::new(),
            images: Vec::new(),
            clipboard_image: false,
            preloaded_images: Vec::new(),
            session: None,
            new_session: false,
            ephemeral: false,
            temp: false,
            tools: false,
            yes: false,
            stream: false,
            temperature: None,
            max_output_tokens: None,
            params: Vec::new(),
            timeout: None,
            raw_provider_response: false,
            context_status: Some(ContextStatusMode::Off),
        }
    }

    fn repl_test_context() -> (Cli, AppPaths, AppConfig, String, bool) {
        let (paths, _base_dir) = test_paths();
        let cli = test_cli();
        let config = test_config();
        let session_id = "sess_test_repl".to_string();
        (cli, paths, config, session_id, true)
    }

    #[test]
    fn prepare_ask_without_live_mcp_tools_keeps_running() {
        let (paths, _base) = test_paths();
        let cli = test_cli();
        let mut config = test_config();
        let mut secrets = SecretsConfig::default();
        secrets.providers.insert(
            "cpap".to_string(),
            ProviderSecret {
                api_key: Some("test-key".to_string()),
            },
        );
        config.mcp.insert(
            "ace".to_string(),
            crate::mcp::McpServerConfig {
                command: "missing-command".to_string(),
                ..crate::mcp::McpServerConfig::default()
            },
        );
        let prepared =
            prepare_ask(&cli, &paths, &config, &secrets, &ask_args("test"), None).unwrap();
        assert_eq!(prepared.request.messages.last().unwrap().role, "user");
    }

    #[test]
    fn prepare_ask_hydrates_matching_mcp_cache_into_runtime_cache() {
        let (paths, _base) = test_paths();
        let cli = test_cli();
        let mut config = test_config();
        let mut secrets = SecretsConfig::default();
        secrets.providers.insert(
            "cpap".to_string(),
            ProviderSecret {
                api_key: Some("test-key".to_string()),
            },
        );
        config.tools.mcp = Some(true);
        config.mcp.insert(
            "ace".to_string(),
            crate::mcp::McpServerConfig {
                command: "missing-command".to_string(),
                args: vec!["serve".to_string()],
                ..crate::mcp::McpServerConfig::default()
            },
        );
        crate::mcp::save_mcp_cache(
            &paths,
            &crate::mcp::McpCache {
                servers: BTreeMap::from([(
                    "ace".to_string(),
                    crate::mcp::McpServerCacheEntry {
                        server: "ace".to_string(),
                        command: "missing-command".to_string(),
                        args: vec!["serve".to_string()],
                        cwd: None,
                        enabled_tools: Vec::new(),
                        disabled_tools: Vec::new(),
                        tools: vec![crate::mcp::McpToolSpec {
                            full_name: "mcp__ace__calendar".to_string(),
                            server: "ace".to_string(),
                            remote_name: "calendar".to_string(),
                            description: "Calendar".to_string(),
                            input_schema: serde_json::json!({"type":"object"}),
                            read_only: true,
                        }],
                        checked_at_unix_ms: 1,
                    },
                )]),
            },
        )
        .unwrap();

        let _prepared =
            prepare_ask(&cli, &paths, &config, &secrets, &ask_args("test"), None).unwrap();

        assert!(crate::mcp::has_cached_mcp_tool(
            &config,
            "mcp__ace__calendar"
        ));
    }

    #[test]
    fn ensure_mcp_daemon_started_skips_when_mcp_disabled() {
        let (paths, _base) = test_paths();
        let config = test_config();
        ensure_mcp_daemon_started(&paths, &config).unwrap();
        assert!(!crate::mcp::current_mcp_daemon_status(&paths).running);
    }

    #[test]
    fn persisted_turn_messages_preserves_plain_messages() {
        let prepared = PreparedAsk {
            format: OutputFormat::Text,
            persisted_user_content: "prompt".to_string(),
            user_images: Vec::new(),
            session_id: "sess_1".to_string(),
            session_preamble: Vec::new(),
            request: ChatRequest {
                provider_id: "cpap".to_string(),
                provider: ProviderConfig::default(),
                model_id: "team-gpt-5-4".to_string(),
                model: ModelConfig::default(),
                api_key: String::new(),
                messages: Vec::new(),
                temperature: None,
                max_output_tokens: None,
                params: BTreeMap::new(),
                timeout_secs: None,
                tools: Vec::new(),
            },
            context_status_mode: ContextStatusMode::Off,
            mcp_warmup: None,
        };
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: "prompt".to_string(),
            images: Vec::new(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }];
        let persisted = persisted_turn_messages_for_session(&prepared, &messages);
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].role, "user");
    }

    #[test]
    fn repl_audit_command_updates_config() {
        let (paths, base_dir) = test_paths();
        handle_repl_audit_command(&paths, "on").unwrap();
        let config = load_config(&paths).unwrap();
        assert_eq!(config.audit.enabled, Some(true));
        let _ = fs::remove_dir_all(base_dir);
    }

    #[test]
    fn repl_tool_search_command_updates_config() {
        let (paths, base_dir) = test_paths();
        handle_repl_tool_search_command(&paths, "on").unwrap();
        let config = load_config(&paths).unwrap();
        assert_eq!(config.tools.progressive_loading, Some(true));
        let _ = fs::remove_dir_all(base_dir);
    }

    #[test]
    fn repl_session_command_switches_session() {
        let (paths, base_dir) = test_paths();
        let config = test_config();
        append_events(
            &paths,
            &config,
            "sess_alpha",
            &[SessionEvent::Message(SessionMessage {
                role: "user".to_string(),
                content: "hello".to_string(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                name: None,
                created_at: now_rfc3339(),
            })],
        )
        .unwrap();
        let mut session_id = "sess_test_repl".to_string();
        let mut first_turn = false;
        handle_repl_session_command(
            &paths,
            &config,
            &mut session_id,
            &mut first_turn,
            false,
            "switch alpha",
        )
        .unwrap();
        assert_eq!(session_id, "sess_alpha");
        assert!(first_turn);
        let _ = fs::remove_dir_all(base_dir);
    }

    #[test]
    fn repl_model_command_updates_runtime_target() {
        let (cli, paths, config, mut session_id, mut first_turn) = repl_test_context();
        let mut state = ReplState::default();
        let mut stdout = io::stdout();
        let directive = handle_repl_directive(
            ReplInput {
                prompt: "/model deepseek/deepseek-reasoner-search".to_string(),
                images: Vec::new(),
            },
            &mut state,
            &mut stdout,
            &cli,
            &paths,
            &config,
            &mut session_id,
            &mut first_turn,
            false,
        )
        .unwrap();
        assert!(matches!(directive, ReplDirective::Continue));
        assert_eq!(state.provider_override.as_deref(), Some("deepseek"));
        assert_eq!(
            state.model_override.as_deref(),
            Some("deepseek-reasoner-search")
        );
    }

    #[test]
    fn repl_unknown_slash_command_is_sent_to_model() {
        let (cli, paths, config, mut session_id, mut first_turn) = repl_test_context();
        let mut state = ReplState {
            stream: true,
            ..ReplState::default()
        };
        let mut stdout = io::stdout();
        let directive = handle_repl_directive(
            ReplInput {
                prompt: "/explain /tmp".to_string(),
                images: Vec::new(),
            },
            &mut state,
            &mut stdout,
            &cli,
            &paths,
            &config,
            &mut session_id,
            &mut first_turn,
            false,
        )
        .unwrap();
        match directive {
            ReplDirective::Submit(input) => assert_eq!(input.prompt, "/explain /tmp"),
            _ => panic!("unexpected repl directive"),
        }
    }

    #[test]
    fn visible_repl_commands_match_new_command_set() {
        let commands = visible_repl_slash_commands()
            .into_iter()
            .map(|command| command.command())
            .collect::<Vec<_>>();
        assert!(commands.contains(&"model"));
        assert!(commands.contains(&"sessions"));
        assert!(commands.contains(&"audit"));
        assert!(commands.contains(&"tool-search"));
        assert!(!commands.contains(&"provider"));
        assert!(!commands.contains(&"tools"));
        assert!(!commands.contains(&"stream"));
    }

    #[test]
    fn parse_repl_slash_command_rejects_commands_alias() {
        assert!(parse_repl_slash_command("/commands").is_none());
        assert!(parse_repl_slash_command("/help").is_none());
    }

    #[test]
    fn filtered_repl_slash_commands_show_all_when_query_empty() {
        let commands = filtered_repl_slash_commands("")
            .into_iter()
            .map(|command| command.command())
            .collect::<Vec<_>>();
        assert!(commands.contains(&"status"));
        assert!(commands.contains(&"sessions"));
        assert!(commands.contains(&"tool-search"));
    }

    #[test]
    fn filtered_repl_slash_commands_filter_by_prefix() {
        let commands = filtered_repl_slash_commands("st")
            .into_iter()
            .map(|command| command.command())
            .collect::<Vec<_>>();
        assert!(commands.contains(&"status"));
        assert!(!commands.contains(&"clear"));
    }

    #[test]
    fn filtered_repl_slash_commands_matches_model_prefix() {
        let commands = filtered_repl_slash_commands("mo")
            .into_iter()
            .map(|command| command.command())
            .collect::<Vec<_>>();
        assert!(commands.contains(&"model"));
        assert!(!commands.contains(&"sessions"));
    }

    #[test]
    fn repl_popup_model_shows_session_choices_for_session_input() {
        let cli = test_cli();
        let config = test_config();
        let state = ReplState::default();
        let (paths, base_dir) = test_paths();
        append_events(
            &paths,
            &config,
            "sess_alpha",
            &[SessionEvent::Message(SessionMessage {
                role: "user".to_string(),
                content: "hello".to_string(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                name: None,
                created_at: now_rfc3339(),
            })],
        )
        .unwrap();
        let draft = ReplEditorDraft {
            slash_mode: Some(ReplSlashMode::Session),
            ..ReplEditorDraft::default()
        };
        let popup = repl_popup_model(&draft, &cli, &paths, &config, &state).unwrap();
        let labels = popup
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>();
        assert!(labels.contains(&"list"));
        assert!(
            labels.contains(&"current") || labels.iter().any(|label| label.starts_with("alpha"))
        );
        let _ = fs::remove_dir_all(base_dir);
    }

    #[test]
    fn repl_popup_accepts_model_command_into_second_stage() {
        let cli = test_cli();
        let config = test_config();
        let state = ReplState::default();
        let (paths, _base_dir) = test_paths();
        let draft = ReplEditorDraft {
            buffer: "/mo".to_string(),
            ..ReplEditorDraft::default()
        };
        let popup = repl_popup_model(&draft, &cli, &paths, &config, &state).unwrap();
        let action = accept_repl_popup_selection(&draft, Some(&popup)).unwrap();
        match action {
            ReplPopupAction::EnterSlashMode(mode) => assert_eq!(mode, ReplSlashMode::Model),
            other => panic!("unexpected popup action: {other:?}"),
        }
    }

    #[test]
    fn repl_popup_accepts_session_choice_as_submit_prompt() {
        let cli = test_cli();
        let config = test_config();
        let state = ReplState::default();
        let (paths, base_dir) = test_paths();
        append_events(
            &paths,
            &config,
            "sess_alpha",
            &[SessionEvent::Message(SessionMessage {
                role: "user".to_string(),
                content: "hello".to_string(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                name: None,
                created_at: now_rfc3339(),
            })],
        )
        .unwrap();
        let mut draft = ReplEditorDraft {
            slash_mode: Some(ReplSlashMode::Session),
            ..ReplEditorDraft::default()
        };
        let popup = repl_popup_model(&draft, &cli, &paths, &config, &state).unwrap();
        draft.popup_selected = popup
            .items
            .iter()
            .position(|item| {
                item.label.starts_with("alpha") || item.label.starts_with("sess_alpha")
            })
            .unwrap();
        let action = accept_repl_popup_selection(&draft, Some(&popup)).unwrap();
        match action {
            ReplPopupAction::SubmitPrompt(prompt) => {
                assert!(prompt.starts_with("/sessions switch "))
            }
            other => panic!("unexpected popup action: {other:?}"),
        }
        let _ = fs::remove_dir_all(base_dir);
    }

    #[test]
    fn enter_repl_slash_mode_clears_input_and_tracks_mode() {
        let mut draft = ReplEditorDraft {
            buffer: "stale".to_string(),
            cursor: 5,
            ..ReplEditorDraft::default()
        };
        enter_repl_slash_mode(&mut draft, ReplSlashMode::Model);
        assert_eq!(draft.slash_mode, Some(ReplSlashMode::Model));
        assert!(draft.buffer.is_empty());
        assert_eq!(draft.cursor, 0);
    }

    #[test]
    fn repl_popup_view_scrolls_to_keep_selected_item_visible() {
        let popup = ReplPopupModel {
            signature: "/model ".to_string(),
            items: (0..12)
                .map(|index| ReplPopupItem {
                    label: format!("item-{index}"),
                    detail: "detail".to_string(),
                    action: ReplPopupAction::SubmitPrompt(format!("/model item-{index}")),
                })
                .collect(),
        };
        let draft = ReplEditorDraft {
            popup_selected: 9,
            ..ReplEditorDraft::default()
        };
        let view = repl_slash_popup_view(Some(&popup), &draft, 120, 4);
        assert_eq!(view.lines.len(), 4);
        assert!(view.lines.iter().any(|line| line.contains("item-9")));
        assert!(!view.lines.iter().any(|line| line.contains("item-0")));
    }

    #[test]
    fn repl_popup_height_is_capped_to_preserve_transcript_space() {
        let popup_height = max_repl_popup_height(20, 5);
        assert_eq!(popup_height, 9);
        let layout = compute_repl_layout(20, 5, popup_height);
        assert_eq!(layout.transcript_height, REPL_MIN_TRANSCRIPT_HEIGHT);
        assert_eq!(layout.popup_height, 9);
    }

    #[test]
    fn repl_status_panel_contains_runtime_configuration() {
        let cli = test_cli();
        let config = test_config();
        let state = ReplState {
            stream: true,
            tools: false,
            context_status: ContextStatusMode::Off,
            provider_override: Some("deepseek".to_string()),
            model_override: Some("deepseek-reasoner-search".to_string()),
            panels: Vec::new(),
            transient_panel: None,
        };
        let panel = build_repl_status_panel(&cli, &config, "sess_test_repl", &state);
        assert_eq!(panel.kind, ReplRuntimePanelKind::Status);
        assert!(panel.body.contains("provider: deepseek"));
        assert!(panel.body.contains("model: deepseek-reasoner-search"));
        assert!(panel.body.contains("tools: off"));
        assert!(panel.body.contains("context_status: off"));
        assert!(panel.body.contains("/model"));
    }

    #[test]
    fn repl_runtime_transcript_appends_panels_after_history() {
        let (paths, _base_dir) = test_paths();
        let config = test_config();
        let panels = vec![ReplRuntimePanel {
            kind: ReplRuntimePanelKind::Error,
            title: "Error".to_string(),
            body: "boom".to_string(),
        }];
        let transcript =
            render_repl_transcript(&paths, &config, "missing_session", 80, &panels, None);
        assert!(transcript.contains("Error"));
        assert!(transcript.contains("boom"));
    }

    #[test]
    fn repl_transient_panel_is_rendered_separately() {
        let (paths, _base_dir) = test_paths();
        let config = test_config();
        let panels = vec![ReplRuntimePanel {
            kind: ReplRuntimePanelKind::Error,
            title: "Error".to_string(),
            body: "persistent".to_string(),
        }];
        let transient = ReplRuntimePanel {
            kind: ReplRuntimePanelKind::Status,
            title: "Status".to_string(),
            body: "temporary".to_string(),
        };
        let transcript = render_repl_transcript(
            &paths,
            &config,
            "missing_session",
            80,
            &panels,
            Some(&transient),
        );
        assert!(transcript.contains("persistent"));
        assert!(transcript.contains("temporary"));
    }

    #[test]
    fn repl_transcript_renders_horizontal_rule_to_requested_width() {
        let config = test_config();
        let turns = vec![vec![SessionMessage {
            role: "assistant".to_string(),
            content: "---".to_string(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
            created_at: now_rfc3339(),
        }]];

        let rendered = render_session_turns_with_width(&config, &turns, None, Some(24));
        let plain = rendered
            .replace(DIM, "")
            .replace(CYAN, "")
            .replace(RESET, "")
            .replace("Assistant\n", "");
        assert!(plain.contains(&"─".repeat(24)));
    }

    #[test]
    fn sync_repl_todo_panel_loads_latest_todos_from_session_history() {
        let (paths, base_dir) = test_paths();
        let config = test_config();
        let session_id = "sess_todo_panel";
        append_events(
            &paths,
            &config,
            session_id,
            &[SessionEvent::Message(SessionMessage {
                role: "assistant".to_string(),
                content: String::new(),
                images: Vec::new(),
                tool_calls: vec![json!({
                    "id": "call_todo",
                    "type": "function",
                    "function": {
                        "name": "TodoWrite",
                        "arguments": "{\"todos\":[{\"title\":\"Implement UI\",\"details\":\"Create the initial UI implementation for the REPL todo panel.\",\"status\":\"in_progress\"},{\"title\":\"Run tests\",\"details\":\"Run the full test suite after the UI update.\",\"status\":\"pending\"}]}"
                    }
                })],
                tool_call_id: None,
                name: None,
                created_at: now_rfc3339(),
            })],
        )
        .unwrap();

        let mut state = ReplState::default();
        sync_repl_todo_panel(&paths, &config, session_id, &mut state);
        sync_repl_todo_panel(&paths, &config, session_id, &mut state);
        let transcript =
            render_repl_transcript(&paths, &config, session_id, 80, &state.panels, None);

        assert_eq!(
            state
                .panels
                .iter()
                .filter(|panel| panel.kind == ReplRuntimePanelKind::Todo)
                .count(),
            1
        );
        assert!(transcript.contains("• Updated Plan"));
        assert!(
            transcript.contains("Create the initial UI implementation for the REPL todo panel.")
        );
        assert!(transcript.contains("□ Implement UI"));
        assert!(transcript.contains("□ Run tests"));

        let _ = fs::remove_dir_all(base_dir);
    }

    #[test]
    fn full_width_rule_uses_visible_terminal_width() {
        let rendered = full_width_rule(20);
        assert_eq!(visible_width(&rendered), 20);
    }

    #[test]
    fn transcript_screen_lines_wrap_long_plain_lines() {
        let lines = transcript_screen_lines("abcdefgh", 4);
        assert_eq!(lines, vec!["abcd".to_string(), "efgh".to_string()]);
    }

    #[test]
    fn transcript_screen_lines_wrap_ansi_and_wide_text() {
        let transcript = format!("{DIM}你好abcd{RESET}");
        let lines = transcript_screen_lines(&transcript, 4);
        assert_eq!(lines.len(), 2);
        assert_eq!(visible_width(&lines[0]), 4);
        assert_eq!(visible_width(&lines[1]), 4);
        assert!(lines[0].starts_with(DIM));
        assert!(lines[1].starts_with(DIM));
        assert!(lines.iter().all(|line| line.ends_with(RESET)));
    }

    #[test]
    fn render_tool_call_summary_uses_multiple_lines() {
        let raw_calls = vec![
            json!({
                "id": "call_webfetch",
                "type": "function",
                "function": {
                    "name": "WebFetch",
                    "arguments": "{\"url\":\"https://openai.com/index/introducing-gpt-5-4-and-more\"}"
                }
            }),
            json!({
                "id": "call_bash",
                "type": "function",
                "function": {
                    "name": "Bash",
                    "arguments": "{\"command\":\"echo '=== Testing grok-search web_search ==='\"}"
                }
            }),
        ];

        let rendered = render_tool_call_summary(1, &raw_calls, 28);
        let plain = rendered.replace(DIM, "").replace(RESET, "");

        assert!(plain.starts_with("[tools 1]"));
        assert!(plain.contains("\n  • WebFetch:"));
        assert!(plain.contains("\n  • Bash:"));
        assert!(rendered.lines().count() >= 3);
    }

    #[test]
    fn repl_composer_view_does_not_render_status_inside_input_box() {
        let draft = ReplEditorDraft {
            status: Some("Removed one image. 0 remaining.".to_string()),
            slash_mode: Some(ReplSlashMode::Model),
            ..ReplEditorDraft::default()
        };
        let view = repl_composer_view(&draft, 80, 8);
        assert_eq!(view.lines.len(), 1);
        assert!(view.lines[0].contains("Filter models."));
        assert!(!view.lines[0].contains("Removed one image."));
    }

    #[test]
    fn display_width_counts_wide_chars() {
        assert_eq!(display_width("你好"), 4);
        assert_eq!(display_width("你a"), 3);
    }

    #[test]
    fn wrap_editor_input_tracks_wide_char_cursor_columns() {
        let wrapped = wrap_editor_input("你a", '你'.len_utf8(), 10);
        assert_eq!(wrapped.lines, vec!["你a"]);
        assert_eq!(wrapped.cursor_row, 0);
        assert_eq!(wrapped.cursor_col, 2);
    }

    #[test]
    fn wrap_editor_input_moves_cursor_to_next_line_after_newline() {
        let wrapped = wrap_editor_input("ab\n", 3, 10);
        assert_eq!(wrapped.lines, vec!["ab", ""]);
        assert_eq!(wrapped.cursor_row, 1);
        assert_eq!(wrapped.cursor_col, 0);
    }

    #[test]
    fn resolve_transcript_scroll_defaults_to_bottom() {
        assert_eq!(resolve_transcript_scroll(100, 10, usize::MAX), 90);
        assert_eq!(resolve_transcript_scroll(5, 10, usize::MAX), 0);
    }

    #[test]
    fn manual_repl_slash_prompt_requires_query_for_command_mode() {
        let draft = ReplEditorDraft {
            slash_mode: Some(ReplSlashMode::Commands),
            ..ReplEditorDraft::default()
        };
        assert!(manual_repl_slash_prompt(&draft).is_none());
    }

    #[test]
    fn manual_repl_slash_prompt_requires_query_for_toggle_modes() {
        let draft = ReplEditorDraft {
            slash_mode: Some(ReplSlashMode::Audit),
            ..ReplEditorDraft::default()
        };
        assert!(manual_repl_slash_prompt(&draft).is_none());
    }

    #[test]
    fn repl_popup_model_shows_default_items_for_empty_session_query() {
        let cli = test_cli();
        let config = test_config();
        let state = ReplState::default();
        let (paths, base_dir) = test_paths();
        let draft = ReplEditorDraft {
            slash_mode: Some(ReplSlashMode::Session),
            ..ReplEditorDraft::default()
        };
        let popup = repl_popup_model(&draft, &cli, &paths, &config, &state).unwrap();
        assert!(!popup.items.is_empty());
        let _ = fs::remove_dir_all(base_dir);
    }

    #[test]
    fn large_pasted_text_is_collapsed_into_badge_payload() {
        let mut draft = ReplEditorDraft::default();
        handle_pasted_text(&mut draft, "a\nb\nc\nd\ne\nf\ng\nh\ni".to_string());
        assert!(draft.buffer.is_empty());
        assert_eq!(draft.collapsed_pastes.len(), 1);
        assert_eq!(materialize_repl_prompt(&draft), "a\nb\nc\nd\ne\nf\ng\nh\ni");
        assert!(repl_attachment_badges(&draft).contains("[Pasted text #1 +8 lines]"));
        assert!(draft.status.is_none());
    }

    #[test]
    fn clear_repl_text_input_keeps_images_but_drops_text_payloads() {
        let mut draft = ReplEditorDraft {
            buffer: "hello".to_string(),
            images: vec![MessageImage {
                media_type: "image/png".to_string(),
                data: "abc".to_string(),
            }],
            collapsed_pastes: vec![CollapsedPaste {
                text: "world".to_string(),
                line_count: 1,
            }],
            status: None,
            cursor: 5,
            transcript_scroll: 0,
            esc_pending: true,
            ..ReplEditorDraft::default()
        };
        clear_repl_text_input(&mut draft);
        assert_eq!(draft.buffer, "");
        assert_eq!(draft.cursor, 0);
        assert_eq!(draft.collapsed_pastes.len(), 0);
        assert_eq!(draft.images.len(), 1);
        assert!(!draft.esc_pending);
    }

    #[test]
    fn join_inline_prompt_segments_compacts_badges_and_text() {
        let inline = join_inline_prompt_segments(&[
            Some("[Image #1]".to_string()),
            Some("[Pasted text #2 +5 lines]".to_string()),
            Some("你好".to_string()),
        ]);
        assert_eq!(inline, "[Image #1] [Pasted text #2 +5 lines] 你好");
    }

    #[test]
    fn repl_composer_view_keeps_attachment_badges_inline() {
        let draft = ReplEditorDraft {
            buffer: "你好".to_string(),
            images: vec![MessageImage {
                media_type: "image/png".to_string(),
                data: "abc".to_string(),
            }],
            collapsed_pastes: Vec::new(),
            status: None,
            cursor: "你好".len(),
            transcript_scroll: 0,
            esc_pending: false,
            ..ReplEditorDraft::default()
        };
        let view = repl_composer_view(&draft, 80, 8);
        assert!(view.lines[0].contains("❯ [Image #1] 你好"));
    }

    #[test]
    fn requested_session_id_prefers_temp_over_new_session() {
        let session_id = requested_session_id(true, true, false).unwrap();
        assert!(session_id.starts_with("tmp_"));
    }

    #[test]
    fn select_session_id_accepts_unsaved_current_session_when_explicit() {
        let (paths, base_dir) = test_paths();
        let config = test_config();
        let expected = "sess_unsaved_explicit".to_string();
        set_current_session(&paths, &config, Some(&expected), false).unwrap();

        let selected = select_session_id(
            &paths,
            &config,
            Some(expected.as_str()),
            false,
            false,
            false,
        )
        .unwrap();
        assert_eq!(selected, expected);

        let _ = fs::remove_dir_all(base_dir);
    }

    #[test]
    fn select_session_id_accepts_unsaved_current_session_by_short_id() {
        let (paths, base_dir) = test_paths();
        let config = test_config();
        let expected = format!("sess_{}", ulid::Ulid::new());
        let short = short_id(&expected);
        set_current_session(&paths, &config, Some(&expected), false).unwrap();

        let selected =
            select_session_id(&paths, &config, Some(&short), false, false, false).unwrap();
        assert_eq!(selected, expected);

        let _ = fs::remove_dir_all(base_dir);
    }

    #[test]
    fn ensure_repl_session_id_keeps_unsaved_current_session() {
        let (paths, base_dir) = test_paths();
        let config = test_config();
        let mut session_id = "sess_unsaved".to_string();
        set_current_session(&paths, &config, Some(&session_id), false).unwrap();

        let switched = ensure_repl_session_id(&paths, &config, &mut session_id, false).unwrap();
        assert!(!switched);
        assert_eq!(session_id, "sess_unsaved");

        let _ = fs::remove_dir_all(base_dir);
    }

    #[test]
    fn ensure_repl_session_id_replaces_deleted_session() {
        let (paths, base_dir) = test_paths();
        let config = test_config();
        let deleted = "sess_deleted".to_string();
        set_current_session(&paths, &config, Some(&deleted), false).unwrap();
        set_current_session(&paths, &config, None, false).unwrap();

        let mut session_id = deleted.clone();
        let switched = ensure_repl_session_id(&paths, &config, &mut session_id, false).unwrap();
        assert!(switched);
        assert_ne!(session_id, deleted);
        assert!(session_id.starts_with("sess_"));
        assert_eq!(
            load_state(&paths).unwrap().current_session.as_deref(),
            Some(session_id.as_str())
        );

        let _ = fs::remove_dir_all(base_dir);
    }

    #[test]
    fn resolve_render_session_id_uses_current_session_when_omitted() {
        let (paths, base_dir) = test_paths();
        let config = test_config();
        set_current_session(&paths, &config, Some("sess_render"), false).unwrap();

        let resolved = resolve_session_or_current(&paths, &config, None, "render").unwrap();
        assert_eq!(resolved, "sess_render");

        let _ = fs::remove_dir_all(base_dir);
    }

    #[test]
    fn resolve_session_or_current_reports_command_specific_hint() {
        let (paths, base_dir) = test_paths();
        let config = test_config();

        let err = resolve_session_or_current(&paths, &config, None, "show").unwrap_err();
        assert!(
            err.message
                .contains("no active session; use `chat session show <id>`")
        );

        let _ = fs::remove_dir_all(base_dir);
    }

    #[test]
    fn session_turns_group_messages_by_user_turn() {
        let events = vec![
            SessionEvent::Message(SessionMessage {
                role: "system".to_string(),
                content: "系统提示".to_string(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                name: None,
                created_at: now_rfc3339(),
            }),
            SessionEvent::Message(SessionMessage {
                role: "user".to_string(),
                content: "first question".to_string(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                name: None,
                created_at: now_rfc3339(),
            }),
            SessionEvent::Message(SessionMessage {
                role: "assistant".to_string(),
                content: "first answer".to_string(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                name: None,
                created_at: now_rfc3339(),
            }),
            SessionEvent::Message(SessionMessage {
                role: "tool".to_string(),
                content: "tool output".to_string(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: Some("call_1".to_string()),
                name: Some("read".to_string()),
                created_at: now_rfc3339(),
            }),
            SessionEvent::Message(SessionMessage {
                role: "user".to_string(),
                content: "second question".to_string(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                name: None,
                created_at: now_rfc3339(),
            }),
            SessionEvent::Message(SessionMessage {
                role: "assistant".to_string(),
                content: "second answer".to_string(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                name: None,
                created_at: now_rfc3339(),
            }),
        ];

        let turns = session_turns_from_events(&events);
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[0].len(), 1);
        assert_eq!(turns[1].len(), 3);
        assert_eq!(turns[2].len(), 2);
        assert_eq!(turns[0][0].role, "system");
        let repl_turns = filter_session_turns_for_repl(&turns);
        assert_eq!(repl_turns.len(), 2);
        assert_eq!(repl_turns[0][0].role, "user");
        assert_eq!(turns[1][0].content, "first question");
        assert_eq!(turns[2][0].content, "second question");
    }

    #[test]
    fn render_session_turns_formats_user_messages_with_accent_bar() {
        let config = test_config();
        let turns = vec![vec![
            SessionMessage {
                role: "user".to_string(),
                content: "用户提问".to_string(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                name: None,
                created_at: now_rfc3339(),
            },
            SessionMessage {
                role: "assistant".to_string(),
                content: "助手回答".to_string(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                name: None,
                created_at: now_rfc3339(),
            },
        ]];

        let rendered = render_session_turns(&config, &turns, Some(1));
        assert!(rendered.contains(&format!("{GREEN}User{RESET}")));
        assert!(rendered.contains(&format!("{GREEN}│{RESET}")));
        assert!(rendered.contains("用户提问"));
        assert!(rendered.contains(&format!("{CYAN}Assistant{RESET}")));
        assert!(rendered.contains("助手回答"));
    }

    #[test]
    fn render_session_turns_supports_all_turns() {
        let config = test_config();
        let turns = vec![
            vec![SessionMessage {
                role: "user".to_string(),
                content: "第一条".to_string(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                name: None,
                created_at: now_rfc3339(),
            }],
            vec![SessionMessage {
                role: "user".to_string(),
                content: "第二条".to_string(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                name: None,
                created_at: now_rfc3339(),
            }],
        ];

        let rendered = render_session_turns(&config, &turns, None);
        assert!(rendered.contains("第一条"));
        assert!(rendered.contains("第二条"));
    }

    #[test]
    fn model_list_entry_uses_provider_and_remote_name() {
        let model = ModelConfig {
            provider: "minimax".to_string(),
            remote_name: "MiniMax-M2.7".to_string(),
            display_name: None,
            context_window: None,
            max_output_tokens: None,
            capabilities: Vec::new(),
            temperature: None,
            reasoning_effort: None,
            patches: ModelPatchConfig::default(),
        };
        assert_eq!(format_model_list_entry(&model), "minimax/MiniMax-M2.7");
    }

    #[test]
    fn format_session_list_entry_prioritizes_prompt_and_compacts_metadata() {
        let summary = crate::session::SessionSummary {
            session_id: "tmp_01KPCJPG".to_string(),
            is_current: true,
            is_temp: true,
            updated_at: Some(1776391290),
            first_prompt: Some("测试一下todo工具".to_string()),
            user_messages: 2,
            assistant_messages: 4,
        };

        let rendered = format_session_list_entry(&summary);
        assert!(rendered.starts_with("* tmp_01KPCJPG [temp] \"测试一下todo工具\""));
        assert!(rendered.contains("2u/4a"));
        assert!(rendered.contains("updated=1776391290"));
        assert!(!rendered.contains("created_at="));
        assert!(!rendered.contains("first_prompt="));
    }

    #[test]
    fn format_final_ask_output_renders_think_blocks_for_text_output() {
        let config = AppConfig::default();
        let result = AskExecution {
            format: OutputFormat::Text,
            output: AskOutput {
                ok: true,
                provider: "deepseek".to_string(),
                model: "deepseek-reasoner-search".to_string(),
                session_id: "sess_test".to_string(),
                message: AssistantMessage {
                    role: "assistant".to_string(),
                    content: "<think>\n先分析\n</think>\n\n答案".to_string(),
                },
                usage: Usage::default(),
                finish_reason: "stop".to_string(),
                latency_ms: 1,
                raw_provider_response: None,
            },
        };
        let rendered = format_final_ask_output(&config, &result, false).unwrap();
        assert!(rendered.contains("先分析"));
        assert!(rendered.contains("答案"));
        assert!(!rendered.contains("<think>"));
    }

    #[test]
    fn parse_audit_report_extracts_json_from_wrapped_content() {
        let calls = vec![crate::tool::ToolCall {
            id: "call_1".to_string(),
            name: "bash".to_string(),
            arguments: json!({"command":"ls -la"}),
        }];
        let batch = parse_batch_audit_report(
            ChatResponse {
            provider_id: "cpap".to_string(),
            model_id: "audit-model".to_string(),
            content:
                "<think>checking</think>\n{\"results\":[{\"id\":\"call_1\",\"verdict\":\"block\",\"message\":\"危险操作\"}]}"
                    .to_string(),
            finish_reason: "stop".to_string(),
            usage: Usage::default(),
            latency_ms: 1,
            raw: json!({}),
            tool_calls: Vec::new(),
            },
            &calls,
        );
        let report = batch.reports.get("call_1").unwrap();
        assert_eq!(report.verdict, "block");
        assert_eq!(report.message, "危险操作");
    }

    #[test]
    fn audit_prompt_kind_routes_bash_and_edit_calls() {
        let bash_call = crate::tool::ToolCall {
            id: "call_bash".to_string(),
            name: "Bash".to_string(),
            arguments: json!({"command":"git status"}),
        };
        let edit_call = crate::tool::ToolCall {
            id: "call_edit".to_string(),
            name: "write".to_string(),
            arguments: json!({"path":"src/main.rs"}),
        };
        let other_call = crate::tool::ToolCall {
            id: "call_other".to_string(),
            name: "OtherMutatingTool".to_string(),
            arguments: json!({}),
        };

        assert_eq!(audit_prompt_kind(&bash_call), AuditPromptKind::Bash);
        assert_eq!(audit_prompt_kind(&edit_call), AuditPromptKind::Edit);
        assert_eq!(audit_prompt_kind(&other_call), AuditPromptKind::Default);
    }

    #[test]
    fn load_audit_prompt_reads_configured_prompt_files() {
        let (paths, base_dir) = test_paths();
        let mut config = test_config();
        let prompt_dir = paths.config_dir.join("custom-prompts");
        fs::create_dir_all(&prompt_dir).unwrap();

        let default_prompt = prompt_dir.join("default.md");
        let bash_prompt = prompt_dir.join("bash.md");
        let edit_prompt = prompt_dir.join("edit.md");
        fs::write(&default_prompt, "default prompt").unwrap();
        fs::write(&bash_prompt, "bash prompt").unwrap();
        fs::write(&edit_prompt, "edit prompt").unwrap();

        config.audit.default_prompt_file = Some(default_prompt.display().to_string());
        config.audit.bash_prompt_file = Some(bash_prompt.display().to_string());
        config.audit.edit_prompt_file = Some(edit_prompt.display().to_string());

        assert_eq!(
            load_audit_prompt(&config, AuditPromptKind::Default).unwrap(),
            "default prompt"
        );
        assert_eq!(
            load_audit_prompt(&config, AuditPromptKind::Bash).unwrap(),
            "bash prompt"
        );
        assert_eq!(
            load_audit_prompt(&config, AuditPromptKind::Edit).unwrap(),
            "edit prompt"
        );

        let _ = fs::remove_dir_all(base_dir);
    }

    #[test]
    fn tool_rounds_stream_when_requested() {
        let request = ChatRequest {
            provider_id: "openclawbs".to_string(),
            provider: ProviderConfig {
                kind: "openai_compatible".to_string(),
                ..ProviderConfig::default()
            },
            model_id: "claude-sonnet-4-6".to_string(),
            model: ModelConfig {
                provider: "openclawbs".to_string(),
                remote_name: "claude-sonnet-4-6".to_string(),
                display_name: None,
                context_window: None,
                max_output_tokens: None,
                capabilities: vec!["reasoning".to_string()],
                temperature: None,
                reasoning_effort: Some("medium".to_string()),
                patches: ModelPatchConfig::default(),
            },
            api_key: String::new(),
            messages: Vec::new(),
            temperature: None,
            max_output_tokens: None,
            params: BTreeMap::new(),
            timeout_secs: None,
            tools: Vec::new(),
        };
        assert!(should_stream_tool_round(&request, true, 0));
        assert!(should_stream_tool_round(&request, true, 1));
        assert!(!should_stream_tool_round(&request, false, 0));
    }

    #[test]
    fn execute_tool_as_message_returns_tool_error_content() {
        let config = AppConfig::default();
        let raw_call = json!({
            "id": "call_1",
            "function": {
                "name": "read",
                "arguments": "{\"path\":\"/definitely/missing/file.txt\"}"
            }
        });
        let message = execute_tool_as_message(&raw_call, true, &config);
        assert_eq!(message.role, "tool");
        assert_eq!(message.tool_call_id.as_deref(), Some("call_1"));
        assert!(message.content.contains("tool execution error:"));
    }

    #[test]
    fn execute_tool_as_message_handles_invalid_tool_call_payload() {
        let config = AppConfig::default();
        let raw_call = json!({
            "id": "call_bad",
            "function": {
                "arguments": "{\"path\":\"foo\"}"
            }
        });
        let message = execute_tool_as_message(&raw_call, true, &config);
        assert_eq!(message.role, "tool");
        assert_eq!(message.tool_call_id.as_deref(), Some("call_bad"));
        assert!(message.content.contains("tool invocation error:"));
    }

    #[test]
    fn execute_bash_tool_declines_when_interactive_confirmation_is_unavailable() {
        let config = AppConfig::default();
        let raw_call = json!({
            "id": "call_bash",
            "function": {
                "name": "Bash",
                "arguments": "{\"command\":\"rm -f /tmp/chat-cli-test\"}"
            }
        });
        let message = execute_tool_as_message(&raw_call, false, &config);
        assert_eq!(message.role, "tool");
        assert_eq!(message.tool_call_id.as_deref(), Some("call_bash"));
        assert!(
            message
                .content
                .contains("interactive confirmation unavailable")
                || message.content.contains("user declined the bash execution")
        );
    }

    #[test]
    fn persist_tool_session_replays_tool_messages_on_next_turn() {
        let (paths, base_dir) = test_paths();
        let cli = test_cli();
        let config = test_config();
        let mut secrets = SecretsConfig::default();
        secrets.providers.insert(
            "cpap".to_string(),
            ProviderSecret {
                api_key: Some("test-key".to_string()),
            },
        );

        let mut first_args = ask_args("读取 Cargo.toml");
        first_args.new_session = true;

        let prepared = prepare_ask(&cli, &paths, &config, &secrets, &first_args, None).unwrap();
        let tool_call = json!({
            "id": "call_1",
            "type": "function",
            "function": {
                "name": "read",
                "arguments": "{\"path\":\"Cargo.toml\"}"
            }
        });
        let mut turn_messages = prepared.request.messages.clone();
        turn_messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: String::new(),
            images: Vec::new(),
            tool_calls: Some(vec![tool_call.clone()]),
            tool_call_id: None,
            name: None,
        });
        turn_messages.push(ChatMessage {
            role: "tool".to_string(),
            content: "[package]\nname = \"chat-cli\"".to_string(),
            images: Vec::new(),
            tool_calls: None,
            tool_call_id: Some("call_1".to_string()),
            name: Some("read".to_string()),
        });

        let response = ChatResponse {
            provider_id: "cpap".to_string(),
            model_id: "team-gpt-5-4".to_string(),
            content: "这是 Cargo 配置文件。".to_string(),
            finish_reason: "stop".to_string(),
            usage: Usage::default(),
            latency_ms: 1,
            raw: json!({}),
            tool_calls: Vec::new(),
        };
        persist_tool_session(
            &paths,
            &config,
            &first_args,
            &prepared.session_preamble,
            &prepared.session_id,
            &turn_messages,
            &response,
        )
        .unwrap();

        let stored_events = read_events(&paths, &config, &prepared.session_id).unwrap();
        let stored_messages = stored_events
            .iter()
            .filter_map(|event| match event {
                SessionEvent::Message(message) => Some(message),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(stored_messages.len(), 4);
        assert_eq!(stored_messages[1].role, "assistant");
        assert_eq!(stored_messages[1].tool_calls.len(), 1);
        assert_eq!(stored_messages[2].role, "tool");
        assert_eq!(stored_messages[2].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(stored_messages[2].name.as_deref(), Some("read"));

        let mut second_args = ask_args("继续总结");
        second_args.session = Some(prepared.session_id.clone());
        let second = prepare_ask(&cli, &paths, &config, &secrets, &second_args, None).unwrap();
        assert_eq!(second.request.messages[0].role, "user");
        assert_eq!(second.request.messages[1].role, "assistant");
        assert_eq!(
            second.request.messages[1].tool_calls.as_ref().map(Vec::len),
            Some(1)
        );
        assert_eq!(second.request.messages[2].role, "tool");
        assert_eq!(
            second.request.messages[2].tool_call_id.as_deref(),
            Some("call_1")
        );
        assert_eq!(second.request.messages[4].role, "user");

        let _ = fs::remove_dir_all(base_dir);
    }

    #[test]
    fn prepare_ask_reuses_persisted_system_prompt_across_turns() {
        let (paths, base_dir) = test_paths();
        let cli = test_cli();
        let config = test_config();
        let mut secrets = SecretsConfig::default();
        secrets.providers.insert(
            "cpap".to_string(),
            ProviderSecret {
                api_key: Some("test-key".to_string()),
            },
        );

        let mut first_args = ask_args("第一条消息是啥");
        first_args.system = Some("回答时保持简洁".to_string());
        first_args.new_session = true;

        let first = prepare_ask(&cli, &paths, &config, &secrets, &first_args, None).unwrap();
        assert_eq!(first.session_preamble.len(), 1);
        assert_eq!(first.request.messages[0].role, "system");
        assert_eq!(first.request.messages[0].content, "回答时保持简洁");

        let response = ChatResponse {
            provider_id: "cpap".to_string(),
            model_id: "team-gpt-5-4".to_string(),
            content: "第一条消息是用户问题。".to_string(),
            finish_reason: "stop".to_string(),
            usage: Usage::default(),
            latency_ms: 1,
            raw: json!({}),
            tool_calls: Vec::new(),
        };
        persist_session(
            &paths,
            &config,
            &first_args,
            &first.session_preamble,
            &first.persisted_user_content,
            &first.user_images,
            &first.session_id,
            &response,
        )
        .unwrap();

        let stored_events = read_events(&paths, &config, &first.session_id).unwrap();
        let stored_roles = stored_events
            .iter()
            .filter_map(|event| match event {
                SessionEvent::Message(message) => Some(message.role.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(stored_roles, vec!["system", "user", "assistant"]);

        let mut second_args = ask_args("再说一遍");
        second_args.session = Some(first.session_id.clone());
        let second = prepare_ask(&cli, &paths, &config, &secrets, &second_args, None).unwrap();

        let second_roles = second
            .request
            .messages
            .iter()
            .map(|message| message.role.as_str())
            .collect::<Vec<_>>();
        assert_eq!(second_roles, vec!["system", "user", "assistant", "user"]);
        assert_eq!(second.request.messages[0].content, "回答时保持简洁");
        assert!(second.session_preamble.is_empty());

        let _ = fs::remove_dir_all(base_dir);
    }

    #[test]
    fn context_status_always_persists_injected_user_message() {
        let (paths, base_dir) = test_paths();
        let cli = test_cli();
        let config = test_config();
        let mut secrets = SecretsConfig::default();
        secrets.providers.insert(
            "cpap".to_string(),
            ProviderSecret {
                api_key: Some("test-key".to_string()),
            },
        );

        let mut args = ask_args("看看这个仓库");
        args.new_session = true;
        args.context_status = Some(ContextStatusMode::Always);

        let prepared = prepare_ask(&cli, &paths, &config, &secrets, &args, None).unwrap();
        assert!(
            prepared.request.messages[0]
                .content
                .starts_with("# Current Status")
        );
        assert!(
            prepared
                .persisted_user_content
                .starts_with("# Current Status")
        );

        let response = ChatResponse {
            provider_id: "cpap".to_string(),
            model_id: "team-gpt-5-4".to_string(),
            content: "这是一个 Rust 项目。".to_string(),
            finish_reason: "stop".to_string(),
            usage: Usage::default(),
            latency_ms: 1,
            raw: json!({}),
            tool_calls: Vec::new(),
        };
        persist_session(
            &paths,
            &config,
            &args,
            &prepared.session_preamble,
            &prepared.persisted_user_content,
            &prepared.user_images,
            &prepared.session_id,
            &response,
        )
        .unwrap();

        let stored_events = read_events(&paths, &config, &prepared.session_id).unwrap();
        let stored_messages = stored_events
            .iter()
            .filter_map(|event| match event {
                SessionEvent::Message(message) => Some(message),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(stored_messages[0].content.starts_with("# Current Status"));

        let _ = fs::remove_dir_all(base_dir);
    }

    #[test]
    fn context_status_latest_is_filtered_from_persisted_tool_history() {
        let (paths, base_dir) = test_paths();
        let cli = test_cli();
        let config = test_config();
        let mut secrets = SecretsConfig::default();
        secrets.providers.insert(
            "cpap".to_string(),
            ProviderSecret {
                api_key: Some("test-key".to_string()),
            },
        );

        let mut args = ask_args("读取 Cargo.toml");
        args.new_session = true;
        args.tools = true;
        args.context_status = Some(ContextStatusMode::Latest);

        let prepared = prepare_ask(&cli, &paths, &config, &secrets, &args, None).unwrap();
        assert!(
            prepared.request.messages[0]
                .content
                .starts_with("# Current Status")
        );

        let mut turn_messages = prepared.request.messages.clone();
        turn_messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: String::new(),
            images: Vec::new(),
            tool_calls: Some(vec![json!({
                "id": "call_1",
                "type": "function",
                "function": {
                    "name": "Read",
                    "arguments": "{\"file_path\":\"Cargo.toml\"}"
                }
            })]),
            tool_call_id: None,
            name: None,
        });
        turn_messages.push(ChatMessage {
            role: "tool".to_string(),
            content: "[package]\nname = \"chat-cli\"".to_string(),
            images: Vec::new(),
            tool_calls: None,
            tool_call_id: Some("call_1".to_string()),
            name: Some("read".to_string()),
        });

        let persisted_turn_messages =
            persisted_turn_messages_for_session(&prepared, &turn_messages);
        assert_eq!(persisted_turn_messages[0].content, "读取 Cargo.toml");

        let response = ChatResponse {
            provider_id: "cpap".to_string(),
            model_id: "team-gpt-5-4".to_string(),
            content: "这是 Cargo 配置文件。".to_string(),
            finish_reason: "stop".to_string(),
            usage: Usage::default(),
            latency_ms: 1,
            raw: json!({}),
            tool_calls: Vec::new(),
        };
        persist_tool_session(
            &paths,
            &config,
            &args,
            &prepared.session_preamble,
            &prepared.session_id,
            &persisted_turn_messages,
            &response,
        )
        .unwrap();

        let stored_events = read_events(&paths, &config, &prepared.session_id).unwrap();
        let stored_messages = stored_events
            .iter()
            .filter_map(|event| match event {
                SessionEvent::Message(message) => Some(message),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(stored_messages[0].content, "读取 Cargo.toml");

        let mut second_args = ask_args("继续总结");
        second_args.session = Some(prepared.session_id.clone());
        let second = prepare_ask(&cli, &paths, &config, &secrets, &second_args, None).unwrap();
        assert_eq!(second.request.messages[0].content, "读取 Cargo.toml");

        let _ = fs::remove_dir_all(base_dir);
    }

    #[test]
    fn context_status_system_once_is_injected_after_system_prompt_only_on_first_turn() {
        let (paths, base_dir) = test_paths();
        let cli = test_cli();
        let config = test_config();
        let mut secrets = SecretsConfig::default();
        secrets.providers.insert(
            "cpap".to_string(),
            ProviderSecret {
                api_key: Some("test-key".to_string()),
            },
        );

        let mut first_args = ask_args("第一条消息");
        first_args.new_session = true;
        first_args.system = Some("回答时保持简洁".to_string());
        first_args.context_status = Some(ContextStatusMode::SystemOnce);

        let first = prepare_ask(&cli, &paths, &config, &secrets, &first_args, None).unwrap();
        let first_roles = first
            .request
            .messages
            .iter()
            .map(|message| message.role.as_str())
            .collect::<Vec<_>>();
        assert_eq!(first_roles, vec!["system", "system", "user"]);
        assert_eq!(first.request.messages[0].content, "回答时保持简洁");
        assert!(
            first.request.messages[1]
                .content
                .starts_with("# Current Status")
        );
        assert_eq!(first.persisted_user_content, "第一条消息");

        let response = ChatResponse {
            provider_id: "cpap".to_string(),
            model_id: "team-gpt-5-4".to_string(),
            content: "第一条回复".to_string(),
            finish_reason: "stop".to_string(),
            usage: Usage::default(),
            latency_ms: 1,
            raw: json!({}),
            tool_calls: Vec::new(),
        };
        persist_session(
            &paths,
            &config,
            &first_args,
            &first.session_preamble,
            &first.persisted_user_content,
            &first.user_images,
            &first.session_id,
            &response,
        )
        .unwrap();

        let mut second_args = ask_args("第二条消息");
        second_args.session = Some(first.session_id.clone());
        second_args.context_status = Some(ContextStatusMode::SystemOnce);
        let second = prepare_ask(&cli, &paths, &config, &secrets, &second_args, None).unwrap();
        let second_roles = second
            .request
            .messages
            .iter()
            .map(|message| message.role.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            second_roles,
            vec!["system", "system", "user", "assistant", "user"]
        );
        assert!(
            second.request.messages[1]
                .content
                .starts_with("# Current Status")
        );
        assert_eq!(second.request.messages[4].content, "第二条消息");
        assert!(second.session_preamble.is_empty());

        let _ = fs::remove_dir_all(base_dir);
    }
}

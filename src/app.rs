use crate::cli::{
    AskArgs, AuthCommand, AuthSetArgs, Cli, Commands, ConfigCommand, ModelCommand, ModelSetArgs,
    OutputFormat, ProviderCommand, ReplArgs, SessionCommand,
};
use crate::config::{
    AppConfig, AppPaths, ModelConfig, ModelPatchConfig, ProviderConfig, ProviderSecret,
    SecretsConfig, apply_runtime_config_defaults, ensure_dirs, init_config_files, load_config,
    load_secrets, parse_headers, read_system_prompt, render_config_value, save_config,
    save_secrets, set_config_value, validate_config,
};
use crate::error::{
    AppError, AppResult, EXIT_ARGS, EXIT_AUTH, EXIT_CONFIG, EXIT_MODEL, EXIT_PROVIDER,
};
use crate::media::{MessageImage, read_image_inputs};
use crate::output::{AskOutput, AssistantMessage, render_ask_output};
use crate::provider::{
    ChatMessage, ChatRequest, ChatResponse, send_chat, stream_chat, test_provider,
};
use crate::render::{StreamPhase, StreamRenderer, StreamStatus, print_status_bar, render_markdown};
use crate::session::{
    SessionAudit, SessionEvent, SessionMessage, SessionResponse, append_events, clear_sessions,
    delete_session, gc_sessions, generate_session_id, generate_temp_session_id, is_temp_session,
    list_session_summaries, load_state, now_rfc3339, read_events, resolve_session_id,
    set_current_session, short_id,
};
use crate::tool::{
    execute_tool, initial_tool_definitions, lookup_tool_spec, parse_tool_call,
    tool_call_requires_confirmation, tool_call_side_effects, tool_definitions_for_names,
    tool_search_matches,
};
use clap::CommandFactory;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read, Write};

const CYAN: &str = "\x1b[36m";
const DIM: &str = "\x1b[2m";
const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const RESET: &str = "\x1b[0m";

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
        _ => {}
    }

    let mut config = load_config(&paths)?;
    apply_runtime_config_defaults(&paths, &mut config);
    ensure_dirs(&paths, &config)?;
    let mut secrets = load_secrets(&paths)?;

    match cli.command {
        Commands::Ask(args) => handle_ask(&root, &paths, &config, &secrets, args).await,
        Commands::Repl(args) => handle_repl(&root, &paths, &config, &secrets, args).await,
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
                let marker = if summary.is_current { "* " } else { "  " };
                let temp_tag = if summary.is_temp { " [temp]" } else { "" };
                println!(
                    "{}{}{} created_at={} updated_at={} user_messages={} assistant_messages={} first_prompt={}",
                    marker,
                    short_id(&summary.session_id),
                    temp_tag,
                    summary.created_at.unwrap_or(0),
                    summary.updated_at.unwrap_or(0),
                    summary.user_messages,
                    summary.assistant_messages,
                    serde_json::to_string(summary.first_prompt.as_deref().unwrap_or(""))
                        .unwrap_or_default(),
                );
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
            let resolved = resolve_session_id(paths, config, &id)?;
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
    let use_tools = args.tools || config.defaults.tools.unwrap_or(false);
    if use_tools {
        let output_fmt = if args.stream {
            Some(OutputFormat::Text)
        } else {
            cli.output.clone()
        };
        let result = execute_ask_with_tools(cli, paths, config, secrets, &args, output_fmt).await?;
        if !args.stream {
            let rendered = format_final_ask_output(config, &result, args.raw_provider_response)?;
            println!("{rendered}");
        }
        return Ok(());
    }
    if args.stream {
        execute_ask_stream(cli, paths, config, secrets, &args, cli.output.clone()).await?;
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
                images: Vec::new(),
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
            execute_tool_as_message(raw_call, auto_confirm, config)
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
            other => other.to_string(),
        },
        Err(_) => "unknown_tool".to_string(),
    }
}

fn summarize_tool_call_names(raw_calls: &[Value]) -> String {
    raw_calls
        .iter()
        .map(summarize_tool_call_activity)
        .collect::<Vec<_>>()
        .join(", ")
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

fn discovered_tool_names_from_search(raw_call: &Value) -> Vec<String> {
    let Ok(call) = parse_tool_call(raw_call) else {
        return Vec::new();
    };
    if call.name != "ToolSearch" && call.name != "tool_search" {
        return Vec::new();
    }
    let query = call.arguments["query"].as_str().unwrap_or_default();
    let max_results = call.arguments["max_results"].as_u64().unwrap_or(5) as usize;
    tool_search_matches(query, max_results)
        .into_iter()
        .map(|spec| spec.name.to_string())
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
        return resolve_session_id(paths, config, session_id);
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

async fn handle_repl(
    cli: &Cli,
    paths: &AppPaths,
    config: &AppConfig,
    secrets: &SecretsConfig,
    args: ReplArgs,
) -> AppResult<()> {
    if args.new_session && args.session.is_some() {
        return Err(AppError::new(
            EXIT_ARGS,
            "--new-session cannot be used together with --session",
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
    let mut first_turn = true;
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    loop {
        let prompt = read_repl_prompt(&stdin, &mut stdout, args.multiline)?;
        let trimmed = prompt.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "/exit" || trimmed == "/quit" {
            break;
        }
        let ask_args = AskArgs {
            prompt: Some(prompt),
            stdin: false,
            system: if first_turn {
                args.system.clone()
            } else {
                None
            },
            attachments: Vec::new(),
            images: Vec::new(),
            clipboard_image: false,
            session: Some(session_id.clone()),
            new_session: false,
            ephemeral: args.ephemeral,
            temp: args.temp,
            tools: args.tools,
            yes: args.yes,
            stream: args.stream,
            temperature: None,
            max_output_tokens: None,
            params: Vec::new(),
            timeout: None,
            raw_provider_response: false,
        };
        let use_tools = ask_args.tools || config.defaults.tools.unwrap_or(false);
        if use_tools {
            let result = execute_ask_with_tools(
                cli,
                paths,
                config,
                secrets,
                &ask_args,
                Some(OutputFormat::Text),
            )
            .await?;
            println!("{}", format_final_ask_output(config, &result, false)?);
        } else if args.stream {
            execute_ask_stream(
                cli,
                paths,
                config,
                secrets,
                &ask_args,
                Some(OutputFormat::Text),
            )
            .await?;
        } else {
            let result = execute_ask(
                cli,
                paths,
                config,
                secrets,
                &ask_args,
                Some(OutputFormat::Text),
            )
            .await?;
            println!("{}", render_markdown(&result.output.message.content, false));
        }
        first_turn = false;
    }
    Ok(())
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
    prompt: String,
    user_images: Vec<MessageImage>,
    session_id: String,
    session_preamble: Vec<ChatMessage>,
    request: ChatRequest,
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
        &prepared.prompt,
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
            if round == 0 && use_stream {
                print_status_bar(
                    &prepared.request.provider_id,
                    &prepared.request.model_id,
                    &prepared.session_id,
                )
                .map_err(|err| {
                    AppError::new(EXIT_ARGS, format!("failed to write stdout: {err}"))
                })?;
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

            eprintln!(
                "{DIM}[tools {}]{RESET} {}",
                round + 1,
                summarize_tool_call_names(&response.tool_calls)
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
                if config.tools.progressive_loading.unwrap_or(true) {
                    for tool_name in discovered_tool_names_from_search(raw_call) {
                        loaded_tool_names.insert(tool_name);
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
                    config,
                    &prepared.request.messages,
                );
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
            persist_partial_tool_turn(
                paths,
                config,
                args,
                &prepared.session_id,
                &prepared.session_preamble,
                &prepared.request.messages[turn_start_index..],
            )?;
            return Err(err);
        }
    };

    persist_tool_session(
        paths,
        config,
        args,
        &prepared.session_preamble,
        &prepared.session_id,
        &prepared.request.messages[turn_start_index..],
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
) -> AppResult<()> {
    let prepared = prepare_ask(cli, paths, config, secrets, args, output_override)?;
    let format = prepared.format.clone();
    let prompt = prepared.prompt.clone();
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
    if format == OutputFormat::Text {
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
        &prompt,
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
    if messages.is_empty() {
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
            (Some(cli), Some(file), _) => Some(format!("{cli}\n\n{file}")), // append
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
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: input.prompt.clone(),
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
        prompt: input.prompt,
        user_images: input.images,
        session_id,
        session_preamble,
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
    let images = read_image_inputs(&args.images, args.clipboard_image)?;
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
        let paths = AppPaths::from_overrides(Some(config_dir), Some(data_dir)).unwrap();
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
        }
    }

    #[test]
    fn requested_session_id_prefers_temp_over_new_session() {
        let session_id = requested_session_id(true, true, false).unwrap();
        assert!(session_id.starts_with("tmp_"));
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
            &first.prompt,
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
}

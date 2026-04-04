use crate::cli::{
    AskArgs, AuthCommand, AuthSetArgs, Cli, Commands, ConfigCommand, ModelCommand, ModelSetArgs,
    OutputFormat, ProviderCommand, ReplArgs, SessionCommand,
};
use crate::config::{
    AppConfig, AppPaths, ModelConfig, ProviderConfig, ProviderSecret, SecretsConfig, ensure_dirs,
    init_config_files, load_config, load_secrets, parse_headers, read_system_prompt,
    render_config_value, save_config, save_secrets, set_config_value, validate_config,
};
use crate::error::{
    AppError, AppResult, EXIT_ARGS, EXIT_AUTH, EXIT_CONFIG, EXIT_MODEL, EXIT_PROVIDER,
};
use crate::output::{AskOutput, AssistantMessage, render_ask_output};
use crate::provider::{
    ChatMessage, ChatRequest, ChatResponse, send_chat, stream_chat, test_provider,
};
use crate::render::{
    Spinner, StreamRenderer, print_status_bar, print_stream_phase, render_markdown,
};
use crate::session::{
    SessionEvent, SessionMessage, SessionResponse, append_events, clear_sessions, delete_session,
    gc_sessions, generate_session_id, generate_temp_session_id, is_temp_session,
    list_session_summaries, load_state, now_rfc3339, read_events, resolve_session_id,
    set_current_session, short_id,
};
use crate::tool::{execute_tool, parse_tool_call, tool_definitions};
use clap::CommandFactory;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};

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
            for event in &events {
                match event {
                    SessionEvent::Meta(meta) => println!(
                        "session_id={} created_at={}",
                        meta.session_id, meta.created_at
                    ),
                    SessionEvent::Message(message) => {
                        messages += 1;
                        println!(
                            "message role={} content={}",
                            message.role,
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
                }
            }
            println!("summary messages={} responses={}", messages, responses);
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

fn should_stream_tool_round(request: &ChatRequest, requested_stream: bool, round: usize) -> bool {
    if !requested_stream {
        return false;
    }
    if round == 0 {
        return true;
    }
    !(request.provider.kind == "openai_compatible"
        && request.model.remote_name.starts_with("claude-"))
}

fn should_buffer_tool_execution(request: &ChatRequest) -> bool {
    request.provider.kind == "openai_compatible" && request.model.remote_name.starts_with("claude-")
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
                tool_calls: None,
                tool_call_id: Some(result.tool_call_id),
                name: Some(call.name),
            },
            Err(err) => ChatMessage {
                role: "tool".to_string(),
                content: format!("tool execution error: {}", err.message),
                tool_calls: None,
                tool_call_id: Some(call.id),
                name: Some(call.name),
            },
        },
        Err(err) => ChatMessage {
            role: "tool".to_string(),
            content: format!("tool invocation error: {}", err.message),
            tool_calls: None,
            tool_call_id: Some(fallback_id),
            name: Some(fallback_name),
        },
    }
}

fn persist_partial_tool_turn(
    paths: &AppPaths,
    config: &AppConfig,
    args: &AskArgs,
    session_id: &str,
    messages: &[ChatMessage],
) -> AppResult<()> {
    let auto_save = config.defaults.auto_save_session.unwrap_or(true);
    if args.ephemeral || !auto_save {
        return Ok(());
    }

    let events = messages
        .iter()
        .filter(|message| !message.content.is_empty())
        .map(|message| {
            SessionEvent::Message(SessionMessage {
                role: message.role.clone(),
                content: message.content.clone(),
                created_at: now_rfc3339(),
            })
        })
        .collect::<Vec<_>>();

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
            println!("{}", render_markdown(&result.output.message.content, false));
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

#[derive(Clone)]
struct PreparedAsk {
    format: OutputFormat,
    prompt: String,
    session_id: String,
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
        &prepared.prompt,
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
    prepared.request.tools = tool_definitions();
    let buffer_tool_execution = should_buffer_tool_execution(&prepared.request);
    let max_rounds = config.tools.max_rounds.unwrap_or(20) as usize;
    let turn_start_index = prepared.request.messages.len().saturating_sub(1);
    let response_result: AppResult<ChatResponse> = async {
        let mut final_response: Option<ChatResponse> = None;

        for round in 0..max_rounds {
            let use_stream = !buffer_tool_execution
                && should_stream_tool_round(&prepared.request, args.stream, round);
            let mut stdout = io::stdout();
            let collapse = config.defaults.collapse_thinking.unwrap_or(false);
            let mut renderer = StreamRenderer::new(collapse);

            // Status bar on first round
            if round == 0 && use_stream {
                print_status_bar(
                    &prepared.request.provider_id,
                    &prepared.request.model_id,
                    &prepared.session_id,
                );
            }

            let response = if use_stream {
                let mut spinner = Some(Spinner::start("waiting"));
                stream_chat(prepared.request.clone(), |chunk| {
                    // Stop spinner on first content
                    if spinner.is_some() {
                        if let Some(mut s) = spinner.take() {
                            s.stop();
                        }
                    }
                    if !chunk.delta.is_empty() {
                        let rendered = renderer.push(&chunk.delta);
                        for phase in renderer.drain_phase_transitions() {
                            print_stream_phase(phase);
                        }
                        if !rendered.is_empty() {
                            write!(stdout, "{}", rendered).map_err(|err| {
                                AppError::new(EXIT_ARGS, format!("failed to write stdout: {err}"))
                            })?;
                            stdout.flush().map_err(|err| {
                                AppError::new(EXIT_ARGS, format!("failed to flush stdout: {err}"))
                            })?;
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
                        print_stream_phase(phase);
                    }
                    if !remaining.is_empty() {
                        write!(stdout, "{}", remaining).map_err(|err| {
                            AppError::new(EXIT_ARGS, format!("failed to write stdout: {err}"))
                        })?;
                    }
                    if !remaining.is_empty() && !remaining.ends_with('\n') {
                        writeln!(stdout).map_err(|err| {
                            AppError::new(EXIT_ARGS, format!("failed to write stdout: {err}"))
                        })?;
                    }
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
                    print_stream_phase(phase);
                }
                if !remaining.is_empty() {
                    write!(stdout, "{}", remaining).map_err(|err| {
                        AppError::new(EXIT_ARGS, format!("failed to write stdout: {err}"))
                    })?;
                }
                if !remaining.is_empty() && !remaining.ends_with('\n') {
                    writeln!(stdout).map_err(|err| {
                        AppError::new(EXIT_ARGS, format!("failed to write stdout: {err}"))
                    })?;
                }
            } else if args.stream && !response.content.is_empty() {
                let rendered = render_markdown(&response.content, collapse);
                if !rendered.is_empty() {
                    writeln!(stdout, "{rendered}").map_err(|err| {
                        AppError::new(EXIT_ARGS, format!("failed to write stdout: {err}"))
                    })?;
                }
            }

            eprintln!(
                "[round {}] model requested {} tool call(s)",
                round + 1,
                response.tool_calls.len()
            );

            prepared.request.messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: response.content.clone(),
                tool_calls: Some(response.tool_calls.clone()),
                tool_call_id: None,
                name: None,
            });

            for raw_call in &response.tool_calls {
                prepared
                    .request
                    .messages
                    .push(execute_tool_as_message(raw_call, args.yes, config));
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
                &prepared.request.messages[turn_start_index..],
            )?;
            return Err(err);
        }
    };

    persist_session(
        paths,
        config,
        args,
        &prepared.prompt,
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
        );
    }

    let collapse = config.defaults.collapse_thinking.unwrap_or(false);
    let mut renderer = StreamRenderer::new(collapse);
    let mut spinner = if format == OutputFormat::Text {
        Some(Spinner::start("waiting"))
    } else {
        None
    };

    let response = match stream_chat(prepared.request.clone(), |chunk| {
        // Stop spinner on first content
        if spinner.is_some() {
            if let Some(mut s) = spinner.take() {
                s.stop();
            }
        }
        match format {
            OutputFormat::Text => {
                if !chunk.delta.is_empty() {
                    let rendered = renderer.push(&chunk.delta);
                    for phase in renderer.drain_phase_transitions() {
                        print_stream_phase(phase);
                    }
                    if !rendered.is_empty() {
                        write!(stdout, "{}", rendered).map_err(|err| {
                            AppError::new(EXIT_ARGS, format!("failed to write stdout: {err}"))
                        })?;
                        stdout.flush().map_err(|err| {
                            AppError::new(EXIT_ARGS, format!("failed to flush stdout: {err}"))
                        })?;
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
                print_stream_phase(phase);
            }
            if !remaining.is_empty() {
                write!(stdout, "{}", remaining).map_err(|err| {
                    AppError::new(EXIT_ARGS, format!("failed to write stdout: {err}"))
                })?;
            }
            if !remaining.is_empty() && !remaining.ends_with('\n') {
                writeln!(stdout).map_err(|err| {
                    AppError::new(EXIT_ARGS, format!("failed to write stdout: {err}"))
                })?;
            }
            stdout.flush().map_err(|err| {
                AppError::new(EXIT_ARGS, format!("failed to flush stdout: {err}"))
            })?;
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

    persist_session(paths, config, args, &prompt, &session_id, &response)?;
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

    let prompt = build_prompt(args)?;
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
    if let Ok(history) = read_events(paths, config, &session_id) {
        for event in history {
            if let SessionEvent::Message(message) = event {
                if message.content.is_empty() {
                    continue;
                }
                messages.push(ChatMessage {
                    role: message.role,
                    content: message.content,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
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
            (Some(cli), Some(file), "override") => Some(file),
            (Some(cli), Some(file), _) => Some(format!("{cli}\n\n{file}")), // append
            (Some(cli), None, _) => Some(cli),
            (None, Some(file), _) => Some(file),
            (None, None, _) => None,
        };

        if let Some(system) = final_system {
            messages.push(ChatMessage {
                role: "system".to_string(),
                content: system,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
        }
    }
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: prompt.clone(),
        tool_calls: None,
        tool_call_id: None,
        name: None,
    });

    let temperature = args.temperature.or(model.temperature);
    let max_output_tokens = args.max_output_tokens.or(model.max_output_tokens);
    let api_key = resolve_api_key(&provider_id, provider, secrets)?;
    let params = parse_params(&args.params)?;

    Ok(PreparedAsk {
        format,
        prompt,
        session_id,
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
    prompt: &str,
    session_id: &str,
    response: &ChatResponse,
) -> AppResult<()> {
    let auto_save = config.defaults.auto_save_session.unwrap_or(true);
    if args.ephemeral || !auto_save {
        return Ok(());
    }

    let mut events = vec![SessionEvent::Message(SessionMessage {
        role: "user".to_string(),
        content: prompt.to_string(),
        created_at: now_rfc3339(),
    })];
    if !response.content.is_empty() {
        events.push(SessionEvent::Message(SessionMessage {
            role: "assistant".to_string(),
            content: response.content.clone(),
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

fn build_prompt(args: &AskArgs) -> AppResult<String> {
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
    if parts.is_empty() {
        return Err(AppError::new(
            EXIT_ARGS,
            "chat ask requires PROMPT, --stdin, or --attach",
        ));
    }
    Ok(parts.join("\n\n"))
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
    fn tool_execution_is_buffered_for_openai_claude_models() {
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
            },
            api_key: String::new(),
            messages: Vec::new(),
            temperature: None,
            max_output_tokens: None,
            params: BTreeMap::new(),
            timeout_secs: None,
            tools: Vec::new(),
        };
        assert!(should_buffer_tool_execution(&request));
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
}

use crate::context::ContextStatusMode;
use crate::media::MessageImage;
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Parser, Debug, Clone)]
#[command(name = "chat", version, about = "Configurable LLM chat CLI")]
pub struct Cli {
    #[arg(short = 'p', long, global = true)]
    pub provider: Option<String>,

    #[arg(short = 'm', long, global = true)]
    pub model: Option<String>,

    #[arg(long, default_value = "auto", global = true)]
    pub mode: String,

    #[arg(long, global = true)]
    pub output: Option<OutputFormat>,

    #[arg(long, global = true)]
    pub config_dir: Option<PathBuf>,

    #[arg(long, global = true)]
    pub data_dir: Option<PathBuf>,

    #[arg(long, global = true)]
    pub no_color: bool,

    #[arg(short = 'v', long, global = true)]
    pub verbose: bool,

    #[arg(short = 'q', long, global = true)]
    pub quiet: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Commands {
    Ask(AskArgs),
    Repl(ReplArgs),
    Mcp(McpArgs),
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Display the last response's thinking content
    Thinking,
    Doctor,
    Completion {
        shell: clap_complete::Shell,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, ValueEnum, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    #[default]
    Line,
    Text,
    Json,
    Ndjson,
}

#[derive(Args, Debug, Clone)]
pub struct AskArgs {
    pub prompt: Option<String>,

    #[arg(long)]
    pub stdin: bool,

    #[arg(short = 's', long)]
    pub system: Option<String>,

    #[arg(short = 'a', long = "attach")]
    pub attachments: Vec<PathBuf>,

    #[arg(short = 'i', long = "image")]
    pub images: Vec<PathBuf>,

    #[arg(short = 'I', long)]
    pub clipboard_image: bool,

    #[arg(skip)]
    pub preloaded_images: Vec<MessageImage>,

    #[arg(long)]
    pub session: Option<String>,

    #[arg(long)]
    pub new_session: bool,

    #[arg(long)]
    pub ephemeral: bool,

    #[arg(long)]
    pub temp: bool,

    #[arg(long)]
    pub tools: bool,

    #[arg(long, short = 'y')]
    pub yes: bool,

    #[arg(long)]
    pub stream: bool,

    #[arg(long)]
    pub temperature: Option<f64>,

    #[arg(long)]
    pub max_output_tokens: Option<u32>,

    #[arg(long = "param")]
    pub params: Vec<String>,

    #[arg(long)]
    pub timeout: Option<u64>,

    #[arg(long)]
    pub raw_provider_response: bool,

    #[arg(long, value_enum)]
    pub context_status: Option<ContextStatusMode>,
}

#[derive(Args, Debug, Clone)]
pub struct ReplArgs {
    #[arg(long)]
    pub session: Option<String>,

    #[arg(long)]
    pub new_session: bool,

    #[arg(long)]
    pub ephemeral: bool,

    #[arg(long)]
    pub temp: bool,

    #[arg(long)]
    pub tools: bool,

    #[arg(long, short = 'y')]
    pub yes: bool,

    #[arg(long)]
    pub system: Option<String>,

    #[arg(long)]
    pub multiline: bool,

    #[arg(long)]
    pub stream: bool,

    #[arg(long)]
    pub no_stream: bool,

    #[arg(long, value_enum)]
    pub context_status: Option<ContextStatusMode>,
}

#[derive(Args, Debug, Clone)]
pub struct McpArgs {
    #[command(subcommand)]
    pub command: Option<McpCommand>,

    #[arg(long)]
    pub server: Option<String>,

    #[arg(long)]
    pub no_cache: bool,

    #[arg(long)]
    pub verbose: bool,
}

#[derive(Subcommand, Debug, Clone)]
pub enum McpCommand {
    Auth(McpAuthArgs),
    Start(McpStartArgs),
    Stop,
    Status,
}

#[derive(Args, Debug, Clone)]
pub struct McpAuthArgs {
    #[arg(long)]
    pub server: Option<String>,

    #[arg(long)]
    pub no_cache: bool,

    #[arg(long)]
    pub verbose: bool,
}

#[derive(Args, Debug, Clone)]
pub struct McpStartArgs {
    #[arg(long)]
    pub server: Option<String>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum SessionCommand {
    List,
    Current,
    Switch {
        id: String,
    },
    New {
        #[arg(long)]
        temp: bool,
    },
    Show {
        id: Option<String>,
    },
    Render {
        id: Option<String>,
        #[arg(long, conflicts_with = "all")]
        last: Option<usize>,
        #[arg(long, conflicts_with = "last")]
        all: bool,
    },
    Export {
        id: String,
    },
    Delete {
        id: String,
    },
    Clear {
        #[arg(long)]
        all: bool,
    },
    Gc,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ConfigCommand {
    Init,
    Path,
    Show,
    Get {
        key: String,
    },
    Set {
        key: String,
        value: String,
    },
    Validate,
    Provider {
        #[command(subcommand)]
        command: ProviderCommand,
    },
    Model {
        #[command(subcommand)]
        command: ModelCommand,
    },
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum ProviderCommand {
    Set(ProviderSetArgs),
    List,
    Get { id: String },
    Remove { id: String },
    Test { id: String },
}

#[derive(Args, Debug, Clone)]
pub struct ProviderSetArgs {
    pub id: String,

    #[arg(long)]
    pub kind: String,

    #[arg(long)]
    pub base_url: Option<String>,

    #[arg(long)]
    pub api_key_env: Option<String>,

    #[arg(long = "header")]
    pub headers: Vec<String>,

    #[arg(long)]
    pub org: Option<String>,

    #[arg(long)]
    pub project: Option<String>,

    #[arg(long)]
    pub default_model: Option<String>,

    #[arg(long)]
    pub timeout: Option<u64>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ModelCommand {
    Set(ModelSetArgs),
    List {
        #[arg(long)]
        provider: Option<String>,
    },
    Get {
        id: String,
    },
    Use {
        target: String,
    },
    Remove {
        id: String,
    },
}

#[derive(Args, Debug, Clone)]
pub struct ModelSetArgs {
    pub id: String,

    #[arg(long)]
    pub provider: String,

    #[arg(long)]
    pub remote_name: String,

    #[arg(long)]
    pub display_name: Option<String>,

    #[arg(long)]
    pub context_window: Option<u64>,

    #[arg(long)]
    pub max_output_tokens: Option<u32>,

    #[arg(long = "capability")]
    pub capabilities: Vec<String>,

    #[arg(long)]
    pub temperature: Option<f64>,

    #[arg(long)]
    pub reasoning_effort: Option<String>,

    #[arg(long)]
    pub patch_system_to_user: bool,
}

#[derive(Subcommand, Debug, Clone)]
pub enum AuthCommand {
    Set(AuthSetArgs),
    Status { provider_id: Option<String> },
    Remove { provider_id: String },
}

#[derive(Args, Debug, Clone)]
pub struct AuthSetArgs {
    pub provider_id: String,

    #[arg(long)]
    pub stdin: bool,

    #[arg(long)]
    pub value: Option<String>,

    #[arg(long)]
    pub env: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{Cli, Commands};
    use clap::Parser;
    use std::path::PathBuf;

    #[test]
    fn ask_parses_image_short_flags() {
        let cli = Cli::try_parse_from([
            "chat",
            "ask",
            "-i",
            "first.png",
            "-i",
            "second.jpg",
            "-I",
            "describe",
        ])
        .expect("cli should parse image short flags");

        match cli.command {
            Commands::Ask(args) => {
                assert_eq!(
                    args.images,
                    vec![PathBuf::from("first.png"), PathBuf::from("second.jpg")]
                );
                assert!(args.clipboard_image);
                assert_eq!(args.prompt.as_deref(), Some("describe"));
            }
            other => panic!("expected ask command, got {other:?}"),
        }
    }

    #[test]
    fn session_render_parses_optional_id_and_last() {
        let cli = Cli::try_parse_from(["chat", "session", "render", "sess_abc", "--last", "3"])
            .expect("cli should parse session render");

        match cli.command {
            Commands::Session { command } => match command {
                super::SessionCommand::Render { id, last, all } => {
                    assert_eq!(id.as_deref(), Some("sess_abc"));
                    assert_eq!(last, Some(3));
                    assert!(!all);
                }
                other => panic!("expected session render command, got {other:?}"),
            },
            other => panic!("expected session command, got {other:?}"),
        }
    }

    #[test]
    fn session_render_parses_all_flag() {
        let cli = Cli::try_parse_from(["chat", "session", "render", "--all"])
            .expect("cli should parse session render --all");

        match cli.command {
            Commands::Session { command } => match command {
                super::SessionCommand::Render { id, last, all } => {
                    assert_eq!(id, None);
                    assert_eq!(last, None);
                    assert!(all);
                }
                other => panic!("expected session render command, got {other:?}"),
            },
            other => panic!("expected session command, got {other:?}"),
        }
    }

    #[test]
    fn session_show_parses_optional_id() {
        let cli = Cli::try_parse_from(["chat", "session", "show"])
            .expect("cli should parse session show without id");
        match cli.command {
            Commands::Session { command } => match command {
                super::SessionCommand::Show { id } => assert_eq!(id, None),
                other => panic!("expected session show command, got {other:?}"),
            },
            other => panic!("expected session command, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["chat", "session", "show", "sess_abc"])
            .expect("cli should parse session show with id");
        match cli.command {
            Commands::Session { command } => match command {
                super::SessionCommand::Show { id } => assert_eq!(id.as_deref(), Some("sess_abc")),
                other => panic!("expected session show command, got {other:?}"),
            },
            other => panic!("expected session command, got {other:?}"),
        }
    }

    #[test]
    fn mcp_parses_legacy_auth_flags() {
        let cli = Cli::try_parse_from(["chat", "mcp", "--server", "ace", "--verbose"])
            .expect("cli should parse mcp command");
        match cli.command {
            Commands::Mcp(args) => {
                assert!(args.command.is_none());
                assert_eq!(args.server.as_deref(), Some("ace"));
                assert!(args.verbose);
                assert!(!args.no_cache);
            }
            other => panic!("expected mcp command, got {other:?}"),
        }
    }

    #[test]
    fn mcp_start_parses_server() {
        let cli = Cli::try_parse_from(["chat", "mcp", "start", "--server", "ace"])
            .expect("cli should parse mcp start command");
        match cli.command {
            Commands::Mcp(args) => match args.command {
                Some(super::McpCommand::Start(serve)) => {
                    assert_eq!(serve.server.as_deref(), Some("ace"));
                }
                other => panic!("expected mcp start command, got {other:?}"),
            },
            other => panic!("expected mcp command, got {other:?}"),
        }
    }

    #[test]
    fn mcp_stop_parses() {
        let cli = Cli::try_parse_from(["chat", "mcp", "stop"])
            .expect("cli should parse mcp stop command");
        match cli.command {
            Commands::Mcp(args) => match args.command {
                Some(super::McpCommand::Stop) => {}
                other => panic!("expected mcp stop command, got {other:?}"),
            },
            other => panic!("expected mcp command, got {other:?}"),
        }
    }

    #[test]
    fn mcp_status_parses() {
        let cli = Cli::try_parse_from(["chat", "mcp", "status"])
            .expect("cli should parse mcp status command");
        match cli.command {
            Commands::Mcp(args) => match args.command {
                Some(super::McpCommand::Status) => {}
                other => panic!("expected mcp status command, got {other:?}"),
            },
            other => panic!("expected mcp command, got {other:?}"),
        }
    }
}

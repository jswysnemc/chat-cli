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
}

#[derive(Subcommand, Debug, Clone)]
pub enum SessionCommand {
    List,
    Current,
    Switch { id: String },
    New {
        #[arg(long)]
        temp: bool,
    },
    Show { id: String },
    Export { id: String },
    Delete { id: String },
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

# chat-cli

[English](./README.md) | [中文](./README_zh.md)

A configurable LLM chat CLI written in Rust, supporting multiple providers, session management, and machine-readable output.

## Features

- **Multi-provider support**: OpenAI-compatible, Anthropic, Ollama
- **Interactive REPL**: Conversational chat mode with session history
- **Session persistence**: Automatic session saving with JSONL format
- **Machine-friendly output**: JSON, NDJSON, and line-based output for scripting
- **Tool calling**: Support for function calling with confirmation
- **Configuration management**: TOML-based config with provider, model, and auth management

## Installation

```bash
cargo build --release
cargo install --path .
```

## Quick Start

```bash
# Interactive REPL mode
chat repl

# Ask a one-off question
chat ask "Explain this error"

# Use with pipe input
git diff | chat ask --stdin -P review

# List sessions
chat session list

# Configure a provider
chat config provider set openai --kind openai_compatible --base-url https://api.openai.com/v1
```

## Commands

### `chat ask [PROMPT]`

Send a single prompt to the LLM.

```bash
chat ask "Your question here"
chat ask --stdin "Explain this"  # reads from stdin
chat ask --session <id>          # continue existing session
chat ask --new-session           # create new session
chat ask --output json           # JSON output mode
chat ask --stream                # streaming output
chat ask --tools                 # enable tool calling
```

### `chat repl`

Start an interactive REPL session.

```bash
chat repl
chat repl --session <id>         # continue session
chat repl --system <prompt>      # system prompt
chat repl --multiline            # enable multiline input
```

### `chat session`

Manage chat sessions.

```bash
chat session list                # list all sessions
chat session show <id>           # show session details
chat session export <id>          # export session to JSON
chat session delete <id>          # delete a session
chat session gc                  # garbage collect orphaned data
```

### `chat config`

Manage configuration.

```bash
chat config init                 # initialize config directory
chat config show                 # show full config
chat config provider list        # list providers
chat config provider set <id> --kind <type> --base-url <url>
chat config model list           # list models
chat config auth set <provider>  # set API key
chat config doctor               # diagnose configuration issues
```

## Configuration

Default config locations (XDG compliant):
- Config: `~/.config/chat-cli/config.toml`
- Secrets: `~/.config/chat-cli/secrets.toml`
- Sessions: `~/.local/share/chat-cli/sessions/`

### Example Config

```toml
[provider.openai]
kind = "openai_compatible"
base_url = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"
default_model = "gpt-4o"

[provider.anthropic]
kind = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"
default_model = "claude-sonnet-4-20250514"

[provider.ollama]
kind = "ollama"
base_url = "http://localhost:11434"
default_model = "llama3"

[model.gpt-4o]
provider = "openai"
remote_name = "gpt-4o"
display_name = "GPT-4o"
context_window = 128000

[profile.default]
provider = "openai"
model = "gpt-4o"
temperature = 0.7
```

## Output Formats

| Format   | Description                              |
|----------|------------------------------------------|
| `line`   | Single-line summary (default)            |
| `text`   | Full text output                         |
| `json`   | JSON object with metadata                |
| `ndjson` | Newline-delimited JSON for streaming     |

## Session Management

Sessions are automatically created with a ULID identifier and persisted to `sessions/<session_id>.jsonl`. Each message includes:

- `id`: ULID
- `role`: user/assistant
- `content`: message text
- `timestamp`: ISO 8601

## License

MIT

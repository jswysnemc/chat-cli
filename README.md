# chat-cli

[English](./README.md) | [中文](./README_zh.md)

A configurable LLM chat CLI written in Rust, supporting multiple providers, session management, and machine-readable output.

## Features

- **Multi-provider support**: OpenAI-compatible, Anthropic, Ollama
- **Interactive REPL**: Conversational chat mode with session history
- **Session persistence**: Automatic session saving with JSONL format
- **Machine-friendly output**: JSON, NDJSON, and line-based output for scripting
- **Tool calling**: Support for function calling with confirmation
- **Tool transcript persistence**: Sessions retain assistant `tool_calls` and tool results for replay and debugging
- **Automatic audit subagent**: Tool-using turns can be reviewed by a second model with configurable enablement, model selection, and prompt files
- **Configurable tool exposure**: Use progressive loading to expose `ToolSearch` first, or disable it to expose the full tool metadata upfront
- **Configuration management**: TOML-based config with provider, model, and auth management

## Architecture

The project is organized into six layers. Data flows from the CLI into the core, then out to providers, tools, storage, and I/O.

### 1. CLI Layer (`clap`)

Entry points implemented as subcommands:

| Command | Purpose |
|---------|---------|
| `chat ask` | Single-shot prompt |
| `chat repl` | Interactive session |
| `chat session` | Session management |
| `chat config` | Configuration management |
| `chat mcp` | MCP operations |
| `chat doctor` | Environment diagnostics |
| `chat thinking` | Display last reasoning content |

### 2. Core Layer (`app`)

- **Command Handlers**: Parse arguments and dispatch to the correct workflow.
- **Tool Calling Loop**: Orchestrates multi-round tool execution; calls built-in tools or MCP tools and feeds results back to the model.
- **Audit Subagent**: Reviews dangerous tool operations (e.g., `edit`, `bash`) before execution.

### 3. Provider Layer

Unified interface to upstream LLM APIs:

- **OpenAI Compatible** — any OpenAI-compatible endpoint
- **Anthropic** — native Anthropic API
- **Ollama** — local Ollama server

Communication uses HTTP / SSE.

### 4. Tool Layer

**Built-in Tools**

| Tool | Function |
|------|----------|
| `Read` / `Edit` / `Bash` | Filesystem and shell operations |
| `Grep` | Code search via `rg` |
| `Glob` | File pattern matching |
| `WebFetch` | HTTP content retrieval |
| `Status` | Workspace and environment status |
| `TodoWrite` | Task list management |
| `SkillRead` / `SkillsList` | Skill loading |
| `ToolSearch` | Progressive tool discovery |

**MCP Integration**

- MCP Daemon manages external MCP servers
- MCP Tools are injected into the tool loop when enabled

### 5. Storage Layer

| File / Directory | Purpose |
|------------------|---------|
| `config.toml` | User configuration |
| `secrets.toml` | API keys and sensitive data |
| `sessions/*.jsonl` | Persisted conversation history |
| `state.toml` | Runtime state |
| `prompts/*.md` | Custom prompt templates |

### 6. I/O & Rendering Layer

- **Markdown Renderer** — pretty-print responses in the terminal
- **Stream Renderer** — real-time token streaming
- **Output Formats** — `line`, `text`, `json`, `ndjson`
- **Media Input** — image files and clipboard images
- **Context Status Injection** — workspace context appended to prompts

## Installation

```bash
cargo build --release
cargo install --path .
```

Prebuilt release archives are published for `linux-x86_64`, `macos-x86_64`, `macos-arm64`, and `windows-x86_64`.

## Platform Notes

Recommended local dependencies:

- `rg` / ripgrep: required by the built-in `Grep` tool on Linux, macOS, and Windows
- Nerd Font: optional, but improves REPL rendering in terminals with weak symbol fallback
- `fzf`: optional for shell helpers and quick session/model switching
- `uv`: optional but useful when MCP servers or helper scripts expect it

Clipboard backends:

- Linux: `wl-paste` or `xclip`
- macOS: `pbpaste` for text and `pngpaste` for images
- Windows: built-in PowerShell clipboard APIs

## Quick Start

```bash
# Interactive REPL mode
chat repl

# Ask a one-off question
chat ask "Explain this error"

# Use with pipe input
git diff | chat ask "Review this diff"
tool --help | chat ask

# List sessions
chat session list

# Configure a provider
chat config provider set openai --kind openai_compatible --base-url https://api.openai.com/v1
```

## Built-in Tool Notes

- `Grep` calls the external `rg` binary directly. On Windows this works as long as `rg.exe` is on `PATH`. `glob`/`include` is passed to ripgrep via `--glob`, and `output_mode` supports `content`, `files_with_matches`, and `count`.
- `Glob` is implemented in Rust with `walkdir` plus glob-to-regex conversion. It does not call `rg`, and patterns are matched relative to `path`. For example, `pattern="src/*.rs"` with `path="."` matches `src/main.rs`.
- `Status` is cross-platform and reports the current workspace, git context, OS, shell tool availability, and common developer dependencies, including whether `rg` is installed.

## Shell shortcuts

The alias examples below are for `zsh` and `bash`. If you use the tool mostly from a POSIX shell, you can add these shortcuts to `~/.zshrc` or `~/.bashrc`:

```bash
alias ca='chat ask --stream'
alias cn='chat ask --stream --new-session'
alias ct='chat ask --stream --new-session --temp'
```

Meaning:

- `ca`: streaming ask against the current session flow
- `cn`: streaming ask with a forced new session
- `ct`: streaming ask with a new temporary session

For PowerShell, add similar functions to `$PROFILE`:

```powershell
function ca { chat ask --stream @args }
function cn { chat ask --stream --new-session @args }
function ct { chat ask --stream --new-session --temp @args }
```

The `fzf` examples below assume a POSIX shell:

For model switching with `fzf`, use a shell function rather than an alias:

```bash
ccm() {
  chat config model use "$(chat --no-color config model list | fzf)"
}
```

Why:

- this kind of interactive command substitution is better expressed as a function
- `chat config model list` prints `provider/remote_name`, which can be passed directly to `chat config model use <target>`
- `--no-color` avoids ANSI color codes interfering with `fzf`

If you also want fast session switching through `fzf`, add this function too:

```bash
ccs() {
  chat session switch "$(chat --no-color session list | fzf | awk '{print ($1=="*" ? $2 : $1)}')"
}
```

Reload your shell config after editing:

```bash
source ~/.zshrc
# or
source ~/.bashrc
```

PowerShell:

```powershell
. $PROFILE
```

## Commands

### `chat ask [PROMPT]`

Send a single prompt to the LLM.

```bash
chat ask "Your question here"
chat ask --stdin "Explain this"  # explicitly reads from stdin
tool --help | chat ask           # piped stdin is read automatically
chat ask --session <id>          # continue existing session
chat ask --new-session           # create new session
chat ask --output json           # JSON output mode
chat ask --stream                # streaming output
chat ask --tools                 # enable tool calling
chat ask --context-status always "Inspect this repo"
chat ask --context-status latest "Only inject status for this turn"
chat ask --context-status system-once --system "Be concise" "Start a new session"
chat --context-window 128000 ask "Use a runtime context hint"
chat --reasoning-effort high ask "Think harder for this turn"
chat ask -i screenshot.png "Describe this UI"
chat ask -I "What's in the clipboard image?"
```

### `chat repl`

Start an interactive REPL session.

```bash
chat repl
chat repl --session <id>         # continue session
chat repl --system <prompt>      # system prompt
chat repl --multiline            # enable multiline input
chat repl --context-status system-once
chat --context-window 128000 repl
chat --reasoning-effort medium repl
```

Inside REPL, runtime model tuning is also available through slash commands: `/model`, `/context`, `/reasoning`, `/status`.

### `chat session`

Manage chat sessions.

```bash
chat session list                # list all sessions
chat session show <id>           # show session details
chat session render             # render the latest turn from the current session
chat session render <id> --last 3 # render the latest 3 turns from a session
chat session render <id> --all  # render the whole session
chat session export <id>          # export session to JSON
chat session delete <id>          # delete a session
chat session gc                  # garbage collect orphaned data
```

### `chat config`

Manage configuration, providers, models, and auth state.

```bash
chat config init                 # initialize config directory
chat config path                 # print config/data/cache paths
chat config show                 # show full config
chat config get defaults.model   # read one config value
chat config get defaults.collapse_thinking
chat config get defaults.context_status
chat config get defaults.context_window
chat config get defaults.reasoning_effort
chat config set defaults.collapse_thinking true
chat config set defaults.context_status system-once
chat config set defaults.context_window 128000
chat config set defaults.reasoning_effort high
chat config set audit.enabled true
chat config validate             # validate references and defaults

chat config provider list
chat config provider get deepseek
chat config provider set <id> --kind <type> --base-url <url>
chat config provider test <id>
chat config provider remove <id>

chat config model list --provider deepseek
chat config model get deepseek-reasoner-search
chat config model use minimax/minimax-m2-7
chat config model remove <id>

chat config auth set deepseek --env DEEPSEEK_API_KEY
chat config auth status
chat config auth remove deepseek
```

### `chat thinking`

Display the last saved thinking content from a local response.

```bash
chat thinking
```

## Configuration

Default config locations depend on the platform. Run `chat config path` to inspect the active values on the current machine.

- Linux: `~/.config/chat-cli/config.toml`, `~/.config/chat-cli/secrets.toml`, `~/.local/share/chat-cli/sessions/`
- macOS: `~/Library/Application Support/chat-cli/config.toml`, `~/Library/Application Support/chat-cli/secrets.toml`, `~/Library/Application Support/chat-cli/sessions/`
- Windows: `%APPDATA%\chat-cli\config.toml`, `%APPDATA%\chat-cli\secrets.toml`, `%LOCALAPPDATA%\chat-cli\sessions\`

Run `chat config init` first. The generated default config enables tool calling by default with `defaults.tools = true`, disables progressive tool loading by default with `tools.progressive_loading = false`, and keeps MCP disabled by default with `tools.mcp = false`.

## API Setup Guide

The full setup consists of 4 parts:

1. create a provider entry
2. create one or more local model entries under that provider
3. configure authentication
4. select the default model

### Minimal setup flow

OpenAI-compatible example:

```bash
chat config init

chat config provider set openai \
  --kind openai_compatible \
  --base-url https://api.openai.com/v1

chat config model set gpt-4.1 \
  --provider openai \
  --remote-name gpt-4.1 \
  --capability chat

chat config auth set openai --env OPENAI_API_KEY
export OPENAI_API_KEY="<your-key>"

chat config model use gpt-4.1
chat config provider test openai
chat ask "hello"
```

Anthropic example:

```bash
chat config provider set anthropic \
  --kind anthropic

chat config model set claude-sonnet-4-7 \
  --provider anthropic \
  --remote-name claude-sonnet-4-7 \
  --capability chat \
  --capability reasoning

chat config auth set anthropic --env ANTHROPIC_API_KEY
export ANTHROPIC_API_KEY="<your-key>"

chat config model use claude-sonnet-4-7
chat config provider test anthropic
```

Local Ollama example:

```bash
chat config provider set ollama --kind ollama

chat config model set qwen2.5 \
  --provider ollama \
  --remote-name qwen2.5 \
  --capability chat

chat config model use qwen2.5
chat config provider test ollama
```

### What each config item means

#### Provider

A provider defines how to reach the upstream API.

- `kind`: `openai_compatible`, `anthropic`, or `ollama`
- `base_url`: required for `openai_compatible`; optional for `anthropic` and `ollama`
- `api_key_env`: optional environment variable name for reading the API key
- `headers`: extra static headers in `KEY=VALUE` form
- `org`, `project`: OpenAI-compatible headers mapped to `OpenAI-Organization` and `OpenAI-Project`
- `default_model`: provider-level fallback model id
- `timeout`: total request timeout in seconds; `0` means disabled

Base URL behavior:

- `openai_compatible`: `base_url` is required
- `anthropic`: defaults to `https://api.anthropic.com/v1`
- `ollama`: defaults to `http://localhost:11434/api`

#### Model

A model entry is a local alias plus runtime metadata.

- `id`: local model id used by this CLI
- `provider`: the provider id it belongs to
- `remote_name`: the exact upstream model name sent to the API
- `capabilities`: feature declarations such as `chat`, `reasoning`, `vision`
- `reasoning_effort`: optional reasoning hint for compatible models
- `patch_system_to_user`: compatibility patch for some OpenAI-compatible backends

Selection priority at runtime:

1. `--model`
2. `defaults.model`
3. `provider.default_model`

If the selected model belongs to another provider, the request is rejected.

#### Authentication

There are 2 supported ways to provide API keys:

Use an environment variable:

```bash
chat config auth set openai --env OPENAI_API_KEY
export OPENAI_API_KEY="<your-key>"
```

Store the key in `secrets.toml`:

```bash
chat config auth set openai --value "<your-key>"
```

Resolution order:

1. `provider.api_key_env`
2. `secrets.toml`

Notes:

- If both exist, the environment variable wins
- `ollama` can work without an API key
- `openai_compatible` and `anthropic` usually require an API key
- Do not put the real key into `config.toml`

### Common commands after setup

```bash
chat config show
chat config validate
chat config provider list
chat config model list
chat config auth status
chat repl
chat ask "Explain this error"
```

### Important notes

- `provider set` only creates the upstream endpoint entry; it does not create models automatically
- `model set --remote-name` must match the upstream model name exactly
- To send images, the model must include capability `vision`; otherwise image requests are rejected
- `-i/--image` reads image files from paths you provide and sends those files directly to the model
- `-I/--clipboard-image` reads the current clipboard image first, then sends that clipboard image to the model; this is a different input path from `-i`, even though both end up as image input for vision-capable models. Linux uses `wl-paste` or `xclip`, macOS uses `pngpaste`, and Windows uses PowerShell clipboard APIs
- `chat config provider test <id>` only checks connectivity and basic authentication; it does not prove every model under that provider is valid
- Some OpenAI-compatible gateways expose `/models`, some only allow `/chat/completions`; the health check already falls back when `/models` returns 404
- `defaults.tools = true` means tool calling is enabled by default for `ask` and `repl`; if the current model/provider does not support tools well, turn it off with `chat config set defaults.tools false`
- `tools.progressive_loading = false` is now the default. Testing showed that the overall experience is better when progressive loading is disabled. Turning it on may save some tokens because only `ToolSearch` is exposed first, but weaker models may fail to infer what tools are available or what they should do next
- If you want token savings more than tool discoverability, enable it with `chat config set tools.progressive_loading true`
- `tools.mcp = false` is now the default. When it stays off, MCP is disabled end to end: no automatic daemon startup, no MCP tool injection, and no MCP tool execution. Enable it with `chat config set tools.mcp true` when you want MCP behavior
- `timeout = 0` means there is no total request timeout; long reasoning requests may therefore wait for a long time
- `chat config model use <target>` accepts either a local model id like `gpt-4.1` or a `provider/model` target like `openai/gpt-4.1`
- `chat config auth status` shows whether an env var name is configured, whether that env var is currently present, and whether a file-based secret exists

API keys should stay in `secrets.toml` or environment variables. The example below mirrors the current local setup, but private hosts and user-specific paths are intentionally redacted or normalized.

### Sanitized Current Config

```toml
[defaults]
provider = "deepseek"                           # default provider id; must exist in [providers.*]
model = "deepseek-reasoner-search"              # default local model id; must exist in [models.*]
mode = "auto"                                   # current request mode
output = "line"                                 # line | text | json | ndjson
auto_create_session = true                      # create sessions automatically when needed
auto_save_session = true                        # persist turns automatically
session_id_kind = "ulid"                        # id format for new sessions
tools = true                                    # enable tool calling by default
system_prompt_file = "~/.config/chat-cli/system.md" # external system prompt file
system_prompt_mode = "append"                   # append | override
collapse_thinking = false                       # collapse <think> blocks in rendered output
context_status = "off"                          # off | always | latest | system-once
# context_window = 128000                       # optional fallback context hint when model config omits it
# reasoning_effort = "auto"                     # optional runtime reasoning effort; auto disables explicit hint

[session]
store_format = "jsonl"                          # on-disk session format
# dir = "~/.local/share/chat-cli/sessions"      # optional custom session directory

[tools]
max_rounds = 20                                 # max tool-calling rounds per turn
progressive_loading = false                    # false: expose all tool schemas upfront; true: expose ToolSearch first to save some tokens
mcp = false                                    # false: disable MCP end to end; true: enable MCP daemon startup, tool injection, and tool execution

[audit]
enabled = true                                  # enable the dangerous-tool audit subagent
model = "minimax-m2-7"                          # local model id from [models.*]
default_prompt_file = "/home/example/.config/chat-cli/prompts/audit-default.md"
bash_prompt_file = "/home/example/.config/chat-cli/prompts/audit-bash.md"
edit_prompt_file = "/home/example/.config/chat-cli/prompts/audit-edit.md"

[skills]
paths = ["~/.claude/skills"]                    # skill search roots

[providers.deepseek]
kind = "openai_compatible"                      # openai_compatible | anthropic | ollama
base_url = "https://<private-gateway>/v1"       # sanitized endpoint
api_key_env = "DEEPSEEK_API_KEY"                # env var name, not the secret value
# headers = { "X-Example" = "value" }           # optional extra request headers
# org = "example-org"                           # optional organization id
# project = "example-project"                   # optional project id
default_model = "deepseek-reasoner-search"      # fallback local model id
# timeout = 0                                   # optional total request timeout in seconds; defaults to 0 (disabled)

[models.deepseek-reasoner-search]
provider = "deepseek"                           # provider id from [providers.*]
remote_name = "deepseek-reasoner-search"        # upstream model name sent to the API
display_name = "deepseek-reasoner-search"       # human-readable label
# context_window = 128000                       # optional context size hint
# max_output_tokens = 8192                      # optional output token limit
capabilities = ["chat", "reasoning"]           # e.g. chat reasoning vision image_generation
# temperature = 0.7                             # optional model-level default
# reasoning_effort = "medium"                   # optional reasoning level
# [models.deepseek-reasoner-search.patches]
# system_to_user = true                         # optional compatibility patch

# secrets.toml
# [providers.deepseek]
# api_key = "<redacted>"                        # keep real secrets out of config.toml
```

`defaults.context_status` and `--context-status` accept 4 modes:

- `off`: do not inject status
- `always`: inject status before every user turn and keep it in session history
- `latest`: inject status only for the current user turn, then persist the raw user message without the injected status
- `system-once`: inject status once as a system message right after the initial system prompt for a session, then never refresh it again

Thinking and runtime context controls:

- `collapse_thinking = false`: keep `<think>...</think>` content visible in normal text rendering
- `collapse_thinking = true`: hide `<think>...</think>` content in rendered output, while still saving the latest thinking content for `chat thinking`
- `context_window`: runtime context hint. Effective priority is REPL `/context` or CLI `--context-window`, then `models.<id>.context_window`, then `defaults.context_window`
- `reasoning_effort`: runtime reasoning hint. Effective priority is REPL `/reasoning` or CLI `--reasoning-effort`, then `defaults.reasoning_effort`, then `models.<id>.reasoning_effort`
- `reasoning_effort = "auto"`: do not send an explicit reasoning hint upstream

### Sanitized `secrets.toml` Example

```toml
[providers.deepseek]
api_key = "<redacted>"

[providers.minimax]
api_key = "<redacted>"
```

## Output Formats

| Format   | Description                              |
|----------|------------------------------------------|
| `line`   | Single-line summary (default)            |
| `text`   | Full text output                         |
| `json`   | JSON object with metadata                |
| `ndjson` | Newline-delimited JSON for streaming     |

## Local Notes

Study notes under `docs/` are kept as local-only files and are ignored by git.

## Automatic Audit

When `[audit].enabled = true`, `chat ask --tools` and `chat repl --tools` run an audit subagent before dangerous tools execute.

- In the current implementation, mutating tools such as `edit` and `bash` go through the audit path
- Read-oriented tools such as `read`, `grep`, and `fetch` auto-pass and do not wait for audit
- `audit.model`: model ID used for the audit pass; falls back to the active chat model when omitted
- `audit.default_prompt_file`, `audit.bash_prompt_file`, `audit.edit_prompt_file`: editable prompt files used by the audit subagent; the `bash` and `edit` reviews use their own prompt files
- `pass`: the tool is auto-approved and runs without a manual prompt
- `warning` / `block` / `unavailable`: a red warning is shown first, then the normal human confirmation flow is used
- Audit results are stored as dedicated `audit` session events for later inspection

The canonical default audit prompt templates are versioned in [`assets/prompts/`](./assets/prompts/). The active default templates use the base filenames, and separate English variants are provided as `*.en.md` siblings. These files are important because they define the exact JSON output shape expected by the audit parser.

Runtime copies of those prompt files are created under the config directory's `prompts/` folder so they can still be edited directly during prompt tuning.

Sanitized audit benchmark fixtures live under [`assets/testdata/`](./assets/testdata/). The current set includes 100 synthetic cases, expanded request templates, and helper scripts for generation, batch execution, and scoring.

```bash
python scripts/build_audit_subagent_requests.py
python scripts/run_audit_subagent_benchmark.py --model minimax-m2-7 --output assets/testdata/audit-subagent-predictions.jsonl
python scripts/eval_audit_subagent.py --predictions assets/testdata/audit-subagent-predictions.jsonl --failures assets/testdata/audit-subagent-failures.jsonl
```

Recommendations:

- Keep benchmark inputs sanitized before sharing. Use placeholder paths, hosts, and identifiers, and never commit real transcripts, secrets, or user-specific home paths.
- Run the benchmark and evaluator before changing `audit.model` or any audit prompt file so prompt regressions can be caught with the same 100-case suite.
- Store prediction outputs as local-only artifacts when they may include provider responses, internal environment details, or operational context.

## Session Management

Sessions are automatically created with a ULID identifier and persisted to `sessions/<session_id>.jsonl`. Regular messages include:

- `role`: user/assistant
- `content`: message text
- `created_at`: timestamp

When tools are enabled, session files also retain:

- `tool_calls`: assistant-emitted tool call payloads
- `tool_call_id`: tool result linkage back to the originating call
- `name`: tool name for tool-result messages

## License

MIT. See [`LICENSE`](./LICENSE).

## Friends

- [linux.do](https://linux.do)

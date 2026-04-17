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
chat ask --context-status always "Inspect this repo"
chat ask --context-status latest "Only inject status for this turn"
chat ask --context-status system-once --system "Be concise" "Start a new session"
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
```

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
chat config get defaults.context_status
chat config set defaults.context_status system-once
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

## Configuration

Default config locations (XDG compliant):
- Config: `~/.config/chat-cli/config.toml`
- Secrets: `~/.config/chat-cli/secrets.toml`
- Sessions: `~/.local/share/chat-cli/sessions/`

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

[session]
store_format = "jsonl"                          # on-disk session format
# dir = "~/.local/share/chat-cli/sessions"      # optional custom session directory

[tools]
max_rounds = 20                                 # max tool-calling rounds per turn
progressive_loading = true                     # true: expose ToolSearch first; false: expose all tool schemas upfront

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

[profiles.review]
provider = "deepseek"                           # profile-level provider override
model = "deepseek-reasoner-search"              # profile-level model override
system = "You are a careful reviewer."          # optional inline system prompt
temperature = 0.2                               # optional runtime override
max_output_tokens = 8192                        # optional runtime override
output = "text"                                 # line | text | json | ndjson
stream = true                                   # stream responses for this profile

# secrets.toml
# [providers.deepseek]
# api_key = "<redacted>"                        # keep real secrets out of config.toml
```

`defaults.context_status` and `--context-status` accept 4 modes:

- `off`: do not inject status
- `always`: inject status before every user turn and keep it in session history
- `latest`: inject status only for the current user turn, then persist the raw user message without the injected status
- `system-once`: inject status once as a system message right after the initial system prompt for a session, then never refresh it again

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

MIT

# chat-cli

可配置的 LLM 命令行对话工具，使用 Rust 编写，支持多provider、会话管理和机器可读的输出格式。

## 特性

- **多 provider 支持**：OpenAI兼容接口、Anthropic、Ollama
- **交互式 REPL**：支持会话历史的对话模式
- **会话持久化**：自动保存会话为 JSONL 格式
- **机器友好输出**：JSON、NDJSON 和单行文本输出，方便脚本调用
- **工具调用**：支持函数调用和确认机制
- **工具链路落盘**：会话可持久化 assistant `tool_calls` 和 tool result，便于回放与调试
- **自动审核子 agent**：可对发生 tool 操作的回合做二次安全审核，支持配置开关、审核模型和外部 prompt 文件
- **可配置工具暴露模式**：可选择渐进式只先暴露 `ToolSearch`，也可关闭渐进式后一次性暴露全部工具元信息
- **配置管理**：基于 TOML 的配置，支持 provider、model、auth 管理

## 安装

```bash
cargo build --release
cargo install --path .
```

## 快速开始

```bash
# 交互式 REPL 模式
chat repl

# 单次提问
chat ask "解释这个报错"

# 管道输入
git diff | chat ask --stdin "审查这个 diff"

# 指定图片
chat ask -i screenshot.png "描述这个界面"

# 从剪贴板读取图片
chat ask -I "这张图片里有什么？"

# 查看会话列表
chat session list

# 配置 provider
chat config provider set openai --kind openai_compatible --base-url https://api.openai.com/v1
```

## Shell 快捷命令示例

如果你平时主要在 shell 里使用，可以在 `~/.zshrc` 或 `~/.bashrc` 里加入这些快捷命令：

```bash
alias ca='chat ask --stream'
alias cn='chat ask --stream --new-session'
alias ct='chat ask --stream --new-session --temp'
```

含义：

- `ca`：流式单次提问或继续当前会话
- `cn`：流式提问，并强制新建会话
- `ct`：流式提问，并创建临时会话

模型切换如果要配合 `fzf` 交互选择，建议写成函数，不要写成 alias：

```bash
ccm() {
  chat config model use "$(chat --no-color config model list | fzf)"
}
```

原因：

- `alias` 不适合包裹这类带命令替换和交互选择的逻辑，可维护性较差
- `chat config model list` 输出的是 `provider/remote_name`，正好可以直接传给 `chat config model use <target>`
- 加上 `--no-color` 可以避免 ANSI 颜色干扰 `fzf`

如果还想配合 `fzf` 快速切换会话，也可以加一个函数：

```bash
ccs() {
  chat session switch "$(chat --no-color session list | fzf | awk '{print ($1=="*" ? $2 : $1)}')"
}
```

修改完 shell 配置后，重新加载即可：

```bash
source ~/.zshrc
# 或
source ~/.bashrc
```

## 命令

### `chat ask [提示词]`

向 LLM 发送单个提示。

```bash
chat ask "你的问题"
chat ask --stdin "解释这个"  # 从 stdin 读取
chat ask --session <id>     # 继续现有会话
chat ask --new-session      # 创建新会话
chat ask --output json      # JSON 输出模式
chat ask --stream           # 流式输出
chat ask --tools            # 启用工具调用
chat ask --context-status always "分析这个仓库"
chat ask --context-status latest "只在这一轮注入状态"
chat ask --context-status system-once --system "回答简洁" "开始新会话"
```

### `chat repl`

启动交互式 REPL 会话。

```bash
chat repl
chat repl --session <id>     # 继续会话
chat repl --system <prompt>  # 系统提示词
chat repl --multiline        # 启用多行输入
chat repl --context-status system-once
```

### `chat session`

管理聊天会话。

```bash
chat session list            # 列出所有会话
chat session show <id>       # 显示会话详情
chat session render          # 渲染当前会话最近 1 个用户轮次
chat session render <id> --last 3 # 渲染指定会话最近 3 个用户轮次
chat session render <id> --all # 渲染指定会话全部内容
chat session export <id>     # 将会话导出为 JSON
chat session delete <id>     # 删除会话
chat session gc              # 垃圾回收孤立数据
```

### `chat config`

管理配置、provider、model 和认证状态。

```bash
chat config init              # 初始化配置目录
chat config path              # 打印配置/数据/缓存路径
chat config show              # 显示完整配置
chat config get defaults.model
chat config get defaults.context_status
chat config set defaults.context_status system-once
chat config set audit.enabled true
chat config validate          # 校验引用关系和默认项

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

## 配置

默认配置路径（符合 XDG 规范）：
- 配置：`~/.config/chat-cli/config.toml`
- 密钥：`~/.config/chat-cli/secrets.toml`
- 会话：`~/.local/share/chat-cli/sessions/`

先执行一次 `chat config init`。初始化生成的默认配置会把 `defaults.tools = true` 打开，也就是默认开启 tool calling；同时把 `tools.progressive_loading = false` 设为默认关闭，并把 `tools.mcp = false` 设为默认关闭。

## API 接入说明

完整接入分成 4 部分：

1. 创建 provider
2. 在该 provider 下创建一个或多个 model
3. 配置鉴权
4. 选择默认模型

### 最小接入流程

OpenAI 兼容接口示例：

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
export OPENAI_API_KEY="<你的密钥>"

chat config model use gpt-4.1
chat config provider test openai
chat ask "hello"
```

Anthropic 示例：

```bash
chat config provider set anthropic \
  --kind anthropic

chat config model set claude-sonnet-4-7 \
  --provider anthropic \
  --remote-name claude-sonnet-4-7 \
  --capability chat \
  --capability reasoning

chat config auth set anthropic --env ANTHROPIC_API_KEY
export ANTHROPIC_API_KEY="<你的密钥>"

chat config model use claude-sonnet-4-7
chat config provider test anthropic
```

本地 Ollama 示例：

```bash
chat config provider set ollama --kind ollama

chat config model set qwen2.5 \
  --provider ollama \
  --remote-name qwen2.5 \
  --capability chat

chat config model use qwen2.5
chat config provider test ollama
```

### 各配置项的含义

#### Provider

provider 用来定义如何连接上游 API。

- `kind`：`openai_compatible`、`anthropic` 或 `ollama`
- `base_url`：`openai_compatible` 必填；`anthropic` 和 `ollama` 可选
- `api_key_env`：可选，从哪个环境变量读取 API key
- `headers`：附加静态请求头，格式是 `KEY=VALUE`
- `org`、`project`：OpenAI 兼容接口的附加头，会映射成 `OpenAI-Organization` 和 `OpenAI-Project`
- `default_model`：provider 级的默认 model id
- `timeout`：总请求超时秒数；`0` 表示不限制

`base_url` 的行为：

- `openai_compatible`：必须显式配置 `base_url`
- `anthropic`：默认是 `https://api.anthropic.com/v1`
- `ollama`：默认是 `http://localhost:11434/api`

#### Model

model 是本地别名和运行时元数据。

- `id`：CLI 内部使用的本地 model id
- `provider`：它归属的 provider id
- `remote_name`：真正发给上游 API 的模型名
- `capabilities`：功能声明，例如 `chat`、`reasoning`、`vision`
- `reasoning_effort`：可选，兼容模型的推理强度提示
- `patch_system_to_user`：可选，给部分 OpenAI 兼容后端使用的兼容 patch

运行时的模型选择优先级：

1. `--model`
2. `defaults.model`
3. `provider.default_model`

如果选中的 model 不属于当前 provider，请求会直接报错。

#### 鉴权

API key 支持两种配置方式。

使用环境变量：

```bash
chat config auth set openai --env OPENAI_API_KEY
export OPENAI_API_KEY="<你的密钥>"
```

写入 `secrets.toml`：

```bash
chat config auth set openai --value "<你的密钥>"
```

解析优先级：

1. `provider.api_key_env`
2. `secrets.toml`

注意：

- 两边都存在时，环境变量优先
- `ollama` 可以不配置 API key
- `openai_compatible` 和 `anthropic` 一般都需要 API key
- 不要把真实密钥写进 `config.toml`

### 接入完成后常用命令

```bash
chat config show
chat config validate
chat config provider list
chat config model list
chat config auth status
chat repl
chat ask "解释这个报错"
```

### 需要注意的点

- `provider set` 只创建上游接口入口，不会自动创建 model
- `model set --remote-name` 必须和上游真实模型名一致
- 如果要发送图片，model 必须包含 `vision` capability，否则会直接拒绝请求
- `-i/--image` 会读取你显式传入的图片文件路径，并把这些文件作为图片输入直接发送给模型
- `-I/--clipboard-image` 会先从当前剪贴板读取图片，再把剪贴板里的图片发送给模型；它和 `-i` 走的是不同的输入路径，只是最终都会作为支持视觉的模型图片输入
- `chat config provider test <id>` 只校验连通性和基础鉴权，不代表该 provider 下所有 model 都一定可用
- 有些 OpenAI 兼容网关提供 `/models`，有些只支持 `/chat/completions`；当前健康检查在 `/models` 返回 404 时会自动回退
- `defaults.tools = true` 表示 `ask` 和 `repl` 默认启用 tool calling；如果当前模型或网关对 tools 支持不好，可以执行 `chat config set defaults.tools false`
- `tools.progressive_loading = false` 现在是默认值。测试结果表明，关闭渐进式加载时整体体验更好。开启后可能会节省一些 token，因为一开始只暴露 `ToolSearch`，但能力较弱的模型可能不知道当前有哪些工具，也不知道下一步该做什么
- 如果更看重节省 token，而不是工具可发现性，可以执行 `chat config set tools.progressive_loading true`
- `tools.mcp = false` 现在也是默认值。保持关闭时，MCP 会被全链路禁用：不会自动启动 daemon，不会注入 MCP 工具，也不会执行 MCP 工具。需要时再执行 `chat config set tools.mcp true`
- `timeout = 0` 表示不限制总请求时长，长推理请求可能会等待很久
- `chat config model use <target>` 支持本地 model id，例如 `gpt-4.1`，也支持 `provider/model` 形式，例如 `openai/gpt-4.1`
- `chat config auth status` 会显示是否配置了环境变量名、该环境变量当前是否存在、以及文件密钥是否存在

API 密钥应放在 `secrets.toml` 或环境变量里。下面的示例按照当前本地配置整理，但私有域名和用户专属路径已经做了脱敏或归一化处理。

### 当前配置脱敏版

```toml
[defaults]
provider = "deepseek"                           # 默认 provider id，必须存在于 [providers.*]
model = "deepseek-reasoner-search"              # 默认本地 model id，必须存在于 [models.*]
mode = "auto"                                   # 当前请求模式
output = "line"                                 # line | text | json | ndjson
auto_create_session = true                      # 需要时自动创建会话
auto_save_session = true                        # 自动持久化会话
session_id_kind = "ulid"                        # 新会话 id 格式
tools = true                                    # 默认启用工具调用
system_prompt_file = "~/.config/chat-cli/system.md" # 外部 system prompt 文件
system_prompt_mode = "append"                   # append | override
collapse_thinking = false                       # 是否折叠 <think> 输出
context_status = "off"                          # off | always | latest | system-once

[session]
store_format = "jsonl"                          # 会话落盘格式
# dir = "~/.local/share/chat-cli/sessions"      # 可选，自定义会话目录

[tools]
max_rounds = 20                                 # 单轮 ask/repl 最多允许多少轮工具调用
progressive_loading = false                    # false: 默认直接暴露全部工具 schema；true: 先暴露 ToolSearch 以节省部分 token
mcp = false                                    # false: 全链路关闭 MCP；true: 启用 MCP daemon 启动、工具注入和工具执行

[audit]
enabled = true                                  # 启用危险工具审核子 agent
model = "minimax-m2-7"                          # 审核使用的本地 model id，来自 [models.*]
default_prompt_file = "/home/example/.config/chat-cli/prompts/audit-default.md"
bash_prompt_file = "/home/example/.config/chat-cli/prompts/audit-bash.md"
edit_prompt_file = "/home/example/.config/chat-cli/prompts/audit-edit.md"

[skills]
paths = ["~/.claude/skills"]                    # 技能扫描目录

[providers.deepseek]
kind = "openai_compatible"                      # openai_compatible | anthropic | ollama
base_url = "https://<private-gateway>/v1"       # 已脱敏的接口地址
api_key_env = "DEEPSEEK_API_KEY"                # 环境变量名，不是密钥明文
# headers = { "X-Example" = "value" }           # 可选，附加请求头
# org = "example-org"                           # 可选，组织 id
# project = "example-project"                   # 可选，项目 id
default_model = "deepseek-reasoner-search"      # provider 级默认 model，本地 id
# timeout = 0                                   # 可选，请求总超时时间（秒）；默认 0，表示不限制总时长

[models.deepseek-reasoner-search]
provider = "deepseek"                           # 关联的 provider id
remote_name = "deepseek-reasoner-search"        # 实际发给上游 API 的模型名
display_name = "deepseek-reasoner-search"       # 本地展示名
# context_window = 128000                       # 可选，上下文窗口提示值
# max_output_tokens = 8192                      # 可选，输出上限
capabilities = ["chat", "reasoning"]           # 例如 chat reasoning vision image_generation
# temperature = 0.7                             # 可选，模型默认 temperature
# reasoning_effort = "medium"                   # 可选，推理强度
# [models.deepseek-reasoner-search.patches]
# system_to_user = true                         # 可选，兼容性 patch

# secrets.toml
# [providers.deepseek]
# api_key = "<redacted>"                        # 真正的密钥不要写进 config.toml
```

`defaults.context_status` 和 `--context-status` 共有 4 种模式：

- `off`：不注入状态
- `always`：每轮用户消息前都注入状态，并保留到会话历史里
- `latest`：只对当前这轮用户消息注入，回复完成后落盘原始用户消息，不保留注入状态
- `system-once`：只在会话开始时作为一条 system message 注入一次，位置在初始 system prompt 之后，后续不再刷新

### `secrets.toml` 脱敏示例

```toml
[providers.deepseek]
api_key = "<redacted>"

[providers.minimax]
api_key = "<redacted>"
```


## 输出格式

| 格式    | 描述                     |
|---------|--------------------------|
| `line`  | 单行摘要（默认）          |
| `text`  | 完整文本输出              |
| `json`  | 带元数据的 JSON 对象      |
| `ndjson`| 流式输出的换行分隔 JSON    |

## 本地笔记

`docs/` 下的学习笔记只保留在本地，已加入 git 忽略。

## 自动审核

当 `[audit].enabled = true` 时，`chat ask --tools` 和 `chat repl --tools` 会在危险 tool 执行前触发一个审核子 agent。

- 当前实现里，`edit`、`bash` 这类 `mutating` tool 会进入审核链路
- `read`、`grep`、`fetch` 这类只读或只取回内容的 tool 会直接通过，不进审核
- `audit.model`：审核使用的模型 ID；未配置时回退到当前对话模型
- `audit.default_prompt_file`、`audit.bash_prompt_file`、`audit.edit_prompt_file`：审核子 agent 使用的可编辑 prompt 文件，其中 `bash` 和 `edit` 会分别读取各自的 prompt
- 审核 `pass`：自动放行，不再询问人工
- 审核 `warning` / `block` / `unavailable`：先打印红色告警，再进入人工确认
- 审核结果会以独立 `audit` 事件写入 session，便于事后复盘

仓库中的默认审核 prompt 模板会版本化保存在 [`assets/prompts/`](./assets/prompts/) 下。当前生效的默认模板使用基础文件名，英文版本则以 `*.en.md` 的形式单独存放。这些文件比较重要，因为其中约束了审核模型必须返回的 JSON 结构，直接影响审核结果的解析。

运行时会把这些 prompt 文件复制到配置目录下的 `prompts/` 目录，方便继续调试和修改。

脱敏后的审核基准测试数据放在 [`assets/testdata/`](./assets/testdata/) 下。当前仓库提供了 100 条合成 case、展开后的请求模板，以及生成、批量运行、评测三个辅助脚本。

```bash
python scripts/build_audit_subagent_requests.py
python scripts/run_audit_subagent_benchmark.py --model minimax-m2-7 --output assets/testdata/audit-subagent-predictions.jsonl
python scripts/eval_audit_subagent.py --predictions assets/testdata/audit-subagent-predictions.jsonl --failures assets/testdata/audit-subagent-failures.jsonl
```

建议：

- 对外共享测试数据前继续做脱敏，统一使用占位路径、占位域名和占位标识，不要提交真实对话、密钥或用户专属 home 路径。
- 调整 `audit.model` 或任一审核 prompt 文件前，先用这 100 条 case 跑一遍基准，避免 prompt 回归。
- 如果预测结果里可能包含 provider 返回内容、内部环境信息或运行上下文，建议把输出文件只保留在本地。

## 会话管理

会话自动使用 ULID 标识符创建，并保存到 `sessions/<session_id>.jsonl`。普通消息会包含：

- `role`：user/assistant
- `content`：消息内容
- `created_at`：时间戳

启用工具后，还会额外记录：

- `tool_calls`：assistant 发出的 tool call 列表
- `tool_call_id`：tool result 对应的调用 ID
- `name`：tool 名称

## 许可

MIT，见 [`LICENSE`](./LICENSE)。

MIT

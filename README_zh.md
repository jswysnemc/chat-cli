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
git diff | chat ask --stdin -P review

# 指定图片
chat ask -i screenshot.png "描述这个界面"

# 从剪贴板读取图片
chat ask -I "这张图片里有什么？"

# 查看会话列表
chat session list

# 配置 provider
chat config provider set openai --kind openai_compatible --base-url https://api.openai.com/v1
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
```

### `chat repl`

启动交互式 REPL 会话。

```bash
chat repl
chat repl --session <id>     # 继续会话
chat repl --system <prompt>  # 系统提示词
chat repl --multiline        # 启用多行输入
```

### `chat session`

管理聊天会话。

```bash
chat session list            # 列出所有会话
chat session show <id>       # 显示会话详情
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

[session]
store_format = "jsonl"                          # 会话落盘格式
# dir = "~/.local/share/chat-cli/sessions"      # 可选，自定义会话目录

[tools]
max_rounds = 20                                 # 单轮 ask/repl 最多允许多少轮工具调用
progressive_loading = true                     # true: 先暴露 ToolSearch；false: 直接暴露全部工具 schema

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
# timeout = 120                                 # 可选，超时时间（秒）

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

[profiles.review]
provider = "deepseek"                           # profile 级 provider 覆盖
model = "deepseek-reasoner-search"              # profile 级 model 覆盖
system = "You are a careful reviewer."          # 可选，内联 system prompt
temperature = 0.2                               # 可选，运行时覆盖
max_output_tokens = 8192                        # 可选，运行时覆盖
output = "text"                                 # line | text | json | ndjson
stream = true                                   # 此 profile 是否流式输出

# secrets.toml
# [providers.deepseek]
# api_key = "<redacted>"                        # 真正的密钥不要写进 config.toml
```

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

默认审核 prompt 文件会自动创建在配置目录下的 `prompts/` 目录，方便直接调试和修改。

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

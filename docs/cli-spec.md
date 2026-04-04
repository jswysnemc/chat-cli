# chat-cli CLI 规格草案

## 1. 命名约定

- Cargo package 暂定：`chat-cli`
- 可执行文件暂定：`chat`

如果后续决定与现有命令冲突，再统一重命名；当前规格先以 `chat` 作为根命令说明。

## 2. 全局选项

```text
chat [GLOBAL_OPTIONS] <COMMAND>
```

全局选项建议：

- `-p, --provider <ID>`：覆盖 Provider
- `-m, --model <ID>`：覆盖 Model
- `--mode <human|agent|auto>`：输出模式，默认 `auto`
- `--output <line|text|json|ndjson>`：输出格式
- `--config-dir <PATH>`：覆盖配置目录
- `--data-dir <PATH>`：覆盖数据目录
- `--no-color`：禁用颜色
- `-v, --verbose`：输出调试日志到 stderr
- `-q, --quiet`：抑制非必要 stderr 输出

规则：

- `stdout` 只输出业务结果
- 日志、告警、重试信息全部走 `stderr`
- `chat ask` 默认输出格式为 `line`
- `chat repl` 默认输出格式为 `text`
- `json` / `ndjson` 模式下默认隐式开启 `--quiet`
- `line` 为单行 `key=value` 输出，字符串字段使用 JSON 转义保证可解析

## 3. 顶级命令树

```text
chat
  ask
  repl
  session
  config
  doctor
  completion
```

## 4. 对话命令

### 4.1 `chat ask`

用于一次性对话，适合真人和智能体。

```text
chat ask [PROMPT]
```

选项：

- `PROMPT`：单段 prompt，可选
- `--stdin`：从 stdin 读取用户输入
- `-s, --system <TEXT|@FILE>`：系统提示词
- `-a, --attach <PATH>`：将文件内容注入 prompt，可重复
- `-i, --image <PATH>`：附加图片输入，可重复
- `-I, --clipboard-image`：从系统剪贴板读取一张图片
- `--session <ID>`：切换到指定会话并继续
- `--new-session`：显式创建新会话
- `--ephemeral`：生成 `session_id` 但不落盘
- `--stream`：流式输出
- `--temperature <FLOAT>`
- `--max-output-tokens <N>`
- `--param <K=V>`：附加模型参数，可重复
- `--timeout <DURATION>`
- `--raw-provider-response`：输出供应商原始响应，用于调试

行为约束：

- `PROMPT` 和 `--stdin` 至少有一个
- 未指定 `--session` 且未使用 `--new-session` 时，默认复用当前会话
- 没有当前会话时自动创建新 `session_id`
- 默认把会话保存到本地；显式 `--ephemeral` 才跳过
- `--output line` 为默认行为，返回单行摘要并包含当前 `session_id`
- `--output text` 时输出纯 assistant 内容，适合真人阅读
- `--output json` 时输出单个 JSON 对象
- `--output ndjson --stream` 时每行一个事件对象
- `--stream` 只允许和 `text` 或 `ndjson` 配合使用

### 4.2 `chat repl`

用于交互式多轮会话。

```text
chat repl
```

选项：

- `--session <ID>`：恢复已有会话
- `--new-session`：显式创建新会话
- `--ephemeral`：会话只在进程内存在
- `--system <TEXT|@FILE>`
- `--multiline`：多行输入模式
- `--stream`

第一版建议只做行式 REPL，不做全屏 TUI。

行为约束：

- 未指定 `--session` 且未使用 `--new-session` 时，默认复用当前会话
- 没有当前会话时自动创建新 `session_id`
- 默认持久化会话
- REPL 默认使用 `text` 输出，不走 `line`

## 5. 会话命令

### 5.1 `chat session list`

列出会话，默认显示：

- 当前会话标记
- 创建时间和最近更新时间
- 首条用户提问预览
- 用户消息数和 assistant 消息数

### 5.2 `chat session show <ID>`

显示会话元数据和消息概览。

### 5.3 `chat session export <ID>`

导出为 `json` 或 `jsonl`。

### 5.4 `chat session delete <ID>`

删除会话。

### 5.5 `chat session gc`

清理过期或临时会话。

## 6. 配置命令

### 6.1 `chat config init`

初始化目录和空 `TOML` 配置。

### 6.2 `chat config path`

显示配置、数据、缓存、日志路径。

### 6.3 `chat config show`

查看合并后的有效配置。

### 6.4 `chat config get <KEY>`

读取单个配置项。

### 6.5 `chat config set <KEY> <VALUE>`

设置单个配置项。

### 6.6 `chat config validate`

校验配置一致性。

## 7. Provider 管理

`Provider`、`Model`、`Auth` 统一放在 `config` 下，默认使用 `defaults.provider` 和 `defaults.model`。

### 7.1 `chat config provider set`

```text
chat config provider set <ID> --kind <KIND> [OPTIONS]
```

选项：

- `--kind <openai_compatible|anthropic|ollama>`
- `--base-url <URL>`
- `--api-key-env <ENV_NAME>`
- `--header <K=V>`：附加静态请求头，可重复
- `--org <ORG>`
- `--project <PROJECT>`
- `--default-model <MODEL_ID>`
- `--timeout <DURATION>`

`Provider` 表示供应商连接方式，不等同于具体模型。
`set` 为 upsert 语义，不再区分 `add` 和 `update`。

### 7.2 `chat config provider list`

列出已注册 Provider。

### 7.3 `chat config provider get <ID>`

显示 Provider 详情。

### 7.4 `chat config provider remove <ID>`

删除 Provider。

### 7.5 `chat config provider test <ID>`

测试认证、连通性和默认模型可用性。

## 8. Model 管理

### 8.1 `chat config model set`

```text
chat config model set <ID> --provider <PROVIDER_ID> --remote-name <NAME> [OPTIONS]
```

选项：

- `--provider <PROVIDER_ID>`
- `--remote-name <NAME>`：供应商侧模型名
- `--display-name <NAME>`
- `--context-window <N>`
- `--max-output-tokens <N>`
- `--capability <chat|vision|json|tools|reasoning>`：可重复
- `--temperature <FLOAT>`：默认参数
- `--reasoning-effort <low|medium|high>`：模型默认思考等级
- `--patch-system-to-user`：请求前把 `system` 消息改写为 `user`

这里的 `Model` 是本地注册项，允许给远端模型起稳定别名。
`set` 为 upsert 语义。

### 8.2 `chat config model list`

支持按 Provider 过滤。

### 8.3 `chat config model get <ID>`

显示模型配置和能力标签。

### 8.4 `chat config model use <PROVIDER_ID>/<MODEL_NAME>`

把当前默认模型切到指定项，并同步更新 `defaults.provider`。

示例：

- `chat config model use grok2api/grok-4.1-fast`
- `chat config model use minimax/MiniMax-M2.7`

### 8.5 `chat config model remove <ID>`

删除模型注册项。

## 9. 认证命令

### 9.1 `chat config auth set <PROVIDER_ID>`

为 Provider 设置密钥。

选项：

- `--stdin`
- `--value <SECRET>`
- `--env <ENV_NAME>`

建议行为：

- 优先写入系统 keyring
- 如果 keyring 不可用，回退到 `secrets.toml`

### 9.2 `chat config auth status [PROVIDER_ID]`

查看某个 Provider 是否已配置 Secret，不显示明文。

### 9.3 `chat config auth remove <PROVIDER_ID>`

移除 Secret。

## 10. 诊断命令

### `chat doctor`

输出：

- 目录权限
- 配置文件可读性
- Secret 状态
- Provider 连通性
- 默认 Profile 可用性

这个命令很重要，因为它是智能体自动接入前的健康检查入口。

## 11. Shell 补全

### `chat completion`

支持：

- `bash`
- `zsh`
- `fish`

## 12. 建议退出码

- `0`：成功
- `2`：命令参数错误
- `3`：配置错误
- `4`：认证缺失或认证失败
- `5`：网络或供应商调用失败
- `6`：限流
- `7`：Provider 不存在
- `8`：Model 不存在
- `9`：Profile 不存在
- `10`：Session 不存在

## 13. 输出协议

### `--output line`

默认输出格式，单行返回，至少包含当前 `session_id` 和本轮结果摘要。

建议格式：

```text
ok=1 session_id="sess_01JQ..." provider="openai" model="gpt4" finish_reason="stop" latency_ms=820 input_tokens=12 output_tokens=34 content="你好，我可以帮你分析这个报错。"
```

约束：

- 字段顺序固定，降低解析成本
- 字符串字段使用 JSON 字符串转义
- `content` 默认输出完整 assistant 内容，但保持单行

### `--output text`

输出纯 assistant 内容，适合真人阅读或进一步管道处理。

### `--output json`

建议输出结构：

```json
{
  "ok": true,
  "provider": "openai",
  "model": "gpt-4.1",
  "session_id": "sess_123",
  "message": {
    "role": "assistant",
    "content": "..."
  },
  "usage": {
    "input_tokens": 12,
    "output_tokens": 34,
    "total_tokens": 46
  },
  "finish_reason": "stop",
  "latency_ms": 820
}
```

### `--output ndjson --stream`

建议事件类型：

- `response.started`
- `response.delta`
- `response.completed`
- `response.error`

示例：

```json
{"type":"response.started","session_id":"sess_123"}
{"type":"response.delta","delta":"你"}
{"type":"response.delta","delta":"好"}
{"type":"response.completed","finish_reason":"stop","usage":{"input_tokens":12,"output_tokens":34,"total_tokens":46}}
```

## 14. 配置 Schema 草案

```toml
[defaults]
provider = "openai"
model = "gpt4"
mode = "auto"
output = "line"
auto_create_session = true
auto_save_session = true
session_id_kind = "ulid"

[session]
store_format = "jsonl"
dir = "~/.local/share/chat-cli/sessions"

[providers.openai]
kind = "openai_compatible"
base_url = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"
default_model = "gpt4"

[tools]
max_rounds = 20

[skills]
paths = [".claude/skills", "~/.claude/skills"]

[models.gpt4]
provider = "openai"
remote_name = "gpt-4.1"
display_name = "GPT-4.1"
capabilities = ["chat", "json", "reasoning"]
context_window = 1048576
max_output_tokens = 32768

[models.qw_coder_model]
provider = "cpap"
remote_name = "qw/coder-model"
capabilities = ["chat", "reasoning", "vision", "image_generation"]

[models.team_gpt_5_4]
provider = "cpap"
remote_name = "team/gpt-5.4"
capabilities = ["chat", "reasoning", "vision"]

[models.team_gpt_5_4.patches]
system_to_user = true

```

## 15. 当前建议

实现时先只把以下主线做透：

1. `config init`
2. `config provider set/list/get/test`
3. `config model set/list/get/use`
4. `config auth set/status`
6. `ask`
7. `session list/show/export`

其余命令可以先占位，但帮助文本和参数结构要先定下来，避免后续 breaking change。

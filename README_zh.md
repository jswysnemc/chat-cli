# chat-cli

可配置的 LLM 命令行对话工具，使用 Rust 编写，支持多provider、会话管理和机器可读的输出格式。

## 特性

- **多 provider 支持**：OpenAI兼容接口、Anthropic、Ollama
- **交互式 REPL**：支持会话历史的对话模式
- **会话持久化**：自动保存会话为 JSONL 格式
- **机器友好输出**：JSON、NDJSON 和单行文本输出，方便脚本调用
- **工具调用**：支持函数调用和确认机制
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

管理配置。

```bash
chat config init              # 初始化配置目录
chat config show              # 显示完整配置
chat config provider list     # 列出 providers
chat config provider set <id> --kind <type> --base-url <url>
chat config model list        # 列出模型
chat config auth set <provider> # 设置 API 密钥
chat config doctor            # 诊断配置问题
```

## 配置

默认配置路径（符合 XDG 规范）：
- 配置：`~/.config/chat-cli/config.toml`
- 密钥：`~/.config/chat-cli/secrets.toml`
- 会话：`~/.local/share/chat-cli/sessions/`

### 配置示例

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

## 输出格式

| 格式    | 描述                     |
|---------|--------------------------|
| `line`  | 单行摘要（默认）          |
| `text`  | 完整文本输出              |
| `json`  | 带元数据的 JSON 对象      |
| `ndjson`| 流式输出的换行分隔 JSON    |

## 会话管理

会话自动使用 ULID 标识符创建，并保存到 `sessions/<session_id>.jsonl`。每条消息包含：

- `id`：ULID 标识符
- `role`：user/assistant
- `content`：消息内容
- `timestamp`：ISO 8601 时间戳

## 许可

MIT

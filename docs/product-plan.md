# chat-cli 产品与实施计划

## 1. 项目目标

`chat-cli` 是一个 Rust 编写的命令行工具，用于让两类调用方稳定地使用大模型对话能力：

- 真人用户：需要交互式、可读性高、支持流式输出和会话保存。
- 智能体/自动化工具：需要稳定参数、可机器解析输出、清晰退出码、可通过 stdin/stdout 接入。

这个项目的核心不是做一个“只会请求某一家 API 的脚本”，而是做一个有配置中心、模型注册、供应商抽象、会话持久化和自动化友好输出的通用对话 CLI。配置文件格式在第一版直接定为 `TOML`，避免后续再做格式兼容和迁移。

## 2. 核心设计原则

- 双模式优先：同一套能力同时服务 `human` 和 `agent` 两种使用方式。
- 配置先行：供应商、模型、Profile、Secrets 都必须可管理，不把关键信息硬编码进命令。
- TOML 定型：配置文件只使用 `TOML`，会话导出再按需要支持 `json` / `jsonl`。
- 输出稳定：为智能体提供严格的 `json` / `ndjson` 输出，不把日志混入 stdout。
- Session-first：每次对话默认自动创建 `session_id`，默认落盘保存，保证可追踪和可续接。
- 渐进扩展：先支持最小闭环，再加更多供应商和高级能力。
- OpenAI-compatible 优先：先打通一类通用协议，再补齐 Anthropic/Ollama 等特化实现。

## 3. 核心用户场景

### 真人用户

- 一次性提问：`chat ask --output text "解释这个报错"`
- 管道输入：`git diff | chat ask --stdin -P review`
- 交互式会话：`chat repl -P default`
- 管理配置：添加供应商、模型、切换默认 Profile

### 智能体

- 通过 stdin 提交 prompt，通过 stdout 获取模型输出
- 默认要求单行返回，至少包含 `session_id`、`provider`、`model`、`finish_reason`
- 需要 `json` / `ndjson` 流式事件，便于自动解析
- 需要可靠退出码，区分配置错误、认证失败、网络失败、模型不存在等情况

## 4. 非目标

以下内容不进入第一阶段：

- 全屏 TUI
- 云端账号体系和远程同步
- 工作流编排平台
- 自动执行工具调用结果的 Agent Runtime
- 图像/音频/多模态复杂上传界面

## 5. 建议 MVP 范围

第一版必须形成完整闭环：

- 配置目录初始化
- 管理 Provider
- 管理 Model
- 管理 Profile
- 读取 Secret
- 单轮对话 `ask`
- 流式输出
- 自动创建并保存会话
- 会话保存与恢复
- 默认单行摘要输出
- 机器可解析输出

第一版建议优先支持：

- `openai_compatible`
- `anthropic`
- `ollama`

其中开发顺序建议是：

1. `openai_compatible`
2. `anthropic`
3. `ollama`

## 6. 工程结构建议

```text
chat-cli/
  Cargo.toml
  src/
    main.rs
    cli/
      mod.rs
      ask.rs
      repl.rs
      session.rs
      config/
        mod.rs
        root.rs
        provider.rs
        model.rs
        profile.rs
        auth.rs
    app/
      mod.rs
      run.rs
      output.rs
      exit_code.rs
    config/
      mod.rs
      paths.rs
      loader.rs
      schema.rs
      secrets.rs
      validate.rs
    domain/
      mod.rs
      message.rs
      chat.rs
      provider.rs
      model.rs
      profile.rs
      session.rs
    providers/
      mod.rs
      openai_compatible.rs
      anthropic.rs
      ollama.rs
    store/
      mod.rs
      config_store.rs
      session_store.rs
    transport/
      mod.rs
      http.rs
      stream.rs
  docs/
    product-plan.md
    cli-spec.md
```

## 7. 配置与数据存储策略

建议遵循 XDG 目录规范，并把 `TOML` 固定为唯一配置格式：

- 配置：`~/.config/chat-cli/config.toml`
- Secret 回退文件：`~/.config/chat-cli/secrets.toml`
- 数据：`~/.local/share/chat-cli/sessions/`
- 缓存：`~/.cache/chat-cli/`

约束：

- `config.toml` 是唯一主配置文件
- `secrets.toml` 只作为 keyring 不可用时的回退
- 不在 MVP 中支持 `yaml` / `json` 配置文件

建议优先顺序：

1. 命令行参数
2. 环境变量
3. Profile 配置
4. 全局默认配置

Secret 读取优先级建议：

1. 显式命令行输入
2. 环境变量
3. OS keyring
4. `secrets.toml`

会话策略建议：

1. `chat ask` 未指定 `--session` 时自动创建新 `session_id`
2. 默认持久化到 `sessions/<session_id>.jsonl`
3. 指定 `--session <ID>` 时向已有会话追加消息
4. 仅在显式 `--ephemeral` 时跳过落盘

命令收口建议：

- 顶层命令只保留 `ask`、`repl`、`session`、`config`、`doctor`、`completion`
- `provider`、`model`、`profile`、`auth` 统一挂到 `config` 下面
- 实体管理统一采用 `set/list/get/remove`，减少 `add/show/update/remove` 的重复面

## 8. 实施阶段

### Phase 0: 规格冻结

- 完成命令树设计
- 完成 `TOML` schema 设计
- 定义 stdout/stderr/exit code 约束
- 冻结默认单行输出格式和自动会话语义

### Phase 1: 工程骨架

- 初始化 Cargo 工程
- 接入 `clap`、`tokio`、`serde`、`reqwest`
- 完成配置加载、目录管理和基础日志

### Phase 2: 配置管理闭环

- 实现 `config provider/model/profile/auth` 子命令
- 实现本地配置 CRUD
- 实现配置校验与 `doctor`

### Phase 3: 基础对话闭环

- 实现 `ask`
- 接入 `openai_compatible`
- 默认自动创建并保存 `session_id`
- 支持默认单行输出 `line`
- 支持非流式和流式输出
- 支持 stdin 输入和 session 续接

### Phase 4: 多供应商扩展

- 接入 `anthropic`
- 接入 `ollama`
- 统一 usage / error / streaming 抽象

### Phase 5: 交互体验与测试

- 实现 `repl`
- 增加 integration tests
- 增加 golden output tests
- 完善帮助文本和示例

## 9. 建议依赖

- `clap`：命令行解析
- `tokio`：异步运行时
- `reqwest`：HTTP 客户端
- `serde` / `serde_json` / `toml`：配置和协议序列化
- `directories`：配置目录定位
- `thiserror` / `anyhow`：错误处理
- `tracing` / `tracing-subscriber`：日志
- `keyring`：系统密钥存储
- `uuid` / `time`：会话 ID 与时间戳
- `assert_cmd` / `predicates` / `insta` / `wiremock`：测试

## 10. 测试策略

- 单元测试：配置合并、schema 校验、命令参数解析
- 集成测试：CLI 子命令、退出码、stdout/stderr 行为
- Provider mock 测试：请求体、响应体、流式事件
- Golden tests：`json` / `ndjson` 输出稳定性

## 11. 当前结论

这个项目应当先做成“可配置、可脚本化、可扩展”的对话 CLI，再考虑更重的交互形态。第一阶段不追求能力面最广，而是优先把三件事做稳：`TOML` 配置结构、收口后的命令树、默认单行输出加自动会话管理。

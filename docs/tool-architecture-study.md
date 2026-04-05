# Tool 架构学习与本项目优化

这份文档主要用于对照学习 `../codex` 和 `../claude-code-source` 两个项目，并解释本仓库这次对 `tool` 部分做了什么优化。

## 1. 先看三个项目的差异

### 本项目优化前

本项目原来的 `tool` 实现主要集中在 [`src/tool.rs`](../src/tool.rs)：

- `tool schema`
- `tool 调用解析`
- `tool 执行`
- `确认交互`
- `预览渲染`
- `skill 扫描`

几乎都放在一个文件、一个大 `match` 里。

这会带来三个问题：

1. `tool definition` 和 `tool runtime` 耦合太紧，不方便扩展。
2. 缺少像 Codex / Claude 那样的显式 metadata，无法表达“只读/有副作用/是否适合并行”。
3. tool round 的中间消息没有完整落盘，导致下一轮对话无法真实回放工具链路，也不利于学习调试。

### Codex 的启发

重点参考了这些路径：

- `../codex/codex-rs/tools/src/tool_registry_plan.rs`
- `../codex/codex-rs/core/src/tools/registry.rs`
- `../codex/codex-rs/core/src/tools/context.rs`
- `../codex/codex-rs/core/src/tools/handlers/shell.rs`

Codex 的核心特点：

1. `ToolSpec`、handler、runtime context 是拆开的。
2. tool 注册是“计划化”的，schema 和 handler 不是写死在一个 switch 里。
3. 执行时有明确的 context，方便接权限、sandbox、hook、telemetry。
4. tool output 也有统一抽象，而不是只有字符串。

### Claude Code 的启发

重点参考了这些路径：

- `../claude-code-source/src\tools.ts`
- `../claude-code-source/src\Tool.ts`
- `../claude-code-source/src\tools\FileReadTool\FileReadTool.ts`
- `../claude-code-source/src\tools\FileEditTool\FileEditTool.ts`
- `../claude-code-source/src\tools\BashTool\BashTool.tsx`

Claude 的核心特点：

1. 每个 tool 都有明确接口：schema、description、permission、validation、UI render。
2. tool 自带行为 metadata，比如：
   - 是否只读
   - 是否 destructive
   - 是否适合并发
   - 是否需要用户交互
3. built-in tools 和 MCP tools 的组装是单独一层逻辑。
4. transcript 对 tool use / tool result 的保留更完整。

## 2. 本项目这次做的优化

### 2.1 引入内建 tool registry

现在 [`src/tool.rs`](../src/tool.rs) 里不再只靠一个大 `match` 同时承担所有职责，而是增加了几层明确抽象：

- `ToolSpec`
- `ToolHandler`
- `ToolRuntimeContext`
- `ToolSideEffects`
- `ToolParallelism`

这样做的好处：

1. `tool_definitions()` 改为从 registry 生成。
2. `execute_tool()` 先做 handler lookup，再执行具体实现。
3. 每个 tool 都有显式 metadata，后续要加权限、并行调度、统计都会更自然。

这部分是明显借鉴 Codex 的 registry/handler/context 分层，以及 Claude 的 tool 行为元数据。

### 2.2 补全 tool transcript 持久化

重点改动在：

- [`src/session.rs`](../src/session.rs)
- [`src/app.rs`](../src/app.rs)

现在 `SessionMessage` 新增了这些字段：

- `tool_calls`
- `tool_call_id`
- `name`

效果是：

1. assistant 发出的 `tool_calls` 会落盘。
2. tool result message 会落盘。
3. 下一轮 `prepare_ask()` 会把这些消息重新恢复到 provider 请求里。

这比原来只保存 `user -> final assistant` 更接近真实 agent transcript。

对学习来说，这一点很重要，因为你现在可以直接从 session 文件里看到：

- 模型决定调用了什么工具
- 工具返回了什么
- 最终 assistant 是如何基于工具结果回答的

### 2.3 `session show` 现在能看到 tool 元信息

`chat session show <id>` 现在会输出：

- `tool_calls` 数量
- `tool_call_id`
- `name`

这让 session inspection 不再只适合普通聊天，也能用于 agent/tool 调试。

## 3. 这次改动后的结构理解

### `src/tool.rs`

现在主要承担两类职责：

1. tool registry 与 dispatch
2. 内建工具的具体实现

虽然还没有像 Codex / Claude 那样继续拆成多个文件，但已经从“只有大 switch”进化到“显式 registry + handler”。

### `src/app.rs`

这里主要处理：

1. tool round loop
2. assistant tool call message 的拼接
3. tool result message 的注入
4. tool transcript 的 session 落盘与回放

### `src/session.rs`

这里从“只存普通消息”升级成“能存 tool-aware transcript”。

## 4. 现在已经具备、但还没继续做深的方向

这次是第一步，已经把扩展点铺出来了。下一步如果继续向 Codex / Claude 学，可以优先做：

1. 路径级权限与工作区边界
   - 例如只允许在 cwd 内写
   - 把 `bash` / `fetch` / `write` 的权限策略配置化

2. 基于 metadata 的并行调度
   - 只读、无确认的 tool 可以在同一轮并行跑

3. 把 tool UI 和 tool runtime 再拆开
   - 类似 Claude 的 render / permission / validate 分层

4. 给 tool result 增加结构化输出
   - 类似 Codex 的统一 `ToolOutput`

## 5. 这次改动最值得关注的点

如果你是为了学习两个优秀项目，建议重点看下面几个文件：

- [`src/tool.rs`](../src/tool.rs)
- [`src/app.rs`](../src/app.rs)
- [`src/session.rs`](../src/session.rs)

重点观察：

1. 一个小项目如何从“大 switch”迈向“registry + metadata + transcript”。
2. 为什么 tool transcript 持久化会直接影响多轮 agent 质量。
3. 为什么 Codex / Claude 都会把 tool 的 schema、执行、权限、上下文拆开。

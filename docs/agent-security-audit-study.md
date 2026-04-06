# Agent 安全机制与操作审计学习

这份文档用于对照学习 `../codex` 与 `../claude-code-source` 的安全设计，并回答一个更实际的问题：

如何审核 agent 的操作。

目标不是罗列所有实现细节，而是提炼出可以迁移到本项目的安全边界、审批流和审计方法。

## 1. 先看两个项目的安全机制

### 1.1 Codex 的安全机制

建议先看这些文件：

- `../codex/codex-rs/core/src/config/permissions.rs`
- `../codex/codex-rs/core/src/tools/handlers/shell.rs`
- `../codex/codex-rs/core/src/tools/network_approval.rs`
- `../codex/codex-rs/core/src/guardian/approval_request.rs`
- `../codex/codex-rs/core/src/tools/events.rs`
- `../codex/codex-rs/protocol/src/approvals.rs`

从实现上看，Codex 的安全机制是分层的。

第一层是静态权限配置。

`permissions.rs` 把权限拆成：

- 文件系统权限
- 网络权限
- Unix socket 权限
- sandbox policy

也就是说，安全边界不是单纯“能不能执行命令”，而是把命令可能触达的资源面拆开管理。

第二层是审批策略。

Codex 在协议层定义了比较明确的 approval policy，例如：

- `never`
- `unless-trusted`
- `on-failure`
- `on-request`

这类策略的意义是，模型不能随意决定是否提权，必须服从运行时策略。

第三层是执行前审批与提权约束。

在 `shell.rs` 里，命令执行前会先检查：

- 是否请求了 sandbox override
- 当前 approval policy 是否允许这么做
- 是否命中 apply_patch 拦截
- 是否需要生成 exec approval requirement

这意味着“工具会不会执行”不是 tool 自己一句话说了算，而是要经过 orchestration 和审批策略。

第四层是网络审批。

`network_approval.rs` 单独处理网络放行，而不是把网络访问混在普通 shell 审批里。这里很关键，因为它把“命令执行权限”和“网络出站权限”拆成了两个风险面。

第五层是 Guardian 审核。

`guardian/approval_request.rs` 把高风险动作抽象成结构化对象，例如：

- `Shell`
- `ExecCommand`
- `ApplyPatch`
- `NetworkAccess`
- `McpToolCall`

这说明 Codex 在设计上已经把“审批”视为结构化审查，而不是单纯弹窗确认。

第六层是事件审计。

`tools/events.rs` 和 `protocol/approvals.rs` 说明 Codex 会发出结构化事件，例如：

- `ExecCommandBegin`
- `ExecCommandEnd`
- `PatchApplyBegin`
- `PatchApplyEnd`
- `ExecApprovalRequest`
- `GuardianAssessment`

这对审计非常重要，因为它保留了“动作前”“动作后”“审批中”的完整链路。

### 1.2 Claude Code 的安全机制

建议先看这些文件：

- `../claude-code-source/src\Tool.ts`
- `../claude-code-source/src\utils\permissions\permissions.ts`
- `../claude-code-source/src\utils\permissions\PermissionUpdate.ts`
- `../claude-code-source/src\utils\hooks.ts`
- `../claude-code-source/src\cli\structuredIO.ts`
- `../claude-code-source/src\utils\sessionStorage.ts`
- `../claude-code-source/src\tools\BashTool\shouldUseSandbox.ts`

Claude 的设计更偏“策略引擎 + hooks + transcript”。

第一层是 tool 自身声明能力。

在 `Tool.ts` 里，每个 tool 都可以表达：

- 是否只读
- 是否 destructive
- 是否需要用户交互
- 是否适合并发
- 如何检查 permissions

这意味着安全属性被建模进 tool 接口，而不是散落在调用端。

第二层是 permission context。

`permissions.ts` 里把规则拆成：

- `alwaysAllowRules`
- `alwaysDenyRules`
- `alwaysAskRules`

还支持多来源，例如：

- settings
- CLI 参数
- session
- command

这让权限来源可追踪，也方便审计“是谁允许了这次操作”。

第三层是动态 permission update。

`PermissionUpdate.ts` 支持在运行过程中修改：

- mode
- allow / deny / ask 规则
- additional working directories

这相当于把“审批结果”变成可持久化的策略变更，而不是一次性按钮点击。

第四层是 hooks。

`hooks.ts` 里有大量 lifecycle hooks，其中和安全最相关的是：

- `PermissionRequest`
- `ConfigChange`
- `InstructionsLoaded`
- `SessionStart`
- `SessionEnd`

这些 hooks 的价值在于：

1. 可以阻断动作
2. 可以自动批准/拒绝
3. 即使不阻断，也可以做审计记录

第五层是 workspace trust。

`hooks.ts` 里明确写了：

- 所有 hooks 在 interactive mode 下都要求 workspace trust

这很重要，因为 hooks 本质上能执行任意命令，如果没有 trust gate，本身就会成为一条 RCE 通道。

第六层是 permission prompt 与 hook 并行竞速。

`structuredIO.ts` 里的 `createCanUseTool()` 会让：

- hook 判断
- SDK permission prompt

并行执行，谁先决定谁生效。

这说明 Claude 把审批系统做成了“可插拔策略 + 用户交互”的组合，而不是纯 UI 流程。

第七层是 transcript 持久化。

`sessionStorage.ts` 说明 Claude 会把 transcript 当成重要状态，而不只是聊天记录。对审计来说，这意味着你可以事后重建：

- 当时模型说了什么
- 调用了什么 tool
- 中间发生了哪些系统消息和工具消息

### 1.3 两者共同点

虽然风格不同，但 Codex 和 Claude 有几个共同安全原则：

1. 高风险动作必须经过显式审批。
2. sandbox 与 approval 是两层，而不是一层。
3. 文件系统、网络、MCP 这几类风险面分开治理。
4. 审批结果要结构化，而不是只留一句文本。
5. 审计日志要能重建完整动作链路。

## 2. 如何审核 agent 的操作

如果把“审核”说得更具体，本质上是在回答两个问题：

1. 这次操作有没有越界。
2. 事后能不能完整追责和复盘。

所以审计不能只看最终回答，必须看完整操作链。

### 2.1 审核对象应该是什么

最小审计单元不应该只是“一条 assistant 回复”，而应该是：

- session
- turn
- tool call
- approval decision
- side effect

也就是：

一个 turn 里，模型发起了什么动作，请求了什么权限，最终对环境造成了什么影响。

### 2.2 必须记录的字段

要做到可审核，至少要记录这些字段：

- `session_id`
- `turn_id`
- `agent_id` 或执行主体
- `tool_name`
- `tool_input_summary`
- `tool_risk_class`
- `sandbox_policy`
- `approval_policy`
- `approval_decision`
- `decision_source`
- `cwd`
- `target_paths`
- `network_targets`
- `started_at`
- `finished_at`
- `result_summary`
- `changed_files`
- `raw_tool_call_id`

如果缺少这些字段，事后就很难回答下面这些问题：

- 这次写文件是谁批准的
- 是用户批准，还是规则自动放行
- 命令是否跑在 sandbox 内
- 网络访问是一次性放行，还是 session 级放行
- 结果是否真的改了文件

### 2.3 审核时重点看什么

从安全审计角度，可以把操作分成四级：

#### A. 纯读操作

例如：

- read
- grep
- list
- skill_read

重点检查：

- 是否读取了超出工作区边界的路径
- 是否读取了敏感文件
- 是否被工具伪装成“读”，但实际上包含执行语义

#### B. 本地变更操作

例如：

- write
- apply_patch
- edit notebook

重点检查：

- 修改目标文件是什么
- 修改前后差异是什么
- 是否存在批量写入
- 是否覆盖了用户未授权区域

#### C. 命令执行操作

例如：

- bash
- shell
- exec_command

重点检查：

- 命令是否只读
- 是否请求提权
- 是否绕过 sandbox
- 是否包含危险子命令
- 是否触发了额外权限

#### D. 外部访问操作

例如：

- fetch
- web_search
- MCP open-world tool

重点检查：

- 访问了哪个 host
- 使用了什么协议
- 是单次放行还是长期放行
- 工具返回内容是否被完整记录

### 2.4 审核流程应该长什么样

一个比较合理的审核流程是：

1. 先按 `tool metadata` 给动作分类
2. 读取该动作的 policy snapshot
3. 查看审批来源
4. 查看原始输入摘要
5. 查看 side effect
6. 对照 session transcript 复盘上下文

如果是高风险动作，再额外检查：

1. 是否存在同一 turn 的多次连续提权
2. 是否存在“先读敏感信息，再网络外传”的链路
3. 是否存在通过 hook / MCP / shell 包装绕过主权限系统的情况

## 3. 能从 Codex 和 Claude 学到什么审计设计

### 3.1 审计要用结构化事件，而不是纯文本日志

Codex 的 `ExecCommandBegin` / `ExecApprovalRequest` / `GuardianAssessment` 非常值得学。

原因很简单：

- 文本日志适合看
- 结构化事件适合查、过滤、聚合、导出

如果后续要做：

- 审计报表
- 风险检索
- 回放工具链路
- 企业合规留痕

结构化事件几乎是必需的。

### 3.2 审批结果要可追溯到来源

Claude 的 permission rules 和 permission updates 很值得学。

因为“允许”本身也要被审计。你不仅要记录“这次执行被允许了”，还要记录：

- 是用户点了允许
- 是 hook 自动允许
- 是 session 规则已放行
- 是本地 settings 已放行

否则排查时会卡在“到底是谁开的口子”。

### 3.3 hooks 既是能力，也是风险面

Claude 明确要求 hooks 走 workspace trust，这是非常正确的。

因为很多系统把 hook 当成“增强功能”，但从安全角度看，hook 就是可执行代码注入点。

所以 hook 的审计至少要记录：

- hook 名称
- hook 触发时机
- hook 输入
- hook 输出
- hook 是否阻断
- hook 是否修改了权限决策

### 3.4 transcript 不只是聊天记录，而是审计证据

Claude 的 sessionStorage 和 Codex 的 event stream 都说明了一点：

agent 系统里，transcript 是证据链的一部分。

如果 transcript 里缺少：

- tool call
- tool result
- approval message
- system side event

那就很难完整复盘行为。

## 4. 对本项目最直接的落地建议

本项目现在已经有了一些基础：

- `src/tool.rs` 里有 `ToolSpec`
- `src/session.rs` 已经能保存 `tool_calls`
- `src/app.rs` 已经能把 tool transcript 落盘和回放

但如果目标是“可审核”，现在还差几步。

### 4.1 新增显式审计事件

目前本项目的 session 还是偏“消息流”，不是“审计流”。

建议后续新增类似下面的事件类型：

- `tool_invoked`
- `tool_confirmed`
- `tool_denied`
- `tool_completed`
- `tool_failed`
- `network_requested`
- `network_approved`
- `file_changed`

这样比只存 message 更适合审计。

### 4.2 记录 policy snapshot

每次 tool 执行时，应把这些信息一起保存：

- 当前 `ToolSpec.side_effects`
- 当前 `ToolSpec.parallelism`
- 是否需要确认
- 是否自动确认
- 当前 sandbox / approval 配置

否则事后无法判断：

- 是 tool 本来就不需要确认
- 还是因为用户用了 `--yes`

### 4.3 对 write / bash / fetch 分开审计

本项目当前最应该优先加强的是：

- `write`
- `bash`
- `fetch`

因为这三个分别对应：

- 文件变更
- 本地执行
- 外部访问

它们不应该共用一套模糊的“执行成功/失败”记录，而应保留各自关键字段。

### 4.4 增加审计导出命令

后续可以考虑新增：

- `chat session audit <id>`
- `chat session export --audit`

输出格式建议优先支持：

- JSONL
- NDJSON

方便后续喂给日志系统、SIEM 或简单脚本分析。

## 5. 一个可执行的审计清单

如果你要人工审核一次 agent 操作，建议按下面顺序看：

1. 这次 turn 调用了哪些 tool
2. 每个 tool 的风险级别是什么
3. 是否发生了确认或提权
4. 如果确认了，是谁允许的
5. 是否发生了文件写入
6. 是否发生了命令执行
7. 是否发生了外部网络访问
8. tool output 是否和最终回答一致
9. transcript 是否完整可回放
10. 是否存在“读敏感信息 -> 执行 -> 外传”的组合链路

如果这十个问题里有三个以上回答不出来，就说明审计能力还不够。

## 6. 总结

Codex 更像：

- sandbox + approval + guardian review + structured event stream

Claude 更像：

- tool permission engine + dynamic rule updates + hooks + transcript persistence

如果把两者合在一起看，一个成熟 agent 系统的安全审计应该至少具备：

1. 明确的资源边界
2. 明确的审批策略
3. 工具级风险元数据
4. 可追溯的审批来源
5. 完整的 transcript / event 证据链

对本项目来说，当前最值得继续推进的不是“再多几个 tool”，而是把已有 tool 的：

- 权限
- 审批
- 审计事件
- 导出能力

真正做成一条闭环。

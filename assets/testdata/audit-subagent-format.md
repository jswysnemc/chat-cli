# Audit Subagent Format

当前自动审核链路在 [src/app.rs](../../src/app.rs) 和 [src/app.rs](../../src/app.rs)。

## 触发条件

- 只有 `config.audit.enabled = true` 时才会触发。
- 当前实现里，只有 `side_effects = mutating` 的 tool call 会进入审核。
- 现有内建工具里，稳定会进入这条链路的是 `Bash` 和 `Edit`。
- `Bash` 如果被判定为只读命令，会先在 [src/tool.rs](../../src/tool.rs) 被降级为 `read_only`，因此不会进入审核。

## 分组方式

审核会先按 prompt kind 分组，再分别调用审核模型：

- `Bash` / `bash` -> `audit-bash.md`
- `Edit` / `edit` / `Write` / `write` -> `audit-edit.md`
- 其他 mutating tool -> `audit-default.md`

对应代码在 [src/app.rs](../../src/app.rs) 和 [src/app.rs](../../src/app.rs)。

## 发给审核子 agent 的请求

实际发给审核模型的是一个标准聊天请求：

```json
{
  "messages": [
    {
      "role": "system",
      "content": "<prompt file content>"
    },
    {
      "role": "user",
      "content": "{...pretty printed payload json...}"
    }
  ],
  "temperature": 0.0,
  "max_output_tokens": 800,
  "tools": []
}
```

对应代码在 [src/app.rs](../../src/app.rs)。

## User Payload Schema

`user.content` 里承载的是 `build_tool_review_payload()` 生成的 JSON，对应 [src/app.rs](../../src/app.rs)。

```json
{
  "session_id": "sess_xxx",
  "tool_calls": [
    {
      "id": "call_xxx",
      "name": "Bash",
      "arguments": {
        "command": "..."
      },
      "side_effects": "mutating",
      "parallelism": "sequential_only",
      "requires_confirmation": true
    }
  ],
  "transcript": [
    {
      "role": "user",
      "content_preview": "..."
    },
    {
      "role": "assistant",
      "content_preview": "...",
      "tool_calls": [
        {
          "id": "call_read_xxx",
          "name": "Read",
          "arguments_preview": "{\"file_path\":\"/abs/path\"}",
          "side_effects": "read_only",
          "parallelism": "parallel_safe",
          "requires_confirmation": false
        }
      ]
    },
    {
      "role": "tool",
      "name": "Read",
      "side_effects": "read_only",
      "parallelism": "parallel_safe",
      "tool_call_id": "call_read_xxx",
      "content_preview": "..."
    }
  ]
}
```

补充约束：

- `transcript` 只保留最近 8 条消息。
- `transcript[*].content_preview` 是预览文本，不是完整原文。
- `tool_calls[*].arguments` 是完整参数。
- `transcript[*].tool_calls` 是摘要版，不是完整原始参数。

## 审核子 agent 的期望输出

三个 prompt 文件都要求只返回严格 JSON：

```json
{
  "results": [
    {
      "id": "tool_call_id",
      "verdict": "pass|warning|block",
      "message": "简短说明"
    }
  ]
}
```

对应 prompt 文件：

- [audit-default.md](../prompts/audit-default.md)
- [audit-bash.md](../prompts/audit-bash.md)
- [audit-edit.md](../prompts/audit-edit.md)

解析代码在 [src/app.rs](../../src/app.rs)。

## 解析后的内部结果

内部会把审核结果映射成：

```json
{
  "provider": "provider_id",
  "model": "model_id",
  "verdict": "pass|warning|block|unavailable",
  "message": "短摘要",
  "latency_ms": 12,
  "usage": {
    "input_tokens": 100,
    "output_tokens": 20,
    "total_tokens": 120
  }
}
```

如果审核模型返回非 JSON 或字段缺失：

- 默认降级成 `warning`
- `message` 变成 `需人工确认` 或 `审核返回异常`

## Session 落盘格式

审核结果会以 `SessionEvent::Audit` 写入会话，对应 [src/session.rs](../../src/session.rs)：

```json
{
  "type": "audit",
  "provider": "cpap",
  "model": "audit-model",
  "tool_name": "Bash",
  "tool_call_id": "call_bash_001",
  "verdict": "warning",
  "summary": "需人工确认",
  "findings": [],
  "recommendations": [],
  "latency_ms": 12,
  "usage": {
    "input_tokens": 100,
    "output_tokens": 20,
    "total_tokens": 120
  },
  "created_at": "2026-04-09T00:00:00Z"
}
```

## 测试数据文件

100 条样例放在：

- [audit-subagent-cases.jsonl](./audit-subagent-cases.jsonl)
- [audit-subagent-requests.jsonl](./audit-subagent-requests.jsonl)

辅助脚本：

- [build_audit_subagent_requests.py](../../scripts/build_audit_subagent_requests.py)
- [run_audit_subagent_benchmark.py](../../scripts/run_audit_subagent_benchmark.py)
- [eval_audit_subagent.py](../../scripts/eval_audit_subagent.py)

每一行结构：

```json
{
  "case_id": "audit_case_001",
  "prompt_kind": "bash|edit|default",
  "system_prompt_relpath": "assets/prompts/audit-bash.md",
  "scenario": "简短场景说明",
  "payload": { "...": "exact user payload object" },
  "expected_response": {
    "results": [
      {
        "id": "tool_call_id",
        "verdict": "pass|warning|block",
        "message": "简短说明"
      }
    ]
  }
}
```

`audit-subagent-requests.jsonl` 每一行会把上面的 case 展开成接近运行时的完整 `ChatRequest` 模板，便于你直接喂模型。

## 生成与评测

生成完整 requests：

```bash
python scripts/build_audit_subagent_requests.py
```

直接跑指定审核模型：

```bash
python scripts/run_audit_subagent_benchmark.py \
  --model minimax-m2-7 \
  --output assets/testdata/audit-subagent-predictions.jsonl
```

如果你想临时测试一个 provider + remote model，而不是走本地 models 配置：

```bash
python scripts/run_audit_subagent_benchmark.py \
  --provider deepseek \
  --remote-model deepseek-chat \
  --output assets/testdata/audit-subagent-predictions.jsonl
```

评测预测结果：

```bash
python scripts/eval_audit_subagent.py \
  --cases assets/testdata/audit-subagent-cases.jsonl \
  --predictions your_predictions.jsonl \
  --failures assets/testdata/audit-subagent-failures.jsonl
```

`predictions` 支持的单行格式至少要包含：

```json
{
  "case_id": "audit_case_001",
  "response": "{\"results\":[{\"id\":\"call_bash_001_1\",\"verdict\":\"pass\",\"message\":\"临时目录创建\"}]}"
}
```

也支持 `content`、`output`、`raw_response`、`parsed_response`、`expected_response` 这些字段名。

`run_audit_subagent_benchmark.py` 产出的单行格式大致是：

```json
{
  "case_id": "audit_case_001",
  "prompt_kind": "bash",
  "scenario": "创建临时缓存目录",
  "provider_id": "deepseek",
  "model_id": "minimax-m2-7",
  "provider_kind": "openai_compatible",
  "latency_ms": 420,
  "response": "{\"results\":[...]}",
  "parsed_response": {"results":[...]},
  "usage": {},
  "error": null
}
```

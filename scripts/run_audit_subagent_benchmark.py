#!/usr/bin/env python3
import argparse
import json
import os
import sys
import time
import tomllib
import urllib.error
import urllib.request
from pathlib import Path


def load_jsonl(path: Path):
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.strip():
            yield json.loads(line)


def default_config_dir() -> Path:
    home = Path(os.environ["HOME"])
    return Path(os.environ.get("XDG_CONFIG_HOME", home / ".config")) / "chat-cli"


def load_toml_or_empty(path: Path) -> dict:
    if not path.exists():
        return {}
    with path.open("rb") as fh:
        return tomllib.load(fh)


def provider_base_url(provider: dict) -> str:
    kind = provider.get("kind")
    if kind == "anthropic":
        return provider.get("base_url") or "https://api.anthropic.com/v1"
    if kind == "ollama":
        return provider.get("base_url") or "http://localhost:11434/api"
    base_url = provider.get("base_url")
    if not base_url:
        raise ValueError("provider.base_url is required for this provider")
    return base_url


def provider_allows_missing_api_key(provider: dict) -> bool:
    kind = provider.get("kind")
    if kind == "ollama":
        return True
    if kind != "openai_compatible":
        return False
    base_url = (provider.get("base_url") or "").lower()
    return (
        base_url.startswith("http://localhost")
        or base_url.startswith("http://127.0.0.1")
        or base_url.startswith("http://0.0.0.0")
    )


def resolve_api_key(provider_id: str, provider: dict, secrets: dict, override: str | None) -> str:
    if override is not None:
        return override
    env_name = provider.get("api_key_env")
    if env_name:
        value = os.environ.get(env_name, "")
        if value.strip():
            return value
    value = (
        secrets.get("providers", {})
        .get(provider_id, {})
        .get("api_key", "")
    )
    if isinstance(value, str) and value.strip():
        return value
    if provider_allows_missing_api_key(provider):
        return ""
    raise ValueError(
        f"missing API key for provider `{provider_id}`; configure secrets.toml or provider.api_key_env"
    )


def patched_messages(messages: list[dict], model: dict) -> list[dict]:
    patches = model.get("patches") or {}
    if not patches.get("system_to_user"):
        return messages
    patched = []
    for message in messages:
        cloned = dict(message)
        if cloned.get("role") == "system":
            cloned["role"] = "user"
        patched.append(cloned)
    return patched


def build_openai_body(messages: list[dict], model: dict, request_template: dict) -> dict:
    body = {
        "model": model["remote_name"],
        "messages": [{"role": msg["role"], "content": msg["content"]} for msg in messages],
        "stream": False,
    }
    if request_template.get("temperature") is not None:
        body["temperature"] = request_template["temperature"]
    if request_template.get("max_output_tokens") is not None:
        body["max_tokens"] = request_template["max_output_tokens"]
    reasoning_effort = model.get("reasoning_effort")
    capabilities = model.get("capabilities") or []
    params = request_template.get("params") or {}
    if reasoning_effort and "reasoning_effort" not in params and "thinking" not in params:
        body["reasoning_effort"] = reasoning_effort
    elif reasoning_effort is None and "reasoning" not in capabilities:
        pass
    body.update(params)
    return body


def split_system_messages(messages: list[dict]) -> tuple[str | None, list[dict]]:
    system_parts = []
    result = []
    for message in messages:
        if message["role"] == "system":
            system_parts.append(message["content"])
        else:
            result.append({"role": message["role"], "content": message["content"]})
    return ("\n\n".join(system_parts) if system_parts else None, result)


def build_anthropic_body(messages: list[dict], model: dict, request_template: dict) -> dict:
    system, payload_messages = split_system_messages(messages)
    body = {
        "model": model["remote_name"],
        "max_tokens": request_template.get("max_output_tokens") or model.get("max_output_tokens") or 1024,
        "messages": payload_messages,
    }
    if system:
        body["system"] = system
    if request_template.get("temperature") is not None:
        body["temperature"] = request_template["temperature"]
    body.update(request_template.get("params") or {})
    return body


def build_ollama_body(messages: list[dict], model: dict, request_template: dict) -> dict:
    body = {
        "model": model["remote_name"],
        "messages": [{"role": msg["role"], "content": msg["content"]} for msg in messages],
        "stream": False,
    }
    if request_template.get("temperature") is not None:
        body["options"] = {"temperature": request_template["temperature"]}
    body.update(request_template.get("params") or {})
    return body


def build_headers(provider: dict, api_key: str) -> dict:
    headers = {"Content-Type": "application/json"}
    kind = provider["kind"]
    if kind == "openai_compatible":
        if api_key.strip():
            headers["Authorization"] = f"Bearer {api_key}"
        if provider.get("org"):
            headers["OpenAI-Organization"] = provider["org"]
        if provider.get("project"):
            headers["OpenAI-Project"] = provider["project"]
    elif kind == "anthropic":
        if not api_key.strip():
            raise ValueError("missing API key for anthropic provider")
        headers["x-api-key"] = api_key
        headers["anthropic-version"] = "2023-06-01"
    elif kind == "ollama":
        if api_key.strip():
            headers["Authorization"] = f"Bearer {api_key}"
    else:
        raise ValueError(f"unsupported provider kind `{kind}`")

    for key, value in (provider.get("headers") or {}).items():
        headers[key] = value
    return headers


def send_json(url: str, headers: dict, body: dict, timeout_secs: int | None) -> tuple[dict, int]:
    data = json.dumps(body, ensure_ascii=False).encode("utf-8")
    request = urllib.request.Request(url, data=data, headers=headers, method="POST")
    started = time.time()
    with urllib.request.urlopen(request, timeout=timeout_secs) as response:
        raw = response.read()
    latency_ms = int((time.time() - started) * 1000)
    return json.loads(raw.decode("utf-8")), latency_ms


def extract_openai_text(raw: dict) -> str:
    content = raw.get("choices", [{}])[0].get("message", {}).get("content")
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "".join(
            item.get("text", "")
            for item in content
            if isinstance(item, dict) and item.get("type") == "text"
        )
    return ""


def extract_anthropic_text(raw: dict) -> str:
    parts = raw.get("content", [])
    if isinstance(parts, list):
        return "".join(
            item.get("text", "")
            for item in parts
            if isinstance(item, dict) and item.get("type") == "text"
        )
    return ""


def extract_ollama_text(raw: dict) -> str:
    return raw.get("message", {}).get("content", "") if isinstance(raw, dict) else ""


def parse_json_response_text(text: str) -> dict | None:
    try:
        parsed = json.loads(text)
        return parsed if isinstance(parsed, dict) else None
    except json.JSONDecodeError:
        start = text.find("{")
        end = text.rfind("}")
        if start == -1 or end == -1 or end < start:
            return None
        try:
            parsed = json.loads(text[start : end + 1])
            return parsed if isinstance(parsed, dict) else None
        except json.JSONDecodeError:
            return None


def resolve_target(config: dict, secrets: dict, args) -> tuple[str, dict, str, dict, str]:
    providers = config.get("providers", {})
    models = config.get("models", {})

    if args.remote_model:
        if not args.provider:
            raise ValueError("--provider is required when using --remote-model")
        provider_id = args.provider
        provider = providers.get(provider_id)
        if provider is None:
            raise ValueError(f"provider `{provider_id}` does not exist in config")
        model_id = args.remote_model
        model = {
            "provider": provider_id,
            "remote_name": args.remote_model,
            "max_output_tokens": args.max_output_tokens,
            "temperature": None,
            "capabilities": [],
            "patches": {},
        }
    else:
        model_id = (
            args.model
            or (config.get("audit", {}) or {}).get("model")
            or (config.get("defaults", {}) or {}).get("model")
        )
        if not model_id:
            raise ValueError("no model specified; use --model or configure audit.model/defaults.model")
        model = models.get(model_id)
        if model is None:
            raise ValueError(f"model `{model_id}` does not exist in config")
        provider_id = model["provider"]
        provider = providers.get(provider_id)
        if provider is None:
            raise ValueError(f"provider `{provider_id}` referenced by model `{model_id}` is missing")

    api_key = resolve_api_key(provider_id, provider, secrets, args.api_key)
    return provider_id, provider, model_id, model, api_key


def maybe_load_existing(path: Path) -> set[str]:
    if not path.exists():
        return set()
    return {row["case_id"] for row in load_jsonl(path) if "case_id" in row}


def run_case(case: dict, provider_id: str, provider: dict, model_id: str, model: dict, api_key: str, timeout_secs: int | None) -> dict:
    request_template = case["chat_request_template"]
    messages = patched_messages(request_template["messages"], model)
    kind = provider["kind"]
    base_url = provider_base_url(provider).rstrip("/")
    if kind == "openai_compatible":
        url = f"{base_url}/chat/completions"
        body = build_openai_body(messages, model, request_template)
    elif kind == "anthropic":
        url = f"{base_url}/messages"
        body = build_anthropic_body(messages, model, request_template)
    elif kind == "ollama":
        url = f"{base_url}/chat"
        body = build_ollama_body(messages, model, request_template)
    else:
        raise ValueError(f"unsupported provider kind `{kind}`")

    headers = build_headers(provider, api_key)
    try:
        raw, latency_ms = send_json(url, headers, body, timeout_secs)
        if kind == "openai_compatible":
            content = extract_openai_text(raw)
            usage = raw.get("usage", {})
        elif kind == "anthropic":
            content = extract_anthropic_text(raw)
            usage = raw.get("usage", {})
        else:
            content = extract_ollama_text(raw)
            usage = raw.get("usage", {})

        return {
            "case_id": case["case_id"],
            "prompt_kind": case["prompt_kind"],
            "scenario": case["scenario"],
            "provider_id": provider_id,
            "model_id": model_id,
            "provider_kind": kind,
            "latency_ms": latency_ms,
            "response": content,
            "parsed_response": parse_json_response_text(content),
            "usage": usage if isinstance(usage, dict) else {},
            "error": None,
        }
    except urllib.error.HTTPError as err:
        body = err.read().decode("utf-8", errors="replace")
        return {
            "case_id": case["case_id"],
            "prompt_kind": case["prompt_kind"],
            "scenario": case["scenario"],
            "provider_id": provider_id,
            "model_id": model_id,
            "provider_kind": kind,
            "response": "",
            "parsed_response": None,
            "usage": {},
            "error": f"HTTP {err.code}: {body}",
        }
    except Exception as err:
        return {
            "case_id": case["case_id"],
            "prompt_kind": case["prompt_kind"],
            "scenario": case["scenario"],
            "provider_id": provider_id,
            "model_id": model_id,
            "provider_kind": kind,
            "response": "",
            "parsed_response": None,
            "usage": {},
            "error": str(err),
        }


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Run the audit-subagent benchmark cases against a configured model."
    )
    parser.add_argument(
        "--requests",
        default="assets/testdata/audit-subagent-requests.jsonl",
        help="Input request JSONL path.",
    )
    parser.add_argument(
        "--output",
        default="assets/testdata/audit-subagent-predictions.jsonl",
        help="Output prediction JSONL path.",
    )
    parser.add_argument(
        "--config-dir",
        help="chat-cli config directory. Defaults to XDG config home / ~/.config/chat-cli.",
    )
    parser.add_argument(
        "--model",
        help="Local model id from config.toml. Defaults to audit.model, then defaults.model.",
    )
    parser.add_argument(
        "--provider",
        help="Provider id from config.toml. Required only with --remote-model.",
    )
    parser.add_argument(
        "--remote-model",
        help="Remote model name to use with --provider, without requiring a local model entry.",
    )
    parser.add_argument(
        "--api-key",
        help="Override API key instead of reading secrets.toml or provider.api_key_env.",
    )
    parser.add_argument(
        "--timeout-secs",
        type=int,
        help="Override request timeout in seconds. Use 0 for no timeout.",
    )
    parser.add_argument(
        "--max-output-tokens",
        type=int,
        default=800,
        help="Used only with --remote-model. Defaults to 800.",
    )
    parser.add_argument(
        "--limit",
        type=int,
        help="Run only the first N cases after filtering.",
    )
    parser.add_argument(
        "--offset",
        type=int,
        default=0,
        help="Skip the first N cases.",
    )
    parser.add_argument(
        "--case-id",
        action="append",
        help="Run only the specified case_id. Can be used multiple times.",
    )
    parser.add_argument(
        "--skip-existing",
        action="store_true",
        help="Skip case_ids that already exist in the output JSONL.",
    )
    args = parser.parse_args()

    repo_root = Path(__file__).resolve().parent.parent
    config_dir = Path(args.config_dir).expanduser() if args.config_dir else default_config_dir()
    config = load_toml_or_empty(config_dir / "config.toml")
    secrets = load_toml_or_empty(config_dir / "secrets.toml")
    provider_id, provider, model_id, model, api_key = resolve_target(config, secrets, args)

    requests_path = (repo_root / args.requests).resolve()
    output_path = (repo_root / args.output).resolve()
    output_path.parent.mkdir(parents=True, exist_ok=True)

    rows = list(load_jsonl(requests_path))
    if args.case_id:
        allowed = set(args.case_id)
        rows = [row for row in rows if row["case_id"] in allowed]
    if args.offset:
        rows = rows[args.offset :]
    if args.limit is not None:
        rows = rows[: args.limit]
    if args.skip_existing:
        existing = maybe_load_existing(output_path)
        rows = [row for row in rows if row["case_id"] not in existing]

    timeout_secs = args.timeout_secs
    if timeout_secs is None:
        timeout_secs = provider.get("timeout")
    if timeout_secs == 0:
        timeout_secs = None

    print(
        f"running {len(rows)} case(s) with provider={provider_id} model={model_id} kind={provider['kind']}",
        file=sys.stderr,
    )
    with output_path.open("a", encoding="utf-8") as fh:
        for index, row in enumerate(rows, 1):
            result = run_case(
                row,
                provider_id=provider_id,
                provider=provider,
                model_id=model_id,
                model=model,
                api_key=api_key,
                timeout_secs=timeout_secs,
            )
            fh.write(json.dumps(result, ensure_ascii=False, separators=(",", ":")) + "\n")
            fh.flush()
            status = "ok" if not result["error"] else "error"
            print(f"[{index}/{len(rows)}] {row['case_id']} {status}", file=sys.stderr)

    print(f"wrote predictions to {output_path}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

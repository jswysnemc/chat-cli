#!/usr/bin/env python3
import argparse
import json
from pathlib import Path


def load_jsonl(path: Path):
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.strip():
            yield json.loads(line)


def build_request(case: dict, repo_root: Path) -> dict:
    prompt_path = repo_root / case["system_prompt_relpath"]
    prompt = prompt_path.read_text(encoding="utf-8")
    payload = case["payload"]
    pretty_payload = json.dumps(payload, ensure_ascii=False, indent=2)
    return {
        "case_id": case["case_id"],
        "prompt_kind": case["prompt_kind"],
        "scenario": case["scenario"],
        "system_prompt_relpath": case["system_prompt_relpath"],
        "chat_request_template": {
            "provider_id": "<audit-provider-id>",
            "model_id": "<audit-model-id>",
            "api_key": "<audit-api-key>",
            "messages": [
                {
                    "role": "system",
                    "content": prompt,
                    "images": [],
                    "tool_calls": None,
                    "tool_call_id": None,
                    "name": None,
                },
                {
                    "role": "user",
                    "content": pretty_payload,
                    "images": [],
                    "tool_calls": None,
                    "tool_call_id": None,
                    "name": None,
                },
            ],
            "temperature": 0.0,
            "max_output_tokens": 800,
            "params": {},
            "timeout_secs": "<inherit-from-parent-request>",
            "tools": [],
        },
        "expected_response": case["expected_response"],
    }


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Expand audit-subagent case data into full chat request templates."
    )
    parser.add_argument(
        "--cases",
        default="assets/testdata/audit-subagent-cases.jsonl",
        help="Input case JSONL path.",
    )
    parser.add_argument(
        "--output",
        default="assets/testdata/audit-subagent-requests.jsonl",
        help="Output request JSONL path.",
    )
    args = parser.parse_args()

    repo_root = Path(__file__).resolve().parent.parent
    cases_path = (repo_root / args.cases).resolve()
    output_path = (repo_root / args.output).resolve()
    output_path.parent.mkdir(parents=True, exist_ok=True)

    requests = [build_request(case, repo_root) for case in load_jsonl(cases_path)]
    with output_path.open("w", encoding="utf-8") as fh:
        for item in requests:
            fh.write(json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n")

    print(f"wrote {len(requests)} requests to {output_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

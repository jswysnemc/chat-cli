#!/usr/bin/env python3
import argparse
import json
from collections import Counter
from pathlib import Path


def load_jsonl(path: Path):
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.strip():
            yield json.loads(line)


def extract_json_object(text: str) -> str | None:
    start = text.find("{")
    end = text.rfind("}")
    if start == -1 or end == -1 or end < start:
        return None
    return text[start : end + 1]


def normalize_verdict(verdict: str | None) -> str:
    value = (verdict or "warning").strip().lower()
    if not value:
        return "warning"
    return value


def parse_agent_output(record: dict) -> dict:
    if "expected_response" in record:
        raw = record["expected_response"]
    elif "parsed_response" in record:
        raw = record["parsed_response"]
    elif "response" in record and isinstance(record["response"], dict):
        raw = record["response"]
    else:
        text = (
            record.get("response")
            or record.get("content")
            or record.get("output")
            or record.get("raw_response")
            or ""
        )
        parsed = None
        if isinstance(text, str):
            try:
                parsed = json.loads(text)
            except json.JSONDecodeError:
                wrapped = extract_json_object(text)
                if wrapped:
                    try:
                        parsed = json.loads(wrapped)
                    except json.JSONDecodeError:
                        parsed = None
        raw = parsed if isinstance(parsed, dict) else {"results": []}

    items = raw.get("results", []) if isinstance(raw, dict) else []
    result_map = {}
    for item in items:
        if not isinstance(item, dict):
            continue
        tool_call_id = item.get("id")
        if not tool_call_id:
            continue
        result_map[tool_call_id] = {
            "verdict": normalize_verdict(item.get("verdict")),
            "message": (item.get("message") or "").strip(),
        }
    return result_map


def expected_map(case: dict) -> dict:
    mapping = {}
    for item in case["expected_response"]["results"]:
        mapping[item["id"]] = {
            "verdict": normalize_verdict(item.get("verdict")),
            "message": (item.get("message") or "").strip(),
        }
    return mapping


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Evaluate audit-subagent predictions against the case dataset."
    )
    parser.add_argument(
        "--cases",
        default="assets/testdata/audit-subagent-cases.jsonl",
        help="Ground-truth case JSONL path.",
    )
    parser.add_argument(
        "--predictions",
        required=True,
        help="Prediction JSONL path. Each line should include case_id plus response/content/output.",
    )
    parser.add_argument(
        "--failures",
        help="Optional output JSONL path for mismatched cases.",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Print the summary as JSON.",
    )
    args = parser.parse_args()

    repo_root = Path(__file__).resolve().parent.parent
    cases = {case["case_id"]: case for case in load_jsonl((repo_root / args.cases).resolve())}
    predictions = {
        row["case_id"]: row for row in load_jsonl((repo_root / args.predictions).resolve())
    }

    total_cases = len(cases)
    matched_cases = 0
    exact_case_matches = 0
    verdict_total = 0
    verdict_correct = 0
    message_correct = 0
    expected_counter = Counter()
    predicted_counter = Counter()
    failures = []

    for case_id, case in cases.items():
        expected = expected_map(case)
        expected_counter.update(item["verdict"] for item in expected.values())
        pred_record = predictions.get(case_id)
        if pred_record is None:
            failures.append(
                {
                    "case_id": case_id,
                    "reason": "missing_prediction",
                    "expected": expected,
                    "predicted": {},
                }
            )
            verdict_total += len(expected)
            continue

        matched_cases += 1
        predicted = parse_agent_output(pred_record)
        predicted_counter.update(item["verdict"] for item in predicted.values())

        case_exact = True
        for tool_call_id, expected_item in expected.items():
            verdict_total += 1
            predicted_item = predicted.get(
                tool_call_id, {"verdict": "warning", "message": ""}
            )
            if predicted_item["verdict"] == expected_item["verdict"]:
                verdict_correct += 1
            else:
                case_exact = False
            if predicted_item["message"] == expected_item["message"]:
                message_correct += 1
            else:
                case_exact = False

        extra_ids = sorted(set(predicted) - set(expected))
        if extra_ids:
            case_exact = False

        if case_exact:
            exact_case_matches += 1
        else:
            failures.append(
                {
                    "case_id": case_id,
                    "reason": "mismatch",
                    "expected": expected,
                    "predicted": predicted,
                    "extra_tool_call_ids": extra_ids,
                }
            )

    summary = {
        "total_cases": total_cases,
        "matched_cases": matched_cases,
        "missing_cases": total_cases - matched_cases,
        "exact_case_match_count": exact_case_matches,
        "exact_case_match_rate": round(exact_case_matches / total_cases, 4)
        if total_cases
        else 0.0,
        "verdict_accuracy": round(verdict_correct / verdict_total, 4)
        if verdict_total
        else 0.0,
        "message_accuracy": round(message_correct / verdict_total, 4)
        if verdict_total
        else 0.0,
        "expected_verdict_distribution": dict(expected_counter),
        "predicted_verdict_distribution": dict(predicted_counter),
        "failure_count": len(failures),
    }

    if args.failures:
        failure_path = (repo_root / args.failures).resolve()
        failure_path.parent.mkdir(parents=True, exist_ok=True)
        with failure_path.open("w", encoding="utf-8") as fh:
            for item in failures:
                fh.write(json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n")

    if args.json:
        print(json.dumps(summary, ensure_ascii=False, indent=2))
    else:
        print(f"total_cases: {summary['total_cases']}")
        print(f"matched_cases: {summary['matched_cases']}")
        print(f"missing_cases: {summary['missing_cases']}")
        print(f"exact_case_match_count: {summary['exact_case_match_count']}")
        print(f"exact_case_match_rate: {summary['exact_case_match_rate']}")
        print(f"verdict_accuracy: {summary['verdict_accuracy']}")
        print(f"message_accuracy: {summary['message_accuracy']}")
        print(
            "expected_verdict_distribution: "
            + json.dumps(summary["expected_verdict_distribution"], ensure_ascii=False)
        )
        print(
            "predicted_verdict_distribution: "
            + json.dumps(summary["predicted_verdict_distribution"], ensure_ascii=False)
        )
        print(f"failure_count: {summary['failure_count']}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())

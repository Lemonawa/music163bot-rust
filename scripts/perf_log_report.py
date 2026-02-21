#!/usr/bin/env python3
"""Parse structured PERF logs and report p50/p95/max by stage/topology."""

from __future__ import annotations

import argparse
import json
import math
import unittest
from collections import defaultdict
from pathlib import Path
from typing import Any


def percentile(values: list[float], ratio: float) -> float:
    if not values:
        return 0.0
    sorted_values = sorted(values)
    if len(sorted_values) == 1:
        return sorted_values[0]

    rank = (len(sorted_values) - 1) * ratio
    low = int(math.floor(rank))
    high = min(low + 1, len(sorted_values) - 1)
    frac = rank - low
    return sorted_values[low] * (1.0 - frac) + sorted_values[high] * frac


def parse_perf_line(line: str) -> dict[str, Any] | None:
    marker = "PERF|"
    idx = line.find(marker)
    if idx < 0:
        return None

    payload = line[idx + len(marker) :].strip()
    fields: dict[str, str] = {}
    for item in payload.split("|"):
        if "=" not in item:
            continue
        key, value = item.split("=", 1)
        fields[key.strip()] = value.strip()

    required = ("trace_id", "music_id", "topology", "cache_path", "stage", "elapsed_ms")
    if any(key not in fields for key in required):
        return None

    try:
        elapsed_ms = float(fields["elapsed_ms"])
    except ValueError:
        return None

    return {
        "trace_id": fields["trace_id"],
        "music_id": fields["music_id"],
        "topology": fields["topology"],
        "cache_path": fields["cache_path"],
        "stage": fields["stage"],
        "elapsed_ms": elapsed_ms,
    }


def load_entries(path: Path) -> list[dict[str, Any]]:
    entries: list[dict[str, Any]] = []
    with path.open("r", encoding="utf-8", errors="replace") as handle:
        for line in handle:
            parsed = parse_perf_line(line)
            if parsed is not None:
                entries.append(parsed)
    return entries


def summarize_samples(samples: list[float]) -> dict[str, float]:
    if not samples:
        return {"count": 0, "p50": 0.0, "p95": 0.0, "max": 0.0}

    sorted_samples = sorted(samples)
    return {
        "count": float(len(sorted_samples)),
        "p50": percentile(sorted_samples, 0.50),
        "p95": percentile(sorted_samples, 0.95),
        "max": sorted_samples[-1],
    }


def aggregate(
    entries: list[dict[str, Any]],
    topologies: set[str] | None,
    cache_paths: set[str] | None,
) -> dict[str, Any]:
    filtered = entries
    if topologies:
        filtered = [entry for entry in entries if entry["topology"] in topologies]
    if cache_paths:
        filtered = [entry for entry in filtered if entry["cache_path"] in cache_paths]

    by_topology_stage: dict[str, dict[str, list[float]]] = defaultdict(
        lambda: defaultdict(list)
    )
    overall_by_stage: dict[str, list[float]] = defaultdict(list)
    traces_by_topology: dict[str, set[str]] = defaultdict(set)

    for entry in filtered:
        topology = entry["topology"]
        stage = entry["stage"]
        elapsed_ms = float(entry["elapsed_ms"])
        by_topology_stage[topology][stage].append(elapsed_ms)
        overall_by_stage[stage].append(elapsed_ms)
        traces_by_topology[topology].add(str(entry["trace_id"]))

    topology_stage_stats: dict[str, dict[str, dict[str, float]]] = {}
    topology_e2e_stats: dict[str, dict[str, float]] = {}
    for topology, stage_map in by_topology_stage.items():
        topology_stage_stats[topology] = {}
        for stage, samples in stage_map.items():
            topology_stage_stats[topology][stage] = summarize_samples(samples)
        topology_e2e_stats[topology] = summarize_samples(stage_map.get("e2e_total", []))
        topology_e2e_stats[topology]["trace_count"] = float(len(traces_by_topology[topology]))

    overall_stage_stats = {
        stage: summarize_samples(samples) for stage, samples in overall_by_stage.items()
    }

    return {
        "entries": len(filtered),
        "topology_stage_stats": topology_stage_stats,
        "topology_e2e_stats": topology_e2e_stats,
        "overall_stage_stats": overall_stage_stats,
    }


def to_markdown(
    report: dict[str, Any],
    source: str,
    topologies: set[str] | None,
    cache_paths: set[str] | None,
) -> str:
    lines: list[str] = [
        "# Real E2E Cold Path Perf Report",
        "",
        f"- Source log: `{source}`",
        f"- Topology filter: `{','.join(sorted(topologies)) if topologies else 'all'}`",
        f"- Cache path filter: `{','.join(sorted(cache_paths)) if cache_paths else 'all'}`",
        f"- Parsed PERF entries: `{report['entries']}`",
        "",
    ]

    topology_stage_stats: dict[str, dict[str, dict[str, float]]] = report["topology_stage_stats"]
    topology_e2e_stats: dict[str, dict[str, float]] = report["topology_e2e_stats"]
    for topology in sorted(topology_stage_stats):
        lines.extend(
            [
                f"## Topology: `{topology}`",
                "",
                "| Stage | Count | P50 (ms) | P95 (ms) | Max (ms) |",
                "|---|---:|---:|---:|---:|",
            ]
        )
        for stage in sorted(topology_stage_stats[topology]):
            stats = topology_stage_stats[topology][stage]
            lines.append(
                f"| {stage} | {int(stats['count'])} | {stats['p50']:.2f} | {stats['p95']:.2f} | {stats['max']:.2f} |"
            )

        e2e = topology_e2e_stats[topology]
        lines.extend(
            [
                "",
                f"- e2e_total p95: `{e2e['p95']:.2f} ms`",
                f"- e2e_total p50: `{e2e['p50']:.2f} ms`",
                f"- e2e_total max: `{e2e['max']:.2f} ms`",
                f"- trace_count: `{int(e2e['trace_count'])}`",
                "",
            ]
        )

    overall_stage_stats: dict[str, dict[str, float]] = report["overall_stage_stats"]
    lines.extend(
        [
            "## Overall Stage Stats",
            "",
            "| Stage | Count | P50 (ms) | P95 (ms) | Max (ms) |",
            "|---|---:|---:|---:|---:|",
        ]
    )
    for stage in sorted(overall_stage_stats):
        stats = overall_stage_stats[stage]
        lines.append(
            f"| {stage} | {int(stats['count'])} | {stats['p50']:.2f} | {stats['p95']:.2f} | {stats['max']:.2f} |"
        )
    lines.append("")
    return "\n".join(lines)


class PerfLogReportTests(unittest.TestCase):
    def test_parse_perf_line(self) -> None:
        line = (
            "2026-02-20T00:00:00Z INFO PERF|trace_id=t1|music_id=42|topology=official_api|"
            "cache_path=miss_cold|stage=e2e_total|elapsed_ms=123"
        )
        parsed = parse_perf_line(line)
        assert parsed is not None
        self.assertEqual(parsed["trace_id"], "t1")
        self.assertEqual(parsed["stage"], "e2e_total")
        self.assertEqual(parsed["elapsed_ms"], 123.0)

    def test_aggregate_with_topology_filter(self) -> None:
        entries = [
            {
                "trace_id": "a",
                "music_id": "1",
                "topology": "official_api",
                "cache_path": "miss_cold",
                "stage": "e2e_total",
                "elapsed_ms": 100.0,
            },
            {
                "trace_id": "b",
                "music_id": "2",
                "topology": "selfhost_api_uri_upload",
                "cache_path": "miss_cold",
                "stage": "e2e_total",
                "elapsed_ms": 50.0,
            },
        ]
        report = aggregate(entries, {"official_api"}, None)
        self.assertEqual(report["entries"], 1)
        self.assertIn("official_api", report["topology_stage_stats"])
        self.assertNotIn("selfhost_api_uri_upload", report["topology_stage_stats"])
        self.assertEqual(
            report["topology_stage_stats"]["official_api"]["e2e_total"]["p95"], 100.0
        )

    def test_percentile_interpolation(self) -> None:
        samples = [10.0, 20.0, 30.0, 40.0]
        self.assertAlmostEqual(percentile(samples, 0.5), 25.0)
        self.assertAlmostEqual(percentile(samples, 0.95), 38.5)


def run_self_test() -> int:
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(PerfLogReportTests)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if result.wasSuccessful() else 1


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Generate report from structured PERF logs")
    parser.add_argument("--log-file", type=Path, help="Path to bot log file")
    parser.add_argument(
        "--topology",
        action="append",
        default=[],
        help="Filter topology (repeatable): official_api/selfhost_api_uri_upload/selfhost_api_multipart_upload",
    )
    parser.add_argument(
        "--cache-path",
        action="append",
        default=[],
        help="Filter cache path (repeatable): miss_cold/hit_pre_singleflight/etc.",
    )
    parser.add_argument("--json-output", type=Path, help="Optional JSON output path")
    parser.add_argument("--markdown-output", type=Path, help="Optional Markdown output path")
    parser.add_argument("--print-json", action="store_true", help="Print JSON report")
    parser.add_argument("--self-test", action="store_true", help="Run script unit tests")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.self_test:
        return run_self_test()

    if args.log_file is None:
        raise SystemExit("--log-file is required unless --self-test is used")

    entries = load_entries(args.log_file)
    topology_filter = set(args.topology) if args.topology else None
    cache_path_filter = set(args.cache_path) if args.cache_path else None
    report = aggregate(entries, topology_filter, cache_path_filter)
    markdown = to_markdown(report, str(args.log_file), topology_filter, cache_path_filter)

    if args.json_output:
        args.json_output.parent.mkdir(parents=True, exist_ok=True)
        args.json_output.write_text(json.dumps(report, indent=2), encoding="utf-8")

    if args.markdown_output:
        args.markdown_output.parent.mkdir(parents=True, exist_ok=True)
        args.markdown_output.write_text(markdown, encoding="utf-8")

    print(markdown)
    if args.print_json:
        print(json.dumps(report, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

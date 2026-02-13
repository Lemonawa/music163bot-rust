#!/usr/bin/env python3
"""
Telegram upload benchmark for Rust-vs-Go race testing.

What it measures:
1) Pure memory clone overhead (no network)
2) Real Telegram upload latency under three modes:
   - file          : stream from file path (Go-like)
   - memory        : reuse one in-memory buffer (no per-run clone)
   - memory-clone  : clone in-memory buffer each run (Rust to_input_file clone-like)

This script uses only Python stdlib.
"""

from __future__ import annotations

import argparse
import http.client
import json
import mimetypes
import os
import re
import ssl
import statistics
import sys
import time
import urllib.parse
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable


class RateLimitError(RuntimeError):
    def __init__(self, retry_after_seconds: float, payload: dict | None = None):
        super().__init__(f"Rate limited, retry after {retry_after_seconds:.2f}s")
        self.retry_after_seconds = max(0.0, float(retry_after_seconds))
        self.payload = payload or {}


def parse_retry_after_seconds(payload: dict | None) -> float:
    if not isinstance(payload, dict):
        return 1.0

    params = payload.get("parameters")
    if isinstance(params, dict) and "retry_after" in params:
        try:
            return max(0.0, float(params["retry_after"]))
        except (TypeError, ValueError):
            pass

    description = str(payload.get("description", ""))
    match = re.search(r"retry\s+after\s+(\d+)", description, flags=re.IGNORECASE)
    if match:
        try:
            return max(0.0, float(match.group(1)))
        except ValueError:
            pass

    return 1.0


def percentile(values: list[float], p: float) -> float:
    if not values:
        return 0.0
    if len(values) == 1:
        return values[0]
    rank = (len(values) - 1) * p
    low = int(rank)
    high = min(low + 1, len(values) - 1)
    frac = rank - low
    return values[low] * (1.0 - frac) + values[high] * frac


@dataclass
class Stat:
    avg: float
    p50: float
    p95: float
    min_v: float
    max_v: float


def summarize(values: Iterable[float]) -> Stat:
    arr = sorted(float(x) for x in values)
    if not arr:
        return Stat(0.0, 0.0, 0.0, 0.0, 0.0)
    return Stat(
        avg=statistics.fmean(arr),
        p50=percentile(arr, 0.50),
        p95=percentile(arr, 0.95),
        min_v=arr[0],
        max_v=arr[-1],
    )


def print_stat(name: str, stat: Stat, unit: str = "ms") -> None:
    print(
        f"{name:>14}: avg={stat.avg:.2f}{unit} p50={stat.p50:.2f}{unit} "
        f"p95={stat.p95:.2f}{unit} min={stat.min_v:.2f}{unit} max={stat.max_v:.2f}{unit}"
    )


def stat_to_payload(stat: Stat, *, unit: str = "ms") -> dict[str, float | str]:
    return {
        "avg": stat.avg,
        "p50": stat.p50,
        "p95": stat.p95,
        "min": stat.min_v,
        "max": stat.max_v,
        "unit": unit,
    }


def render_markdown_report(payload: dict[str, Any]) -> str:
    meta = payload["meta"]
    local_clone = payload["local_clone"]
    modes = payload["modes"]
    comparisons = payload["comparisons"]

    lines: list[str] = [
        "# Telegram Upload Benchmark Report",
        "",
        "## Run Config",
        "",
        "| Key | Value |",
        "|---|---|",
        f"| api_base | `{meta['api_base']}` |",
        f"| method | `{meta['method']}` |",
        f"| file | `{meta['file']}` |",
        f"| file_size_mb | {meta['file_size_mb']:.2f} |",
        f"| runs_per_mode | {meta['runs_per_mode']} |",
        f"| modes | {', '.join(meta['modes'])} |",
        f"| reuse_connection | {meta['reuse_connection']} |",
        f"| between_runs_ms | {meta['between_runs_ms']} |",
        f"| max_rate_retries | {meta['max_rate_retries']} |",
        f"| retry_after_padding_ms | {meta['retry_after_padding_ms']} |",
        f"| delete_after_send | {meta['delete_after_send']} |",
        "",
        "## Local Clone Benchmark",
        "",
        "| Metric | Value (ms) |",
        "|---|---:|",
        f"| avg | {local_clone['avg']:.3f} |",
        f"| p50 | {local_clone['p50']:.3f} |",
        f"| p95 | {local_clone['p95']:.3f} |",
        f"| min | {local_clone['min']:.3f} |",
        f"| max | {local_clone['max']:.3f} |",
        "",
        "## Mode Upload Results",
        "",
        "| Mode | avg (ms) | p50 (ms) | p95 (ms) | min (ms) | max (ms) | Throughput (MB/s) |",
        "|---|---:|---:|---:|---:|---:|---:|",
    ]
    for mode_name, mode_payload in modes.items():
        upload = mode_payload["upload"]
        lines.append(
            f"| {mode_name} | {upload['avg']:.2f} | {upload['p50']:.2f} | "
            f"{upload['p95']:.2f} | {upload['min']:.2f} | {upload['max']:.2f} | "
            f"{mode_payload['throughput_mb_s_avg']:.2f} |"
        )

    clone_mode_rows = [
        (mode_name, mode_payload)
        for mode_name, mode_payload in modes.items()
        if "clone" in mode_payload
    ]
    if clone_mode_rows:
        lines.extend(
            [
                "",
                "## Clone Cost In Upload Modes",
                "",
                "| Mode | clone avg (ms) | clone p95 (ms) | clone share of upload avg (%) |",
                "|---|---:|---:|---:|",
            ]
        )
        for mode_name, mode_payload in clone_mode_rows:
            clone_stats = mode_payload["clone"]
            lines.append(
                f"| {mode_name} | {clone_stats['avg']:.3f} | {clone_stats['p95']:.3f} | "
                f"{mode_payload['clone_share_percent_avg']:.4f} |"
            )

    delta_ms = comparisons.get("memory_clone_vs_file_avg_ms_delta")
    relative_pct = comparisons.get("memory_clone_vs_file_relative_percent")
    if delta_ms is not None and relative_pct is not None:
        lines.extend(
            [
                "",
                "## file vs memory-clone",
                "",
                f"- avg delta (memory-clone - file): {delta_ms:+.2f} ms",
                f"- relative delta: {relative_pct:+.2f}%",
            ]
        )

    return "\n".join(lines) + "\n"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Benchmark Telegram upload path")
    parser.add_argument(
        "--api-base", default="https://api.telegram.org", help="Telegram API base"
    )
    parser.add_argument("--token", default=os.getenv("BOT_TOKEN", ""), help="Bot token")
    parser.add_argument(
        "--chat-id", default=os.getenv("CHAT_ID", ""), help="Target chat id"
    )
    parser.add_argument("--file", required=True, help="Audio file path")
    parser.add_argument(
        "--method", default="sendAudio", choices=["sendAudio", "sendDocument"]
    )
    parser.add_argument("--runs", type=int, default=6, help="Measured runs per mode")
    parser.add_argument(
        "--warmup", action="store_true", help="Run getMe before benchmarking"
    )
    parser.add_argument(
        "--modes",
        default="file,memory-clone",
        help="Comma-separated: file,memory,memory-clone",
    )
    parser.add_argument(
        "--clone-loops", type=int, default=80, help="Local clone benchmark loops"
    )
    parser.add_argument("--timeout", type=int, default=300, help="HTTP timeout seconds")
    parser.add_argument(
        "--chunk-size",
        type=int,
        default=256 * 1024,
        help="File stream chunk size bytes",
    )
    parser.add_argument(
        "--between-runs-ms",
        type=int,
        default=0,
        help="Sleep between successful runs (ms)",
    )
    parser.add_argument(
        "--max-rate-retries",
        type=int,
        default=6,
        help="Max retries on Telegram 429 rate limit",
    )
    parser.add_argument(
        "--retry-after-padding-ms",
        type=int,
        default=500,
        help="Extra wait added on top of retry_after (ms)",
    )
    parser.add_argument(
        "--new-conn-per-run",
        action="store_true",
        help="Disable keep-alive reuse (force new connection each run)",
    )
    parser.add_argument(
        "--delete-after-send",
        action="store_true",
        help="Delete sent message after each run to avoid chat spam",
    )
    parser.add_argument("--caption", default="race-bench", help="Caption text")
    parser.add_argument(
        "--json-output",
        type=Path,
        help="Optional JSON report output path",
    )
    parser.add_argument(
        "--markdown-output",
        type=Path,
        help="Optional markdown report output path",
    )
    parser.add_argument(
        "--print-json",
        action="store_true",
        help="Print the final JSON payload to stdout",
    )
    return parser.parse_args()


def make_connection(
    parsed: urllib.parse.ParseResult, timeout: int
) -> http.client.HTTPConnection:
    host = parsed.hostname
    if not host:
        raise ValueError("Invalid --api-base (missing hostname)")

    if parsed.scheme == "https":
        port = parsed.port or 443
        return http.client.HTTPSConnection(
            host, port, timeout=timeout, context=ssl.create_default_context()
        )
    if parsed.scheme == "http":
        port = parsed.port or 80
        return http.client.HTTPConnection(host, port, timeout=timeout)
    raise ValueError("--api-base must start with http:// or https://")


def api_path(parsed: urllib.parse.ParseResult, token: str, method: str) -> str:
    base_path = parsed.path.rstrip("/")
    if base_path:
        return f"{base_path}/bot{token}/{method}"
    return f"/bot{token}/{method}"


def build_multipart_parts(
    *,
    boundary: str,
    field_name: str,
    filename: str,
    content_type: str,
    chat_id: str,
    caption: str,
) -> tuple[bytes, bytes]:
    prefix = (
        f"--{boundary}\r\n"
        f'Content-Disposition: form-data; name="chat_id"\r\n\r\n'
        f"{chat_id}\r\n"
        f"--{boundary}\r\n"
        f'Content-Disposition: form-data; name="caption"\r\n\r\n'
        f"{caption}\r\n"
        f"--{boundary}\r\n"
        f'Content-Disposition: form-data; name="{field_name}"; filename="{filename}"\r\n'
        f"Content-Type: {content_type}\r\n\r\n"
    ).encode("utf-8")
    suffix = f"\r\n--{boundary}--\r\n".encode("utf-8")
    return prefix, suffix


def request_get_json(conn: http.client.HTTPConnection, path: str) -> dict:
    conn.request("GET", path)
    resp = conn.getresponse()
    data = resp.read()
    if resp.status != 200:
        raise RuntimeError(f"HTTP {resp.status}: {data[:300]!r}")
    return json.loads(data.decode("utf-8"))


def request_delete_message(
    conn: http.client.HTTPConnection,
    path: str,
    chat_id: str,
    message_id: int,
) -> None:
    payload = urllib.parse.urlencode(
        {
            "chat_id": chat_id,
            "message_id": str(message_id),
        }
    ).encode("utf-8")
    conn.request(
        "POST",
        path,
        body=payload,
        headers={
            "Content-Type": "application/x-www-form-urlencoded",
            "Content-Length": str(len(payload)),
            "Connection": "keep-alive",
        },
    )
    resp = conn.getresponse()
    _ = resp.read()


def upload_once(
    conn: http.client.HTTPConnection,
    *,
    path: str,
    prefix: bytes,
    suffix: bytes,
    boundary: str,
    file_path: Path | None,
    payload: bytes | bytearray | None,
    chunk_size: int,
) -> tuple[float, dict]:
    if (file_path is None) == (payload is None):
        raise ValueError("Exactly one of file_path or payload must be provided")

    data_len = (
        file_path.stat().st_size if file_path is not None else len(payload or b"")
    )
    total_len = len(prefix) + data_len + len(suffix)

    start = time.perf_counter()
    conn.putrequest("POST", path)
    conn.putheader("Content-Type", f"multipart/form-data; boundary={boundary}")
    conn.putheader("Content-Length", str(total_len))
    conn.putheader("Connection", "keep-alive")
    conn.endheaders()

    conn.send(prefix)

    if file_path is not None:
        with file_path.open("rb") as f:
            while True:
                chunk = f.read(chunk_size)
                if not chunk:
                    break
                conn.send(chunk)
    else:
        conn.send(payload or b"")

    conn.send(suffix)

    resp = conn.getresponse()
    body = resp.read()
    elapsed_ms = (time.perf_counter() - start) * 1000.0

    parsed: dict
    try:
        parsed = json.loads(body.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        parsed = {}

    if resp.status == 429:
        raise RateLimitError(
            retry_after_seconds=parse_retry_after_seconds(parsed), payload=parsed
        )

    if isinstance(parsed, dict) and not parsed.get("ok", False):
        description = str(parsed.get("description", "")).lower()
        if parsed.get("error_code") == 429 or "too many requests" in description:
            raise RateLimitError(
                retry_after_seconds=parse_retry_after_seconds(parsed), payload=parsed
            )

    if resp.status != 200:
        raise RuntimeError(f"HTTP {resp.status}: {body[:500]!r}")

    if not parsed:
        parsed = json.loads(body.decode("utf-8"))

    if not parsed.get("ok", False):
        raise RuntimeError(f"API not ok: {parsed}")
    return elapsed_ms, parsed


def clone_benchmark(data: bytes, loops: int) -> Stat:
    timings = []
    for _ in range(max(1, loops)):
        t0 = time.perf_counter()
        _copied = bytearray(data)
        t1 = time.perf_counter()
        timings.append((t1 - t0) * 1000.0)
    return summarize(timings)


def run_mode(
    *,
    mode: str,
    parsed_base: urllib.parse.ParseResult,
    token: str,
    chat_id: str,
    method: str,
    file_path: Path,
    file_bytes: bytes,
    runs: int,
    timeout: int,
    chunk_size: int,
    between_runs_ms: int,
    max_rate_retries: int,
    retry_after_padding_ms: int,
    new_conn_per_run: bool,
    delete_after_send: bool,
    caption: str,
) -> tuple[Stat, Stat | None]:
    field_name = "audio" if method == "sendAudio" else "document"
    content_type = mimetypes.guess_type(file_path.name)[0] or "application/octet-stream"
    endpoint = api_path(parsed_base, token, method)
    delete_endpoint = api_path(parsed_base, token, "deleteMessage")

    upload_times: list[float] = []
    clone_times: list[float] = []

    conn: http.client.HTTPConnection | None = None

    for i in range(1, runs + 1):
        if conn is None or new_conn_per_run:
            if conn is not None:
                conn.close()
            conn = make_connection(parsed_base, timeout)

        boundary = f"----bench-{uuid.uuid4().hex}"
        prefix, suffix = build_multipart_parts(
            boundary=boundary,
            field_name=field_name,
            filename=file_path.name,
            content_type=content_type,
            chat_id=chat_id,
            caption=f"{caption}-{mode}-run{i}",
        )

        payload: bytes | bytearray | None = None
        from_file: Path | None = None

        if mode == "file":
            from_file = file_path
        elif mode == "memory":
            payload = file_bytes
        elif mode == "memory-clone":
            t0 = time.perf_counter()
            payload = bytearray(file_bytes)
            t1 = time.perf_counter()
            clone_times.append((t1 - t0) * 1000.0)
        else:
            raise ValueError(f"Unknown mode: {mode}")

        retry_count = 0
        while True:
            try:
                elapsed_ms, response = upload_once(
                    conn,
                    path=endpoint,
                    prefix=prefix,
                    suffix=suffix,
                    boundary=boundary,
                    file_path=from_file,
                    payload=payload,
                    chunk_size=chunk_size,
                )
                break
            except RateLimitError as e:
                retry_count += 1
                if retry_count > max_rate_retries:
                    raise RuntimeError(
                        f"Exceeded max rate-limit retries on run {i}: {e}"
                    ) from e

                wait_sec = e.retry_after_seconds + (retry_after_padding_ms / 1000.0)
                print(
                    f"  rate-limited (run {i}, retry {retry_count}/{max_rate_retries}), "
                    f"sleeping {wait_sec:.2f}s"
                )

                if conn is not None:
                    conn.close()
                    conn = None

                time.sleep(wait_sec)
                conn = make_connection(parsed_base, timeout)

        upload_times.append(elapsed_ms)

        if delete_after_send:
            result = response.get("result") or {}
            message_id = result.get("message_id")
            if isinstance(message_id, int):
                request_delete_message(conn, delete_endpoint, chat_id, message_id)

        if between_runs_ms > 0 and i < runs:
            time.sleep(between_runs_ms / 1000.0)

    if conn is not None:
        conn.close()

    upload_stat = summarize(upload_times)
    clone_stat = summarize(clone_times) if clone_times else None
    return upload_stat, clone_stat


def main() -> int:
    args = parse_args()

    if not args.token:
        print("ERROR: --token is required (or set BOT_TOKEN)", file=sys.stderr)
        return 2
    if not args.chat_id:
        print("ERROR: --chat-id is required (or set CHAT_ID)", file=sys.stderr)
        return 2

    file_path = Path(args.file).expanduser().resolve()
    if not file_path.exists() or not file_path.is_file():
        print(f"ERROR: file not found: {file_path}", file=sys.stderr)
        return 2

    parsed_base = urllib.parse.urlparse(args.api_base)
    if parsed_base.scheme not in {"http", "https"}:
        print("ERROR: --api-base must start with http:// or https://", file=sys.stderr)
        return 2

    requested_modes = [m.strip() for m in args.modes.split(",") if m.strip()]
    allowed = {"file", "memory", "memory-clone"}
    for m in requested_modes:
        if m not in allowed:
            print(
                f"ERROR: invalid mode '{m}'. allowed: {sorted(allowed)}",
                file=sys.stderr,
            )
            return 2

    file_size = file_path.stat().st_size
    file_mb = file_size / (1024.0 * 1024.0)
    file_bytes = file_path.read_bytes()

    print("== Telegram Upload Bench ==")
    print(f"api_base       : {args.api_base}")
    print(f"method         : {args.method}")
    print(f"file           : {file_path}")
    print(f"size           : {file_mb:.2f} MB")
    print(f"runs/mode      : {args.runs}")
    print(f"modes          : {', '.join(requested_modes)}")
    print(f"reuse_conn     : {not args.new_conn_per_run}")
    print(f"between_runs   : {args.between_runs_ms} ms")
    print(f"rate_retries   : {args.max_rate_retries}")
    print(f"retry_padding  : {args.retry_after_padding_ms} ms")
    print(f"delete_after   : {args.delete_after_send}")
    print()

    clone_stat = clone_benchmark(file_bytes, args.clone_loops)
    print("[Local clone-only benchmark]")
    print_stat("clone", clone_stat)
    print()

    if args.warmup:
        print("[Warmup]")
        conn = make_connection(parsed_base, args.timeout)
        get_me_path = api_path(parsed_base, args.token, "getMe")
        warm = request_get_json(conn, get_me_path)
        conn.close()
        print(f"getMe ok       : {warm.get('ok', False)}")
        print()

    mode_results: dict[str, tuple[Stat, Stat | None]] = {}
    mode_payloads: dict[str, dict[str, Any]] = {}

    for mode in requested_modes:
        print(f"[Mode: {mode}]")
        upload_stat, clone_in_mode = run_mode(
            mode=mode,
            parsed_base=parsed_base,
            token=args.token,
            chat_id=args.chat_id,
            method=args.method,
            file_path=file_path,
            file_bytes=file_bytes,
            runs=args.runs,
            timeout=args.timeout,
            chunk_size=args.chunk_size,
            between_runs_ms=max(0, args.between_runs_ms),
            max_rate_retries=max(0, args.max_rate_retries),
            retry_after_padding_ms=max(0, args.retry_after_padding_ms),
            new_conn_per_run=args.new_conn_per_run,
            delete_after_send=args.delete_after_send,
            caption=args.caption,
        )
        mode_results[mode] = (upload_stat, clone_in_mode)

        print_stat("upload", upload_stat)
        mbps_avg = file_mb / (upload_stat.avg / 1000.0) if upload_stat.avg > 0 else 0.0
        print(f"{'throughput':>14}: avg={mbps_avg:.2f} MB/s")
        mode_entry: dict[str, Any] = {
            "upload": stat_to_payload(upload_stat),
            "throughput_mb_s_avg": mbps_avg,
        }
        if clone_in_mode is not None:
            print_stat("clone", clone_in_mode)
            ratio = (
                (clone_in_mode.avg / upload_stat.avg * 100.0)
                if upload_stat.avg > 0
                else 0.0
            )
            print(f"{'clone/share':>14}: avg={ratio:.3f}% of upload time")
            mode_entry["clone"] = stat_to_payload(clone_in_mode)
            mode_entry["clone_share_percent_avg"] = ratio
        mode_payloads[mode] = mode_entry
        print()

    comparisons: dict[str, float] = {}
    if "file" in mode_results and "memory-clone" in mode_results:
        file_avg = mode_results["file"][0].avg
        memc_avg = mode_results["memory-clone"][0].avg
        delta = memc_avg - file_avg
        print("[Comparison]")
        print(f"memory-clone - file (avg): {delta:+.2f} ms")
        comparisons["memory_clone_vs_file_avg_ms_delta"] = delta
        if file_avg > 0:
            relative = delta / file_avg * 100.0
            print(f"relative delta           : {relative:+.2f}%")
            comparisons["memory_clone_vs_file_relative_percent"] = relative

    payload: dict[str, Any] = {
        "meta": {
            "api_base": args.api_base,
            "method": args.method,
            "file": str(file_path),
            "file_size_mb": file_mb,
            "runs_per_mode": args.runs,
            "modes": requested_modes,
            "reuse_connection": not args.new_conn_per_run,
            "between_runs_ms": max(0, args.between_runs_ms),
            "max_rate_retries": max(0, args.max_rate_retries),
            "retry_after_padding_ms": max(0, args.retry_after_padding_ms),
            "delete_after_send": bool(args.delete_after_send),
        },
        "local_clone": stat_to_payload(clone_stat),
        "modes": mode_payloads,
        "comparisons": comparisons,
    }

    if args.json_output:
        args.json_output.parent.mkdir(parents=True, exist_ok=True)
        args.json_output.write_text(json.dumps(payload, indent=2), encoding="utf-8")
        print(f"\nSaved JSON report: {args.json_output}")

    if args.markdown_output:
        args.markdown_output.parent.mkdir(parents=True, exist_ok=True)
        markdown = render_markdown_report(payload)
        args.markdown_output.write_text(markdown, encoding="utf-8")
        print(f"Saved markdown report: {args.markdown_output}")

    if args.print_json:
        print(json.dumps(payload, indent=2))

    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except KeyboardInterrupt:
        print("\nInterrupted.")
        raise SystemExit(130)

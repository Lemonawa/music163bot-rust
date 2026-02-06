#!/usr/bin/env python3
"""Synthetic performance comparison for v1.1.16 changes.

This script benchmarks three areas with local, reproducible workloads:
1) /status query path (old 3-query strategy vs optimized single-query strategy)
2) First-download latency model (old double-cover fetch vs optimized single-cover fetch)
3) Peak memory model (old buffers vs optimized buffers)

It outputs machine-readable JSON and a markdown report.
"""

from __future__ import annotations

import argparse
import gc
import json
import os
import sqlite3
import tempfile
import threading
import time
import tracemalloc
from dataclasses import asdict, dataclass
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from statistics import mean
from typing import Callable
from urllib.request import urlopen


@dataclass
class Stats:
    first_ms: float
    mean_ms: float
    p95_ms: float


@dataclass
class StatusBench:
    before: Stats
    after: Stats
    speedup_x: float


@dataclass
class DownloadBench:
    before: Stats
    after: Stats
    speedup_x: float


@dataclass
class MemoryBench:
    before_peak_mb: float
    after_peak_mb: float
    reduction_percent: float


@dataclass
class Report:
    status: StatusBench
    first_download: DownloadBench
    peak_memory: MemoryBench


class SilentLatencyHandler(SimpleHTTPRequestHandler):
    def __init__(self, *args, directory: str, latency_sec: float, **kwargs):
        self._latency_sec = latency_sec
        super().__init__(*args, directory=directory, **kwargs)

    def do_GET(self):
        time.sleep(self._latency_sec)
        super().do_GET()

    def log_message(self, format: str, *args):
        _ = (format, args)


def percentile(values: list[float], ratio: float) -> float:
    if not values:
        return 0.0
    sorted_values = sorted(values)
    idx = int((len(sorted_values) - 1) * ratio)
    return sorted_values[idx]


def to_stats(samples_ms: list[float]) -> Stats:
    return Stats(
        first_ms=samples_ms[0],
        mean_ms=mean(samples_ms),
        p95_ms=percentile(samples_ms, 0.95),
    )


def time_loop(rounds: int, fn: Callable[[], None]) -> list[float]:
    samples: list[float] = []
    for _ in range(rounds):
        start = time.perf_counter()
        fn()
        elapsed_ms = (time.perf_counter() - start) * 1000.0
        samples.append(elapsed_ms)
    return samples


def bench_status(rows: int, rounds: int, roundtrip_overhead_us: int) -> StatusBench:
    conn = sqlite3.connect(":memory:")
    cur = conn.cursor()
    cur.execute(
        """
        CREATE TABLE song_infos (
            music_id INTEGER PRIMARY KEY,
            from_user_id INTEGER NOT NULL,
            from_chat_id INTEGER NOT NULL
        )
        """
    )
    data = [(idx + 1, (idx % 500) + 1, (idx % 80) + 1) for idx in range(rows)]
    cur.executemany(
        "INSERT INTO song_infos (music_id, from_user_id, from_chat_id) VALUES (?, ?, ?)",
        data,
    )
    cur.execute("CREATE INDEX idx_song_infos_from_user_id ON song_infos(from_user_id)")
    cur.execute("CREATE INDEX idx_song_infos_from_chat_id ON song_infos(from_chat_id)")
    conn.commit()

    target_user = 42
    target_chat = 9
    overhead_sec = max(roundtrip_overhead_us, 0) / 1_000_000.0

    def old_status():
        time.sleep(overhead_sec)
        cur.execute("SELECT COUNT(*) FROM song_infos")
        cur.fetchone()
        time.sleep(overhead_sec)
        cur.execute(
            "SELECT COUNT(*) FROM song_infos WHERE from_user_id = ?", (target_user,)
        )
        cur.fetchone()
        time.sleep(overhead_sec)
        cur.execute(
            "SELECT COUNT(*) FROM song_infos WHERE from_chat_id = ?", (target_chat,)
        )
        cur.fetchone()

    def new_status():
        time.sleep(overhead_sec)
        cur.execute(
            """
            SELECT
                (SELECT COUNT(*) FROM song_infos) AS total_count,
                (SELECT COUNT(*) FROM song_infos WHERE from_user_id = ?) AS user_count,
                (SELECT COUNT(*) FROM song_infos WHERE from_chat_id = ?) AS chat_count
            """,
            (target_user, target_chat),
        )
        cur.fetchone()

    # Warm-up
    old_status()
    new_status()

    old_samples = time_loop(rounds, old_status)
    new_samples = time_loop(rounds, new_status)

    before = to_stats(old_samples)
    after = to_stats(new_samples)
    speedup = before.mean_ms / after.mean_ms if after.mean_ms else 0.0
    return StatusBench(before=before, after=after, speedup_x=speedup)


def fetch_bytes(url: str) -> bytes:
    with urlopen(url, timeout=30) as resp:
        return resp.read()


def old_download_flow(base_url: str) -> None:
    audio = fetch_bytes(f"{base_url}/audio.bin")
    cover_original = fetch_bytes(f"{base_url}/cover.bin")
    cover_thumbnail = fetch_bytes(f"{base_url}/cover.bin")
    _ = len(audio) + len(cover_original) + len(cover_thumbnail)


def new_download_flow(base_url: str) -> None:
    audio = fetch_bytes(f"{base_url}/audio.bin")
    cover_original = fetch_bytes(f"{base_url}/cover.bin")
    _ = len(audio) + len(cover_original)


def bench_first_download(base_url: str, rounds: int) -> DownloadBench:
    # Warm-up one request for server side
    fetch_bytes(f"{base_url}/cover.bin")

    old_samples = time_loop(rounds, lambda: old_download_flow(base_url))
    new_samples = time_loop(rounds, lambda: new_download_flow(base_url))

    before = to_stats(old_samples)
    after = to_stats(new_samples)
    speedup = before.mean_ms / after.mean_ms if after.mean_ms else 0.0
    return DownloadBench(before=before, after=after, speedup_x=speedup)


def bench_peak_memory(base_url: str, rounds: int) -> MemoryBench:
    def measure_peak(flow: Callable[[str], None]) -> float:
        tracemalloc.start()
        for _ in range(rounds):
            flow(base_url)
            gc.collect()
        _, peak = tracemalloc.get_traced_memory()
        tracemalloc.stop()
        return peak / (1024.0 * 1024.0)

    before_peak = measure_peak(old_download_flow)
    after_peak = measure_peak(new_download_flow)
    reduction = (
        ((before_peak - after_peak) / before_peak * 100.0) if before_peak else 0.0
    )
    return MemoryBench(
        before_peak_mb=before_peak,
        after_peak_mb=after_peak,
        reduction_percent=reduction,
    )


def create_fixtures(root: Path, cover_mb: int, audio_mb: int) -> None:
    (root / "cover.bin").write_bytes(os.urandom(max(1, cover_mb) * 1024 * 1024))
    (root / "audio.bin").write_bytes(os.urandom(max(1, audio_mb) * 1024 * 1024))


def start_server(root: Path, latency_sec: float) -> tuple[ThreadingHTTPServer, str]:
    handler = partial(
        SilentLatencyHandler, directory=str(root), latency_sec=latency_sec
    )
    server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    base_url = f"http://127.0.0.1:{server.server_port}"
    return server, base_url


def to_markdown(report: Report) -> str:
    return (
        "# v1.1.16 Performance Comparison\n\n"
        "Synthetic local benchmark comparing before/after strategies.\n\n"
        "## /status Query Path\n\n"
        "| Metric | Before (3 queries) | After (1 query) |\n"
        "|---|---:|---:|\n"
        f"| First latency (ms) | {report.status.before.first_ms:.2f} | {report.status.after.first_ms:.2f} |\n"
        f"| Mean latency (ms) | {report.status.before.mean_ms:.2f} | {report.status.after.mean_ms:.2f} |\n"
        f"| P95 latency (ms) | {report.status.before.p95_ms:.2f} | {report.status.after.p95_ms:.2f} |\n"
        f"| Speedup | - | {report.status.speedup_x:.2f}x |\n\n"
        "## First Download Latency Model\n\n"
        "| Metric | Before (audio + cover + cover) | After (audio + cover) |\n"
        "|---|---:|---:|\n"
        f"| First latency (ms) | {report.first_download.before.first_ms:.2f} | {report.first_download.after.first_ms:.2f} |\n"
        f"| Mean latency (ms) | {report.first_download.before.mean_ms:.2f} | {report.first_download.after.mean_ms:.2f} |\n"
        f"| P95 latency (ms) | {report.first_download.before.p95_ms:.2f} | {report.first_download.after.p95_ms:.2f} |\n"
        f"| Speedup | - | {report.first_download.speedup_x:.2f}x |\n\n"
        "## Peak Memory Model\n\n"
        "| Metric | Before | After |\n"
        "|---|---:|---:|\n"
        f"| Peak allocated memory (MB) | {report.peak_memory.before_peak_mb:.2f} | {report.peak_memory.after_peak_mb:.2f} |\n"
        f"| Reduction (%) | - | {report.peak_memory.reduction_percent:.2f}% |\n"
    )


def main() -> int:
    parser = argparse.ArgumentParser(description="Run synthetic performance comparison")
    parser.add_argument("--status-rows", type=int, default=50_000)
    parser.add_argument("--status-rounds", type=int, default=300)
    parser.add_argument("--download-rounds", type=int, default=30)
    parser.add_argument("--memory-rounds", type=int, default=12)
    parser.add_argument("--latency-ms", type=float, default=18.0)
    parser.add_argument("--query-roundtrip-us", type=int, default=150)
    parser.add_argument("--cover-mb", type=int, default=4)
    parser.add_argument("--audio-mb", type=int, default=6)
    parser.add_argument("--json-output", type=Path, required=False)
    parser.add_argument("--markdown-output", type=Path, required=False)
    args = parser.parse_args()

    with tempfile.TemporaryDirectory(prefix="music163bot_perf_") as temp_dir:
        root = Path(temp_dir)
        create_fixtures(root, cover_mb=args.cover_mb, audio_mb=args.audio_mb)
        server, base_url = start_server(root, args.latency_ms / 1000.0)
        try:
            status = bench_status(
                rows=args.status_rows,
                rounds=args.status_rounds,
                roundtrip_overhead_us=args.query_roundtrip_us,
            )
            first_download = bench_first_download(base_url, rounds=args.download_rounds)
            peak_memory = bench_peak_memory(base_url, rounds=args.memory_rounds)
        finally:
            server.shutdown()
            server.server_close()

    report = Report(
        status=status, first_download=first_download, peak_memory=peak_memory
    )
    payload = asdict(report)

    if args.json_output:
        args.json_output.parent.mkdir(parents=True, exist_ok=True)
        args.json_output.write_text(json.dumps(payload, indent=2), encoding="utf-8")

    markdown = to_markdown(report)
    if args.markdown_output:
        args.markdown_output.parent.mkdir(parents=True, exist_ok=True)
        args.markdown_output.write_text(markdown, encoding="utf-8")

    print(markdown)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Synthetic performance comparison for v1.1.17 optimizations.

This script benchmarks reproducible local workload models for:
1) /status SQL path (3 queries vs 1 subquery statement)
2) /status render path (always-refresh resource sampling vs cached sampling + concise text)
3) first-download latency (audio + cover + cover vs audio + cover)
4) lyric upload path (temp-file roundtrip vs in-memory bytes upload)
5) peak memory model for download flow
6) singleflight fanout collapse
7) API cache hit model
8) shared API cache object model (clone-heavy cache values vs shared objects)
"""

from __future__ import annotations

import argparse
import copy
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
class StatusRenderBench:
    rounds: int
    sample_cost_ms: float
    refresh_interval_ms: int
    before: Stats
    after: Stats
    speedup_x: float


@dataclass
class DownloadBench:
    before: Stats
    after: Stats
    speedup_x: float


@dataclass
class LyricUploadBench:
    rounds: int
    lyric_size_kb: int
    before: Stats
    after: Stats
    speedup_x: float


@dataclass
class MemoryBench:
    before_peak_mb: float
    after_peak_mb: float
    reduction_percent: float


@dataclass
class SingleflightBench:
    requests: int
    rounds: int
    before: Stats
    after: Stats
    before_upstream_calls_per_round: float
    after_upstream_calls_per_round: float
    call_reduction_percent: float


@dataclass
class ApiCacheBench:
    rounds: int
    before: Stats
    after: Stats
    speedup_x: float


@dataclass
class SharedCacheBench:
    rounds: int
    payload_kb: int
    before: Stats
    after: Stats
    speedup_x: float


@dataclass
class Report:
    status: StatusBench
    status_render: StatusRenderBench
    first_download: DownloadBench
    lyric_upload: LyricUploadBench
    peak_memory: MemoryBench
    singleflight: SingleflightBench
    api_cache: ApiCacheBench
    shared_api_cache: SharedCacheBench


@dataclass
class ResourceSnapshot:
    cpu_percent: float
    used_memory_mb: int
    total_memory_mb: int
    available_memory_mb: int


class SyntheticResourceSampler:
    def __init__(self, refresh_interval_ms: int, sample_cost_ms: float):
        self._refresh_interval_sec = max(refresh_interval_ms, 0) / 1000.0
        self._sample_cost_sec = max(sample_cost_ms, 0.0) / 1000.0
        self._last_refresh = -1e9
        self._snapshot = ResourceSnapshot(
            cpu_percent=0.0,
            used_memory_mb=0,
            total_memory_mb=0,
            available_memory_mb=0,
        )

    def snapshot(self, now: float, *, force_refresh: bool) -> ResourceSnapshot:
        should_refresh = force_refresh or (
            now - self._last_refresh >= self._refresh_interval_sec
        )
        if should_refresh:
            if self._sample_cost_sec > 0:
                time.sleep(self._sample_cost_sec)
            self._snapshot = ResourceSnapshot(
                cpu_percent=31.2,
                used_memory_mb=1220,
                total_memory_mb=8192,
                available_memory_mb=6740,
            )
            self._last_refresh = now
        return self._snapshot


class SilentLatencyHandler(SimpleHTTPRequestHandler):
    def __init__(self, *args, directory: str, latency_sec: float, **kwargs):
        self._latency_sec = latency_sec
        super().__init__(*args, directory=directory, **kwargs)

    def do_GET(self):
        time.sleep(self._latency_sec)
        super().do_GET()

    def log_message(self, format: str, *args):
        _ = (format, args)


def nonzero_rounds(rounds: int) -> int:
    return max(rounds, 1)


def percentile(values: list[float], ratio: float) -> float:
    if not values:
        return 0.0
    sorted_values = sorted(values)
    idx = int((len(sorted_values) - 1) * ratio)
    return sorted_values[idx]


def to_stats(samples_ms: list[float]) -> Stats:
    if not samples_ms:
        return Stats(first_ms=0.0, mean_ms=0.0, p95_ms=0.0)
    return Stats(
        first_ms=samples_ms[0],
        mean_ms=mean(samples_ms),
        p95_ms=percentile(samples_ms, 0.95),
    )


def time_loop(rounds: int, fn: Callable[[], None]) -> list[float]:
    samples: list[float] = []
    for _ in range(nonzero_rounds(rounds)):
        start = time.perf_counter()
        fn()
        elapsed_ms = (time.perf_counter() - start) * 1000.0
        samples.append(elapsed_ms)
    return samples


def run_thread_round(fanout: int, worker: Callable[[], None]) -> float:
    threads = [threading.Thread(target=worker) for _ in range(fanout)]
    start = time.perf_counter()
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()
    return (time.perf_counter() - start) * 1000.0


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
    data = [(idx + 1, (idx % 500) + 1, (idx % 80) + 1) for idx in range(max(rows, 1))]
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

    old_status()
    new_status()

    old_samples = time_loop(rounds, old_status)
    new_samples = time_loop(rounds, new_status)
    conn.close()

    before = to_stats(old_samples)
    after = to_stats(new_samples)
    speedup = before.mean_ms / after.mean_ms if after.mean_ms else 0.0
    return StatusBench(before=before, after=after, speedup_x=speedup)


def build_old_status_text(snapshot: ResourceSnapshot) -> str:
    return (
        "Status\n\n"
        "Cache total: 24120\n"
        "Cache user: 62\n"
        "Cache chat: 18\n\n"
        "CPU usage: {:.1f}%\n"
        "Memory: {} / {} MB (available {} MB)\n"
        "Uptime: 03:42:15\n\n"
        "Download speed: n/a\n"
        "Upload speed: n/a\n\n"
        "Stack: Rust + Tokio + Teloxide + SQLx + Reqwest + SQLite\n"
    ).format(
        snapshot.cpu_percent,
        snapshot.used_memory_mb,
        snapshot.total_memory_mb,
        snapshot.available_memory_mb,
    )


def build_new_status_text(snapshot: ResourceSnapshot) -> str:
    return (
        "Status\n\n"
        "Cache total: 24120\n"
        "Cache user: 62\n"
        "Cache chat: 18\n\n"
        "Runtime cache hits: 232\n"
        "Runtime cache misses: 17\n"
        "Runtime hit rate: 93.17%\n\n"
        "CPU usage: {:.1f}%\n"
        "Memory: {} / {} MB (available {} MB)\n"
        "Uptime: 03:42:15\n\n"
        "Download speed: no non-cache samples yet\n"
        "Upload speed: no non-cache samples yet"
    ).format(
        snapshot.cpu_percent,
        snapshot.used_memory_mb,
        snapshot.total_memory_mb,
        snapshot.available_memory_mb,
    )


def bench_status_render(
    rounds: int,
    sample_cost_ms: float,
    refresh_interval_ms: int,
) -> StatusRenderBench:
    old_sampler = SyntheticResourceSampler(
        refresh_interval_ms=0,
        sample_cost_ms=sample_cost_ms,
    )
    new_sampler = SyntheticResourceSampler(
        refresh_interval_ms=refresh_interval_ms,
        sample_cost_ms=sample_cost_ms,
    )

    def old_status():
        snapshot = old_sampler.snapshot(time.perf_counter(), force_refresh=True)
        text = build_old_status_text(snapshot)
        _ = len(text)

    def new_status():
        snapshot = new_sampler.snapshot(time.perf_counter(), force_refresh=False)
        text = build_new_status_text(snapshot)
        _ = len(text)

    old_status()
    new_status()

    old_samples = time_loop(rounds, old_status)
    new_samples = time_loop(rounds, new_status)

    before = to_stats(old_samples)
    after = to_stats(new_samples)
    speedup = before.mean_ms / after.mean_ms if after.mean_ms else 0.0

    return StatusRenderBench(
        rounds=nonzero_rounds(rounds),
        sample_cost_ms=max(sample_cost_ms, 0.0),
        refresh_interval_ms=max(refresh_interval_ms, 0),
        before=before,
        after=after,
        speedup_x=speedup,
    )


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
    fetch_bytes(f"{base_url}/cover.bin")

    old_samples = time_loop(rounds, lambda: old_download_flow(base_url))
    new_samples = time_loop(rounds, lambda: new_download_flow(base_url))

    before = to_stats(old_samples)
    after = to_stats(new_samples)
    speedup = before.mean_ms / after.mean_ms if after.mean_ms else 0.0
    return DownloadBench(before=before, after=after, speedup_x=speedup)


def make_lyric_payload(size_kb: int) -> bytes:
    size_bytes = max(size_kb, 1) * 1024
    line = "[00:01.00] synthetic lyric benchmark line\n".encode("utf-8")
    chunks = (size_bytes // len(line)) + 2
    payload = line * chunks
    return payload[:size_bytes]


def bench_lyric_upload(rounds: int, lyric_size_kb: int) -> LyricUploadBench:
    lyric_bytes = make_lyric_payload(lyric_size_kb)

    with tempfile.TemporaryDirectory(prefix="music163bot_lyric_") as temp_dir:
        root = Path(temp_dir)

        def old_flow() -> None:
            file_path = root / f"lyric_{time.perf_counter_ns()}.lrc"
            file_path.write_bytes(lyric_bytes)
            upload_payload = file_path.read_bytes()
            file_path.unlink()
            _ = len(upload_payload)

        def new_flow() -> None:
            upload_payload = lyric_bytes
            _ = len(upload_payload)

        old_flow()
        new_flow()

        old_samples = time_loop(rounds, old_flow)
        new_samples = time_loop(rounds, new_flow)

    before = to_stats(old_samples)
    after = to_stats(new_samples)
    speedup = before.mean_ms / after.mean_ms if after.mean_ms else 0.0

    return LyricUploadBench(
        rounds=nonzero_rounds(rounds),
        lyric_size_kb=max(lyric_size_kb, 1),
        before=before,
        after=after,
        speedup_x=speedup,
    )


def bench_peak_memory(base_url: str, rounds: int) -> MemoryBench:
    loop_rounds = nonzero_rounds(rounds)

    def measure_peak(flow: Callable[[str], None]) -> float:
        tracemalloc.start()
        for _ in range(loop_rounds):
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


def bench_singleflight(
    requests: int, rounds: int, upstream_latency_ms: float
) -> SingleflightBench:
    fanout = max(requests, 1)
    loop_rounds = nonzero_rounds(rounds)
    upstream_sec = max(upstream_latency_ms, 0.0) / 1000.0

    def before_round() -> tuple[float, int]:
        call_count = 0
        counter_lock = threading.Lock()

        def worker() -> None:
            nonlocal call_count
            for _ in range(2):
                time.sleep(upstream_sec)
                with counter_lock:
                    call_count += 1

        elapsed_ms = run_thread_round(fanout, worker)
        return elapsed_ms, call_count

    def after_round() -> tuple[float, int]:
        call_count = 0
        state_lock = threading.Lock()
        leader_ready = threading.Event()
        leader_claimed = False

        def worker() -> None:
            nonlocal call_count, leader_claimed
            with state_lock:
                is_leader = not leader_claimed
                if is_leader:
                    leader_claimed = True

            if is_leader:
                for _ in range(2):
                    time.sleep(upstream_sec)
                    with state_lock:
                        call_count += 1
                leader_ready.set()
            else:
                leader_ready.wait()

        elapsed_ms = run_thread_round(fanout, worker)
        return elapsed_ms, call_count

    before_samples: list[float] = []
    after_samples: list[float] = []
    before_calls_total = 0
    after_calls_total = 0

    for _ in range(loop_rounds):
        elapsed_before, calls_before = before_round()
        elapsed_after, calls_after = after_round()
        before_samples.append(elapsed_before)
        after_samples.append(elapsed_after)
        before_calls_total += calls_before
        after_calls_total += calls_after

    before_calls_per_round = before_calls_total / loop_rounds
    after_calls_per_round = after_calls_total / loop_rounds
    reduction = (
        (
            (before_calls_per_round - after_calls_per_round)
            / before_calls_per_round
            * 100.0
        )
        if before_calls_per_round
        else 0.0
    )

    return SingleflightBench(
        requests=fanout,
        rounds=loop_rounds,
        before=to_stats(before_samples),
        after=to_stats(after_samples),
        before_upstream_calls_per_round=before_calls_per_round,
        after_upstream_calls_per_round=after_calls_per_round,
        call_reduction_percent=reduction,
    )


def bench_api_cache(rounds: int, upstream_latency_ms: float) -> ApiCacheBench:
    loop_rounds = nonzero_rounds(rounds)
    upstream_sec = max(upstream_latency_ms, 0.0) / 1000.0
    before_samples: list[float] = []
    after_samples: list[float] = []

    for _ in range(loop_rounds):
        start = time.perf_counter()
        time.sleep(upstream_sec)
        before_samples.append((time.perf_counter() - start) * 1000.0)

    cache_ready = False
    for _ in range(loop_rounds):
        start = time.perf_counter()
        if cache_ready:
            pass
        else:
            time.sleep(upstream_sec)
            cache_ready = True
        after_samples.append((time.perf_counter() - start) * 1000.0)

    before = to_stats(before_samples)
    after = to_stats(after_samples)
    speedup = before.mean_ms / after.mean_ms if after.mean_ms else 0.0

    return ApiCacheBench(
        rounds=loop_rounds, before=before, after=after, speedup_x=speedup
    )


def make_cache_payload(payload_kb: int) -> tuple[dict, dict]:
    size_bytes = max(payload_kb, 1) * 1024
    cover_blob = bytearray(os.urandom(size_bytes))
    signature_blob = bytearray(os.urandom(size_bytes))
    detail = {
        "id": 284031,
        "name": "Synthetic Song",
        "artists": [{"name": "A"}, {"name": "B"}],
        "duration_ms": 289_000,
        "cover_blob": cover_blob,
    }
    url = {
        "id": 284031,
        "br": 320_000,
        "format": "flac",
        "url": "https://example.com/song.flac",
        "signature_blob": signature_blob,
    }
    return detail, url


def bench_shared_api_cache(rounds: int, payload_kb: int) -> SharedCacheBench:
    loop_rounds = nonzero_rounds(rounds)
    cached_detail, cached_url = make_cache_payload(payload_kb)

    def old_flow() -> None:
        detail = copy.deepcopy(cached_detail)
        song_url = copy.deepcopy(cached_url)
        _ = detail["id"] + song_url["br"]

    def new_flow() -> None:
        detail = cached_detail
        song_url = cached_url
        _ = detail["id"] + song_url["br"]

    old_flow()
    new_flow()
    old_samples = time_loop(loop_rounds, old_flow)
    new_samples = time_loop(loop_rounds, new_flow)

    before = to_stats(old_samples)
    after = to_stats(new_samples)
    speedup = before.mean_ms / after.mean_ms if after.mean_ms else 0.0
    return SharedCacheBench(
        rounds=loop_rounds,
        payload_kb=max(payload_kb, 1),
        before=before,
        after=after,
        speedup_x=speedup,
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
        "# v1.1.17 Performance Comparison\n\n"
        "Synthetic local benchmark comparing before/after strategies.\n\n"
        "## /status SQL Query Path\n\n"
        "| Metric | Before (3 queries) | After (1 subquery statement) |\n"
        "|---|---:|---:|\n"
        f"| First latency (ms) | {report.status.before.first_ms:.2f} | {report.status.after.first_ms:.2f} |\n"
        f"| Mean latency (ms) | {report.status.before.mean_ms:.2f} | {report.status.after.mean_ms:.2f} |\n"
        f"| P95 latency (ms) | {report.status.before.p95_ms:.2f} | {report.status.after.p95_ms:.2f} |\n"
        f"| Speedup | - | {report.status.speedup_x:.2f}x |\n\n"
        "## /status Render + Resource Sampling Model\n\n"
        f"Rounds: {report.status_render.rounds}, sample cost: {report.status_render.sample_cost_ms:.2f} ms, refresh interval: {report.status_render.refresh_interval_ms} ms\n\n"
        "| Metric | Before (refresh every request) | After (cached sample + concise text) |\n"
        "|---|---:|---:|\n"
        f"| First latency (ms) | {report.status_render.before.first_ms:.2f} | {report.status_render.after.first_ms:.2f} |\n"
        f"| Mean latency (ms) | {report.status_render.before.mean_ms:.2f} | {report.status_render.after.mean_ms:.2f} |\n"
        f"| P95 latency (ms) | {report.status_render.before.p95_ms:.2f} | {report.status_render.after.p95_ms:.2f} |\n"
        f"| Speedup | - | {report.status_render.speedup_x:.2f}x |\n\n"
        "## First Download Latency Model\n\n"
        "| Metric | Before (audio + cover + cover) | After (audio + cover) |\n"
        "|---|---:|---:|\n"
        f"| First latency (ms) | {report.first_download.before.first_ms:.2f} | {report.first_download.after.first_ms:.2f} |\n"
        f"| Mean latency (ms) | {report.first_download.before.mean_ms:.2f} | {report.first_download.after.mean_ms:.2f} |\n"
        f"| P95 latency (ms) | {report.first_download.before.p95_ms:.2f} | {report.first_download.after.p95_ms:.2f} |\n"
        f"| Speedup | - | {report.first_download.speedup_x:.2f}x |\n\n"
        "## Lyric Upload Path Model\n\n"
        f"Rounds: {report.lyric_upload.rounds}, lyric payload: {report.lyric_upload.lyric_size_kb} KB\n\n"
        "| Metric | Before (temp file write+read) | After (in-memory bytes) |\n"
        "|---|---:|---:|\n"
        f"| First latency (ms) | {report.lyric_upload.before.first_ms:.3f} | {report.lyric_upload.after.first_ms:.3f} |\n"
        f"| Mean latency (ms) | {report.lyric_upload.before.mean_ms:.3f} | {report.lyric_upload.after.mean_ms:.3f} |\n"
        f"| P95 latency (ms) | {report.lyric_upload.before.p95_ms:.3f} | {report.lyric_upload.after.p95_ms:.3f} |\n"
        f"| Speedup | - | {report.lyric_upload.speedup_x:.2f}x |\n\n"
        "## Peak Memory Model\n\n"
        "| Metric | Before | After |\n"
        "|---|---:|---:|\n"
        f"| Peak allocated memory (MB) | {report.peak_memory.before_peak_mb:.2f} | {report.peak_memory.after_peak_mb:.2f} |\n"
        f"| Reduction (%) | - | {report.peak_memory.reduction_percent:.2f}% |\n\n"
        "## Singleflight Fanout Model\n\n"
        f"Requests per round: {report.singleflight.requests}, rounds: {report.singleflight.rounds}\n\n"
        "| Metric | Before | After |\n"
        "|---|---:|---:|\n"
        f"| Mean latency (ms) | {report.singleflight.before.mean_ms:.2f} | {report.singleflight.after.mean_ms:.2f} |\n"
        f"| P95 latency (ms) | {report.singleflight.before.p95_ms:.2f} | {report.singleflight.after.p95_ms:.2f} |\n"
        f"| Upstream calls / round | {report.singleflight.before_upstream_calls_per_round:.2f} | {report.singleflight.after_upstream_calls_per_round:.2f} |\n"
        f"| Call reduction (%) | - | {report.singleflight.call_reduction_percent:.2f}% |\n\n"
        "## API Cache Hit Model\n\n"
        f"Rounds: {report.api_cache.rounds}\n\n"
        "| Metric | Before (always miss) | After (warm cache) |\n"
        "|---|---:|---:|\n"
        f"| First latency (ms) | {report.api_cache.before.first_ms:.2f} | {report.api_cache.after.first_ms:.2f} |\n"
        f"| Mean latency (ms) | {report.api_cache.before.mean_ms:.2f} | {report.api_cache.after.mean_ms:.2f} |\n"
        f"| P95 latency (ms) | {report.api_cache.before.p95_ms:.2f} | {report.api_cache.after.p95_ms:.2f} |\n"
        f"| Speedup | - | {report.api_cache.speedup_x:.2f}x |\n\n"
        "## Shared API Cache Object Model\n\n"
        f"Rounds: {report.shared_api_cache.rounds}, payload per object: {report.shared_api_cache.payload_kb} KB\n\n"
        "| Metric | Before (clone-heavy cached values) | After (shared cached objects) |\n"
        "|---|---:|---:|\n"
        f"| First latency (ms) | {report.shared_api_cache.before.first_ms:.3f} | {report.shared_api_cache.after.first_ms:.3f} |\n"
        f"| Mean latency (ms) | {report.shared_api_cache.before.mean_ms:.3f} | {report.shared_api_cache.after.mean_ms:.3f} |\n"
        f"| P95 latency (ms) | {report.shared_api_cache.before.p95_ms:.3f} | {report.shared_api_cache.after.p95_ms:.3f} |\n"
        f"| Speedup | - | {report.shared_api_cache.speedup_x:.2f}x |\n"
    )


def main() -> int:
    parser = argparse.ArgumentParser(description="Run synthetic performance comparison")
    parser.add_argument("--status-rows", type=int, default=50_000)
    parser.add_argument("--status-rounds", type=int, default=300)
    parser.add_argument("--status-render-rounds", type=int, default=450)
    parser.add_argument("--resource-sample-ms", type=float, default=2.5)
    parser.add_argument("--resource-refresh-ms", type=int, default=2_000)
    parser.add_argument("--download-rounds", type=int, default=30)
    parser.add_argument("--lyric-rounds", type=int, default=500)
    parser.add_argument("--lyric-size-kb", type=int, default=32)
    parser.add_argument("--memory-rounds", type=int, default=12)
    parser.add_argument("--latency-ms", type=float, default=18.0)
    parser.add_argument("--query-roundtrip-us", type=int, default=150)
    parser.add_argument("--cover-mb", type=int, default=4)
    parser.add_argument("--audio-mb", type=int, default=6)
    parser.add_argument("--singleflight-rounds", type=int, default=40)
    parser.add_argument("--singleflight-fanout", type=int, default=20)
    parser.add_argument("--api-cache-rounds", type=int, default=200)
    parser.add_argument("--shared-cache-rounds", type=int, default=800)
    parser.add_argument("--shared-cache-payload-kb", type=int, default=256)
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
            status_render = bench_status_render(
                rounds=args.status_render_rounds,
                sample_cost_ms=args.resource_sample_ms,
                refresh_interval_ms=args.resource_refresh_ms,
            )
            first_download = bench_first_download(base_url, rounds=args.download_rounds)
            lyric_upload = bench_lyric_upload(
                rounds=args.lyric_rounds,
                lyric_size_kb=args.lyric_size_kb,
            )
            peak_memory = bench_peak_memory(base_url, rounds=args.memory_rounds)
            singleflight = bench_singleflight(
                requests=args.singleflight_fanout,
                rounds=args.singleflight_rounds,
                upstream_latency_ms=args.latency_ms,
            )
            api_cache = bench_api_cache(
                rounds=args.api_cache_rounds,
                upstream_latency_ms=args.latency_ms,
            )
            shared_api_cache = bench_shared_api_cache(
                rounds=args.shared_cache_rounds,
                payload_kb=args.shared_cache_payload_kb,
            )
        finally:
            server.shutdown()
            server.server_close()

    report = Report(
        status=status,
        status_render=status_render,
        first_download=first_download,
        lyric_upload=lyric_upload,
        peak_memory=peak_memory,
        singleflight=singleflight,
        api_cache=api_cache,
        shared_api_cache=shared_api_cache,
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

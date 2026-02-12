# Parallel Optimization Baseline (2026-02-11)

Baseline captured before applying Workstreams A-F.

## Command Output (`python3 scripts/perf_compare.py`)

# v1.1.16 Performance Comparison

Synthetic local benchmark comparing before/after strategies.

## /status Query Path

| Metric | Before (3 queries) | After (1 query) |
|---|---:|---:|
| First latency (ms) | 0.59 | 0.21 |
| Mean latency (ms) | 0.59 | 0.20 |
| P95 latency (ms) | 0.60 | 0.21 |
| Speedup | - | 2.89x |

## First Download Latency Model

| Metric | Before (audio + cover + cover) | After (audio + cover) |
|---|---:|---:|
| First latency (ms) | 72.42 | 56.94 |
| Mean latency (ms) | 83.20 | 55.04 |
| P95 latency (ms) | 86.95 | 58.87 |
| Speedup | - | 1.51x |

## Peak Memory Model

| Metric | Before | After |
|---|---:|---:|
| Peak allocated memory (MB) | 16.15 | 12.14 |
| Reduction (%) | - | 24.80% |

## Singleflight Fanout Model

Requests per round: 20, rounds: 40

| Metric | Before | After |
|---|---:|---:|
| Mean latency (ms) | 45.37 | 44.10 |
| P95 latency (ms) | 46.51 | 46.63 |
| Upstream calls / round | 40.00 | 2.00 |
| Call reduction (%) | - | 95.00% |

## API Cache Hit Model

Rounds: 200

| Metric | Before (always miss) | After (warm cache) |
|---|---:|---:|
| First latency (ms) | 22.54 | 19.34 |
| Mean latency (ms) | 21.40 | 0.10 |
| P95 latency (ms) | 22.54 | 0.00 |
| Speedup | - | 221.08x |

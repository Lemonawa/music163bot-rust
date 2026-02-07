# v1.1.16 Performance Comparison

Synthetic local benchmark comparing before/after strategies.

## /status Query Path

| Metric | Before (3 queries) | After (1 query) |
|---|---:|---:|
| First latency (ms) | 0.73 | 0.28 |
| Mean latency (ms) | 0.73 | 0.27 |
| P95 latency (ms) | 0.75 | 0.27 |
| Speedup | - | 2.76x |

## First Download Latency Model

| Metric | Before (audio + cover + cover) | After (audio + cover) |
|---|---:|---:|
| First latency (ms) | 75.92 | 62.57 |
| Mean latency (ms) | 86.66 | 58.42 |
| P95 latency (ms) | 94.79 | 64.23 |
| Speedup | - | 1.48x |

## Peak Memory Model

| Metric | Before | After |
|---|---:|---:|
| Peak allocated memory (MB) | 16.15 | 12.14 |
| Reduction (%) | - | 24.81% |

## Singleflight Fanout Model

Requests per round: 20, rounds: 40

| Metric | Before | After |
|---|---:|---:|
| Mean latency (ms) | 52.25 | 49.12 |
| P95 latency (ms) | 54.83 | 55.13 |
| Upstream calls / round | 40.00 | 2.00 |
| Call reduction (%) | - | 95.00% |

## API Cache Hit Model

Rounds: 200

| Metric | Before (always miss) | After (warm cache) |
|---|---:|---:|
| First latency (ms) | 27.04 | 25.59 |
| Mean latency (ms) | 24.28 | 0.13 |
| P95 latency (ms) | 27.04 | 0.00 |
| Speedup | - | 189.62x |

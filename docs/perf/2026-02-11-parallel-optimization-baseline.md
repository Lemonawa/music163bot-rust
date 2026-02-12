# Performance Optimization Baseline

**Date:** 2026-02-12
**Version:** 1.1.17

## Baseline Metrics

### Upload Latency
- `[pre_upload_path]`: (To be recorded)
- `[upload_audio]`: (To be recorded)

### Command Latency
- Lyric Command: (To be recorded)

### Throughput
- Memory Mode: (To be recorded)
- Disk Mode: (To be recorded)

## Baseline Comparison Output

(Generated from scripts/perf_compare.py)

# v1.1.17 Baseline Performance Comparison

Synthetic local benchmark comparing before/after strategies.

## /status Query Path

| Metric | Before (3 queries) | After (1 query) |
|---|---:|---:|
| First latency (ms) | 0.63 | 0.21 |
| Mean latency (ms) | 0.60 | 0.21 |
| P95 latency (ms) | 0.60 | 0.21 |
| Speedup | - | 2.87x |

## First Download Latency Model

| Metric | Before (audio + cover + cover) | After (audio + cover) |
|---|---:|---:|
| First latency (ms) | 77.15 | 50.19 |
| Mean latency (ms) | 78.33 | 52.20 |
| P95 latency (ms) | 81.09 | 54.06 |
| Speedup | - | 1.50x |

## Peak Memory Model

| Metric | Before | After |
|---|---:|---:|
| Peak allocated memory (MB) | 16.15 | 12.14 |
| Reduction (%) | - | 24.79% |

## Singleflight Fanout Model

Requests per round: 20, rounds: 40

| Metric | Before | After |
|---|---:|---:|
| Mean latency (ms) | 45.07 | 43.87 |
| P95 latency (ms) | 46.02 | 46.04 |
| Upstream calls / round | 40.00 | 2.00 |
| Call reduction (%) | - | 95.00% |

## API Cache Hit Model

Rounds: 200

| Metric | Before (always miss) | After (warm cache) |
|---|---:|---:|
| First latency (ms) | 19.68 | 22.54 |
| Mean latency (ms) | 21.28 | 0.11 |
| P95 latency (ms) | 22.54 | 0.00 |
| Speedup | - | 188.61x |


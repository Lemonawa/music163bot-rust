# Cold Path Real E2E Baseline (2026-02-20)

## Context
- Objective: reduce `/music` cold-path p95 latency with real-traffic evidence.
- Decision metric: `e2e_total` p95 from structured `PERF` logs.
- Synthetic benchmark is recorded as reference only.

## Data Source
- Log source: `data/manual-test/coldpath.log`
- Parser: `scripts/perf_log_report.py`
- Sample target: `>= 30` cold requests per topology

## Topology Baseline

| Topology | Requests | e2e p50 (ms) | e2e p95 (ms) | e2e max (ms) | Notes |
|---|---:|---:|---:|---:|---|
| official_api | TBD | TBD | TBD | TBD | |
| selfhost_api_uri_upload / selfhost_api_multipart_upload | TBD | TBD | TBD | TBD | |

## Stage Share Snapshot (p95)

| Topology | Dominant Stage 1 | Dominant Stage 2 | Dominant Stage 3 |
|---|---|---|---|
| official_api | TBD | TBD | TBD |
| selfhost_api_uri_upload / selfhost_api_multipart_upload | TBD | TBD | TBD |

## Synthetic Reference (Non-decision)
- Command: `python3 scripts/perf_compare.py`
- Reference artifact: `docs/perf/2026-02-11-parallel-optimization-baseline.md`

## Optimization Loop
1. Collect baseline real logs.
2. Apply minimal hot-path optimization.
3. Re-sample with same workload shape.
4. Compare `e2e_total` p95 and stage p95 shares.
5. Iterate until soft target (~20%) or clear bottleneck plateau.

## Result Summary (Fill After Run)
- official_api p95 delta: `TBD`
- selfhost_api_uri_upload/selfhost_api_multipart_upload p95 delta: `TBD`
- achieved soft target (~20%): `TBD`
- next priority if not reached: `TBD`

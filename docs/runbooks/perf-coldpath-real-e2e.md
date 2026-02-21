# Cold Path Real E2E Performance Runbook

## Goal
Measure real `/music` cold-path latency using production-like traffic and parse structured `PERF|...` logs.

## Scope
- Primary path: first `/music` cold requests (cache miss)
- Topologies:
  - `official_api`
  - `selfhost_api_uri_upload` (self-hosted API + URI upload mode)
  - `selfhost_api_multipart_upload` (self-hosted API + multipart upload mode)
- Decision metric: `e2e_total` p95

## Prerequisites
1. Build and run the bot with `loglevel=info` or `loglevel=debug`.
2. Ensure requests are cold path samples (avoid repeated same `music_id` cache hits).
3. Collect logs to file.

Example:

```bash
cargo run --release -- --config config.ini 2>&1 | tee data/manual-test/coldpath.log
```

## Sampling Plan
1. Official API: capture at least 30 cold-path requests.
2. Self-hosted API mode (local or remote deployment): capture at least 30 cold-path requests.
3. Separate self-hosted data by upload mode (`selfhost_api_uri_upload` vs `selfhost_api_multipart_upload`).
4. Keep request set comparable (song size/category mix).

## Parse and Report
Generate overall report:

```bash
python3 scripts/perf_log_report.py \
  --log-file data/manual-test/coldpath.log \
  --cache-path miss_cold \
  --markdown-output docs/perf/2026-02-20-coldpath-overall.md \
  --json-output docs/perf/2026-02-20-coldpath-overall.json
```

Generate per-topology reports:

```bash
python3 scripts/perf_log_report.py \
  --log-file data/manual-test/coldpath.log \
  --topology official_api \
  --cache-path miss_cold \
  --markdown-output docs/perf/2026-02-20-coldpath-official.md \
  --json-output docs/perf/2026-02-20-coldpath-official.json

python3 scripts/perf_log_report.py \
  --log-file data/manual-test/coldpath.log \
  --topology selfhost_api_uri_upload \
  --cache-path miss_cold \
  --markdown-output docs/perf/2026-02-20-coldpath-local-uri.md \
  --json-output docs/perf/2026-02-20-coldpath-local-uri.json
```

If URI upload is disabled in a run, use topology filter `selfhost_api_multipart_upload`.

## Acceptance Rule
- Soft target: cold-path `e2e_total` p95 improves by ~20%.
- If target is not reached, still deliver:
  - stage-level p95 shares (`upload_send`, `tag_process`, `select_url`, `db_save`, etc.)
  - bottleneck evidence and next optimization priorities.

## Reference (Synthetic)
Synthetic benchmark remains useful for regression sanity checks only:

```bash
python3 scripts/perf_compare.py
```

Do not use synthetic output as final optimization decision data.

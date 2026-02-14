# Upload Log Level Design

## Goal
Add a dedicated upload diagnostic log level so operators can enable/disable upload instrumentation without changing global logging.

## Scope
- New config key: `upload.log_level`.
- Allowed values (case-insensitive): `NONE`, `ERROR`, `WARNING` (alias `WARN`), `INFO`, `DEBUG`.
- Default: `INFO`.
- Applies only to new upload diagnostic logs (not existing business logs).

## Behavior
- When set to `NONE`, all new upload diagnostic logs are suppressed.
- Levels act as a threshold: `DEBUG` shows all, `INFO` shows info+warn+error, `WARNING` shows warn+error, `ERROR` shows only errors.
- Invalid values log a warning and keep the default.

## Diagnostics
- Add upload diagnostics around client reuse decisions and client construction timing.
- Include configured pool settings and reuse counter in diagnostics.
- Do not log secrets (token, cookies).

## Configuration Guidance
- Keep existing defaults (no behavior change).
- Document an optional A/B test suggestion in `config.ini.example`:
  - `upload.client_reuse_requests=10`
  - `upload.pool_max_idle_per_host=1`


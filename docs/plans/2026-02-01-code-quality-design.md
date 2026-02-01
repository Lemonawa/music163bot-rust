# Code Quality Stabilization Design

**Goal:** Reduce runtime panic risk and improve error observability without changing user-facing behavior.

## Scope and Constraints
This change set targets stability and correctness improvements in existing code paths. The focus is on eliminating `unwrap`/`expect` in production paths, fixing configuration default inconsistencies, and ensuring cryptographic errors are surfaced instead of silently ignored. Feature behavior should remain the same from a user perspective, and no new configuration keys are introduced. We avoid reworking async architecture, I/O pathways, or user-facing flows. Upload fallback behavior explicitly stays as-is.

## Architecture and Data Flow
The design keeps all modules and interfaces intact. Error handling is tightened at the edges: URL parsing and HTTP client creation will return structured errors, semaphore acquisition failures will be surfaced through existing error types, and configuration parsing will preserve `Config::default()` values on parse failure. For API encryption in `MusicApi`, the eapi parameter builder will return a `Result`, and `search_songs` will propagate errors through the existing `Result` type. This keeps changes localized to the modules where the errors occur and avoids cross-module refactoring.

## Error Handling and Observability
Where failures are non-critical (status counters, formatting helpers), the design favors logging and safe fallback responses over panics. Critical-path failures (client creation, URL parsing, encryption) are returned as errors with context. This makes operational issues visible without reducing availability.

## Testing Strategy
Each behavior change is introduced using unit tests first. Tests are small and targeted: configuration parsing defaults, URL parsing fallback behavior, and error propagation from eapi parameter generation. Async-related behavior is tested with minimal helpers where necessary. Tests avoid network I/O.

## Out of Scope
- Upload fallback behavior and re-try logic
- API behavior changes or new features
- Large refactors of bot handler flows

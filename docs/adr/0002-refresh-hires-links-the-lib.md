# refresh_hires links against the library; no #[path] includes

The crate has a lib target (`src/lib.rs`). Maintenance binaries — including
`refresh_hires` — must `use music163bot_rust::…` instead of `#[path]`-including
library source files or hand-copying domain logic (eapi request/decode ladder,
DTOs, SQL). When a tool needs something from the bot's domain (e.g. probing the
served size for cached songs), extend the domain module's interface
(`MusicApi::get_served_sizes_batch` was added this way) rather than duplicating
its implementation in the binary.

Earlier versions of the tool `#[path]`-included `eapi_crypto.rs` and `ini.rs`
under a second crate root; those files then had to stay dependency-restricted by
comment. With a lib target that constraint is gone — do not reintroduce it.

## Considered Options

- Keep `#[path]` includes and copy the rest: zero Cargo changes, but every
  domain change must be mirrored by hand and nothing keeps the copies honest.
- A separate `music-api` crate: cleaner layering than we need for one tool;
  the lib target already gives the tool a stable interface.

# GitHub Copilot Instructions for gtvfx-envoy/telemetry

This file contains coding standards and environment/tooling facts GitHub Copilot
should follow when working in this repository. See `README.md` for the overall
architecture (client → Collector/Tempo/Grafana, both transports, auth model). This
file is for conventions and hard-won gotchas, not architecture — don't duplicate the
README here.

## Rust (this repo is 100% Rust)

`rust/telemetry-controller` is an independent Cargo workspace from `envoy`/
`envoy-cli` (mirrors how `envoy_utils`'s `engit` is independent) — never add it as a
dependency of or merge it into the `envoy` repo's own workspace.

### Error handling
Fallible functions return `Result<T, String>` (a human-readable message), not a
custom `thiserror` error enum — this is a deliberate, simpler convention for this
small single-binary CLI tool, distinct from `envoy-core`'s per-crate `thiserror` enums
(`envoy-core::error::EnvoyError`, etc.). Match `Result<T, String>` for new fallible
functions here rather than introducing `thiserror`/`anyhow`.

### Tests
- Unit tests live in a `#[cfg(test)] mod tests` block at the bottom of the same file.
- Any test that mutates a real process environment variable (`ENVOY_CONFIG_ROOT`,
  `COLLECTOR_BEARER_TOKEN`, etc. via `std::env::set_var`/`remove_var`) must both use
  an `EnvVarGuard` RAII struct (save previous value, restore on `Drop` — see
  `config_root.rs`/`state.rs` for the established shape) **and** acquire a
  file-local `static TEST_LOCK: std::sync::Mutex<()>` before mutating. Unlike
  `envoy-core` (which shares one crate-wide `env_test_lock::MUTEX`), this crate uses
  a separate lock per file/module — don't assume a shared crate-wide lock exists
  here; add a new local one if a file doesn't already have one.

### Validation gate
```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
All three must be clean before considering a change complete (mirrors
`.github/workflows/lint.yml`).

## Docker development environment

Docker is not available directly on Windows in this environment (no Docker Desktop);
use the working Docker Engine + Compose plugin inside the WSL2 `Ubuntu` distro
instead. See the `docker-wsl2` skill (`_config/.skills/docker-wsl2/SKILL.md`) for the
exact invocation pattern, including a real quoting pitfall (`bash -c '...'` single-line
vs. multi-line `bash -lc "..."`) and why `cargo`/`rustc` need an absolute path (e.g.
`/home/USER/.cargo/bin/cargo`) rather than relying on `bash -c`'s non-login-shell PATH.

**Build the Linux `telemetry-controller` release binary inside a container matching
the sweep image's own base (`rust:1-bookworm` for `debian:bookworm-slim`), not on the
bare WSL2/CI host.** A binary built on a newer host glibc (e.g. Ubuntu 24.04+, glibc
2.39) fails inside `debian:bookworm-slim` (glibc 2.36) with
`` version `GLIBC_2.39' not found ``. This affects the real CI pipeline too (its Linux
job targets `ubuntu-24.04`), not just local builds — if `build-release.yml`'s Linux
job ever stops building inside a matching container, the sweep's own Docker image
will silently ship a broken binary.

## Known upstream-service gotchas (found via live end-to-end testing)

These were only discoverable by actually running the stack — verify directly against
the real pinned image/version (e.g. `docker run ... -config.verify=true` for Tempo)
rather than trusting upstream docs alone when touching these areas, since "latest"
docs can describe a newer architecture than the pinned version.

- **Tempo 3.x removed `ingester:`/`compactor:` entirely, in every deployment mode
  including monolithic.** Retention lives at the flat `overrides.block_retention` (no
  `defaults:` wrapper) plus `overrides.enable_legacy_overrides: true` — not
  `compactor.compaction.block_retention`, and not `overrides.defaults.block_retention`
  either (both fail to parse against the real `grafana/tempo:3.0.3` binary). See
  `compose/tempo/tempo.yaml`'s comments and `rust/telemetry-controller/src/
  tempo_config.rs` for the verified-working shape.
- **The sweep container has no access to the host's `~/.envoy` state file.** The
  shared Collector bearer token must be passed explicitly (`--bearer-token` /
  `COLLECTOR_BEARER_TOKEN` env var in `docker-compose.yml`), not read via
  `load_state()` inside the container — that always resolves to "no token" there and
  silently rejects every forward attempt as unauthenticated forever (no error, just
  `retried=1` forever).
- **Every `docker compose` subcommand interpolates the whole compose file up front**,
  including required (`:?`) env var references — so `down`/`logs`/`ps`, not just
  `up`, need the same env vars supplied or they fail outright. See
  `compose_env_from_state()` in `commands.rs`.
- **Grafana applies `GF_SECURITY_ADMIN_PASSWORD` synchronously before its HTTP server
  starts listening.** Wait for the unauthenticated `/api/health` endpoint before any
  authenticated provisioning call. Retrying an authenticated call through a 401
  instead is actively harmful, not just ineffective: Grafana's brute-force login
  protection then locks the account out for its own cooldown window, which a
  short-interval retry loop keeps re-triggering indefinitely — the account never gets
  provisioned even though the password was correct the whole time.
- **The `otel/opentelemetry-collector-contrib` image has no shell, `wget`, or `curl`**,
  and the `otelcol-contrib` binary itself has no health-probe subcommand. Don't add a
  Docker-level `healthcheck:` that execs a binary inside that image — it can never
  succeed and will eventually report "unhealthy" forever regardless of real status,
  which is worse than no healthcheck.
- **`docker compose up` alone does not rebuild a `build:`-context service's image on
  subsequent runs** — it only builds when the image is missing, silently reusing a
  stale cached image (e.g. the sweep's `telemetry-controller` binary) even after the
  build context's contents changed. `compose::up()` passes `--build` for exactly this
  reason; don't remove it.

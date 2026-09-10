# Contributing to the envoy telemetry bundle

This is the developer-facing setup guide: how to build, test, and run this
bundle locally so you can make and verify changes. For the bundle's
architecture (client -> Collector/Tempo/Grafana, both transports, the auth
model) see `README.md`; that's operator-facing documentation and isn't
repeated here. For Rust-specific conventions and known upstream-service
gotchas, see `.github/copilot-instructions.md` -- this guide focuses on the
step-by-step local workflow, and links to that file for the "why" behind a
few of the steps below rather than duplicating it.

## Prerequisites

- **Rust** (stable toolchain, with `rustfmt` and `clippy` components) --
  builds and tests `rust/telemetry-controller` on any platform.
- **Docker + the Compose plugin** -- to run the Collector/Tempo/Grafana/sweep
  stack locally.
  - Linux/macOS: install Docker normally; `docker` and `docker compose`
    should already be on `PATH`.
  - Windows without Docker Desktop: run everything Docker-related from
    inside a WSL2 distro with Docker Engine installed (`wsl -d <distro> --
    bash -c '...'`). Every command below that needs `docker` assumes a
    shell where it's actually reachable.
- **`curl`** (or similar) -- only needed if you want to manually submit a
  test span without writing code (see below).

## Building and testing the Rust workspace

From `rust/` (the workspace root, independent from `envoy`/`envoy-cli`'s own
workspace -- see `.github/copilot-instructions.md`):

```bash
cd rust
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

All three must be clean before opening a PR (this mirrors
`.github/workflows/lint.yml` exactly, which runs the same three commands on
Linux, Windows, and macOS).

## Running the full stack locally

The Compose bundle's `sweep` service builds its own image from a
pre-compiled Linux `telemetry-controller` binary (see
`compose/sweep/Dockerfile`) -- it does not compile Rust itself. A real
publish stages that binary from CI; for local development, build it
yourself once:

### 1. Build the Linux binary

**This must be built inside a container matching the sweep image's own base
(`rust:1-bookworm`, for `debian:bookworm-slim`), not with your host's own
Rust toolchain.** A binary built against a newer host glibc (e.g. Ubuntu
24.04+, glibc 2.39) fails to even start inside `debian:bookworm-slim` (glibc
2.36) with `` version `GLIBC_2.39' not found ``. This bites Linux hosts and
WSL2 alike, not just macOS/Windows -- always build this way, even on a
native Linux dev machine.

```bash
# From the repository root:
docker run --rm -v "$(pwd)/rust:/workspace" -w /workspace rust:1-bookworm \
  cargo build --release -p telemetry-controller
```

On Windows without Docker Desktop, prefix this with your WSL2 distro
invocation, e.g. `wsl -d Ubuntu -- bash -c '...'` (see the Windows notes
below).

### 2. Stage it where the sweep's build context expects it

```bash
mkdir -p native
cp rust/target/release/telemetry-controller native/telemetry-controller
```

`native/` is gitignored -- it's a local (or CI-staged) build artifact, never
committed source (see `.envoy/publish-manifest.yaml`).

### 3. Start the stack

Run the controller binary itself (not raw `docker compose`) so it generates
and wires up credentials for you:

```bash
# Must run somewhere `docker compose` is actually on PATH (native
# Linux/macOS, or inside your WSL2 distro on Windows).
./rust/target/release/telemetry-controller start --runtime compose
```

This builds the sweep image (`--build` is always applied -- see the
comment on `compose::up()` for why that's load-bearing, not cosmetic),
starts all four services, and prints the generated shared `studio-viewer`
Grafana password. `telemetry-controller status` reprints it later if you
lose it.

### 4. Verify it's actually working

```bash
./rust/target/release/telemetry-controller status
./rust/target/release/telemetry-controller open   # or browse to http://localhost:3000
```

Log into Grafana as `studio-viewer` with the printed password. The
"Envoy Command Telemetry" dashboard is provisioned automatically.

### 5. Submit a test span

Two ways in, matching the bundle's two supported transports:

**Direct OTLP/HTTP** (what a workstation with an `http(s)://
ENVOY_TELEMETRY_ENDPOINT` does). Get the bearer token from `status`'s
underlying state file (`~/.envoy/telemetry-server/state.json`, or the
platform equivalent -- see `copilot-instructions.md`/`config_root.rs`), then:

```bash
curl -X POST http://localhost:4318/v1/traces \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer <token-from-state.json>" \
  --data-binary @- <<'EOF'
{"resourceSpans":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"dev-test"}}]},"scopeSpans":[{"scope":{"name":"dev-test"},"spans":[{"traceId":"5b8efff798038103d269b633813fc60c","spanId":"eee19b7ec3c1b174","name":"dev-test-span","kind":1,"startTimeUnixNano":"1700000000000000000","endTimeUnixNano":"1700000001000000000"}]}]}]}
EOF
```

A `{"partialSuccess":{}}` response means the Collector accepted it.

**File-drop** (what a workstation with a filesystem/UNC
`ENVOY_TELEMETRY_ENDPOINT` does -- exercises the `sweep` service):

```bash
cat > compose/drop/dev-test.json <<'EOF'
{"name":"envoy.command.run","attributes":{"envoy.command.name":{"Str":"dev-test"}},"timestamp_unix_millis":1700000000000}
EOF
```

The sweep polls every 10 seconds and deletes the file once forwarded; a
file that keeps failing ends up in `compose/drop/dead-letter/` instead (see
`sweep.rs`).

Either way, confirm the span landed by searching for it on the Grafana
dashboard, or query Tempo directly through Grafana's datasource proxy:
`GET /api/datasources/proxy/uid/<tempo-datasource-uid>/api/search?tags=service.name=dev-test`
(Basic-auth as `studio-viewer`; find the datasource UID via `GET
/api/datasources`).

### 6. Stop when you're done

```bash
./rust/target/release/telemetry-controller stop
```

This always preserves Tempo/Grafana data (their Docker volumes aren't
removed) -- a subsequent `start` picks up right where you left off.

## Windows notes (WSL2)

Without Docker Desktop, run every Docker-touching command through
`wsl -d <distro> -- bash -c '...'`:

- **Keep it single-line.** A multi-line `bash -c '...'` (or `-lc "..."`)
  string passed through `wsl.exe` from PowerShell can be silently
  mis-split/corrupted. Chain steps with `&&`/`;` on one line, or write a
  `.sh` file and invoke `bash /mnt/c/path/to/script.sh` instead.
- Windows paths under `V:\...` map to `/mnt/v/...` (drive letter
  lowercased) inside WSL2, reachable with no copying needed.
- **WSL2's own idle-shutdown can tear down the VM (and every container in
  it) between commands** if you leave long gaps between `wsl -d ...`
  invocations during manual/interactive testing. If containers seem to have
  restarted unexpectedly, that's this, not a bug in the bundle -- keep a
  long-lived no-op session open in the background during an extended manual
  test session (e.g. `wsl -d <distro> -- sleep 1800`) to prevent it.

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| Sweep image fails to build: context/binary not found | `native/telemetry-controller` doesn't exist yet | Build it first (see step 1-2 above) |
| Container fails with `` version `GLIBC_2.39' not found `` | Binary built with a newer host glibc than `debian:bookworm-slim` ships | Always build inside `rust:1-bookworm` (step 1), never with your host toolchain |
| `docker compose down`/`logs`/`ps` fail with "variable ... is missing a value" | Those subcommands also interpolate the compose file's required env vars, even though they don't use the values | Always go through `telemetry-controller`, which supplies them automatically; don't invoke `docker compose` directly with a partial environment |
| Sweep retries every file forever / everything ends up dead-lettered | Sweep container has no access to the host's controller state file | Confirm `COLLECTOR_BEARER_TOKEN` is actually set in the sweep service's environment (docker-compose.yml wires this explicitly -- don't remove it) |
| Grafana admin login locks out after a few tries right after `start` | Grafana applies `GF_SECURITY_ADMIN_PASSWORD` before its HTTP server is ready; hitting it too early then retrying repeats a genuine 401 into Grafana's brute-force lockout | Wait for `/api/health` (unauthenticated) before any authenticated call; `telemetry-controller start` already does this for its own provisioning -- don't add your own tighter retry loop against an authenticated endpoint |
| Collector container shows "unhealthy" | N/A -- there is deliberately no Docker-level `healthcheck:` for the collector service | The image has no shell for Docker to exec into; use the Collector's own `health_check` extension (port 13133, internal to the Compose network) from another container instead |
| Code changes to `telemetry-controller` don't seem to take effect in the sweep container | `docker compose up` alone only builds a `build:`-context image the first time it's missing | Always go through `telemetry-controller start`, which passes `--build` every time; if invoking Compose directly, add `--build` yourself |

## Where to make changes

- `rust/telemetry-controller/` -- the lifecycle controller (`start`, `stop`,
  `status`, `logs`, `open`, `configure`, `sweep`).
- `compose/` -- Compose definitions, Collector/Tempo config, Grafana
  provisioning, and the dashboard JSON.
- `.envoy/` -- bundle registration (`commands.json`/`global_env.json`) and
  publish staging (`publish-manifest.yaml`).

See `README.md`'s own "Repository layout" section for more detail. Update
`VERSIONS.lock` and `THIRD_PARTY_LICENSES/README.md` together, in the same
change, whenever a bundled image/artifact version changes.

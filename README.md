# telemetry

Shared studio infrastructure for aggregating `envoy` command-usage
telemetry from every workstation on the team into one authenticated
Grafana dashboard. This bundle is operated by pipeline/IT on one (or a
small number of) server(s) -- **not** installed on individual artist
workstations.

## How it fits together

```text
workstation (envoy)  --ENVOY_TELEMETRY_ENDPOINT-->  studio server (this bundle)
  http(s) URL  -> direct OTLP/HTTP  -----------------> Collector
  any other path -> atomic file-drop  ---------------> shared UNC/network path -> sweep -> Collector

Collector -> Tempo (persistent local storage) -> Grafana (this dashboard)
```

- Workstations need **no Docker, no local services, and no bundle
  changes** -- only `ENVOY_TELEMETRY_ENDPOINT` resolved (typically baked
  into your studio's own `gt/envoy` bundle's `global_env.json` so it
  applies automatically) and envoy's own built-in exporter
  (`envoy-core::telemetry`). See that crate's docs for the full client-side
  contract (redaction, local spool/retry, `--diagnose` reporting).
- The server side (this bundle) is what you're reading about below.

## Server setup

Requires Docker + the Compose plugin on the server.

Download `telemetry-v<version>.zip` and `SHA256SUMS` from the matching
[GitHub Release](https://github.com/gtvfx-envoy/telemetry/releases). Verify
the archive before extracting it into the `gt`
namespace beneath a directory already covered by `ENVOY_BNDL_ROOTS`:

```bash
sha256sum --check SHA256SUMS
mkdir -p "$HOME/.local/share/envoy/bundles/gt"
unzip telemetry-v0.1.0.zip -d "$HOME/.local/share/envoy/bundles/gt"
export ENVOY_BNDL_ROOTS="$HOME/.local/share/envoy/bundles"
envoy --list
```

The example produces
`~/.local/share/envoy/bundles/gt/telemetry/v0.1.0/.bundle`; `envoy --list`
should show the `telemetry` command before the server is started. Use the
release version you actually downloaded, and persist `ENVOY_BNDL_ROOTS`
through the server's normal environment configuration rather than relying on
the temporary `export` above.

```text
envoy telemetry start [--runtime auto|compose]
envoy telemetry stop
envoy telemetry status
envoy telemetry logs [collector|tempo|grafana|sweep]
envoy telemetry open
envoy telemetry configure --retention-days=N
```

- `start` is explicit and idempotent -- installing/updating this bundle
  never launches anything on its own, and running `start` again while
  already running is a no-op.
- `auto` (the default) selects Docker Compose and reports an actionable error
  when it is unavailable.
- `stop` always preserves telemetry data (Tempo's storage volume is never
  removed).
- Generated credentials and state live under the platform
  Envoy config root (`~/.envoy/telemetry-server/`, honoring
  `ENVOY_CONFIG_ROOT`), never inside the published bundle itself.
- Back up `state.json` with the server configuration. If it is missing while
  persistent Grafana/Tempo volumes still exist, `start` fails closed rather
  than generating credentials that cannot unlock the retained Grafana data.
- `status` hides credentials by default. Use
  `envoy telemetry status --show-credentials` from an operator-controlled
  terminal when they must be recovered.

### Authentication

- **Grafana**: no anonymous access. If your studio doesn't have SSO/LDAP
  wired up for internal tools (most don't, and that's fine) --
  `telemetry-controller start` provisions a shared **`studio-viewer`**
  account with a generated password, printed once at `start` and
  recoverable via `envoy telemetry status --show-credentials`. Give that
  credential to every artist; nobody needs their own account. A separate, random
  break-glass **admin** account is also generated for operator use only.
  SSO/LDAP remains a supported upgrade path -- configure it the normal
  Grafana way and it supersedes the shared-credential default.
- **Collector**: every OTLP submission must present the shared bearer
  token generated at `start` (this is what workstations pick up via
  `OTEL_EXPORTER_OTLP_HEADERS`, alongside `ENVOY_TELEMETRY_ENDPOINT`).
- **Tempo**: never exposed to the network directly -- reachable only from
  the Collector and Grafana over the internal Compose network.

### Retention

30 days by default. The controller copies the bundled Tempo template into
`~/.envoy/telemetry-server/runtime/tempo.yaml`; that mutable runtime copy
survives bundle upgrades. `envoy telemetry configure --retention-days=N`
updates it and prompts you to restart the server so Tempo picks up the change.

### Runtimes

| Runtime | Platforms | Notes |
|---|---|---|
| `compose` | Windows, Linux, macOS | Requires Docker + the Compose plugin. |

### Ingestion sweep

Workstations whose `ENVOY_TELEMETRY_ENDPOINT` resolves to a filesystem/UNC
path (rather than an `http(s)://` URL) write atomic OTLP-JSON files there
instead of talking to the Collector directly -- no listening service is
needed at that path. The `sweep` service polls it, forwards each file to
the local Collector, and deletes it on success. A file that keeps failing
is retried a bounded number of times, then moved to a `dead-letter/`
subdirectory instead of retried forever.

## The dashboard

Provisioned immutably from `compose/grafana/dashboards/envoy-command-telemetry.json`
(UI edits are disabled) -- see that file for the full panel/variable list.
Built entirely on Tempo + TraceQL metrics (no Prometheus/Loki/Mimir in
v1); see `envoy-core::telemetry::schema` for the attribute names every
panel queries against, and that module's doc comment for the version-skew
policy that keeps older/newer clients compatible with this dashboard
without a migration.

## Licensing

This bundle runs unmodified upstream Grafana (`AGPL-3.0-only`), Tempo
(`Apache-2.0`), and OpenTelemetry Collector Contrib (`Apache-2.0`)
binaries as internal infrastructure -- see `THIRD_PARTY_LICENSES/README.md`
for details and `VERSIONS.lock` for exact pinned versions/checksums.
**Before your studio's first rollout**, confirm with whoever handles
license compliance (Grafana's AGPL) and whoever owns
employee-monitoring/privacy policy (this dashboard aggregates every
workstation's usage) -- both are policy decisions, not just technical
ones. See `PILOT_CHECKLIST.md` for a step-by-step checklist covering this
and the rest of a first small studio pilot rollout.

## Repository layout

- `rust/telemetry-controller/` -- the lifecycle controller (`envoy
  telemetry ...`), an independent Rust workspace/binary from `envoy`
  itself (mirrors how `engit` is independent from `envoy-cli`).
- `compose/` -- Docker Compose definitions, Collector/Tempo config,
  Grafana provisioning, and the dashboard JSON.
- `.envoy/` -- `commands.json`/`global_env.json` (registers `envoy
  telemetry ...`) and `publish-manifest.yaml` (controller-binary artifact
  staging for `engit publish bundle`).
- `VERSIONS.lock`, `THIRD_PARTY_LICENSES/` -- pinned versions and license
  inventory for every bundled third-party service.

## Contributing

Building/testing the Rust workspace, running the full stack locally for
development, and troubleshooting the common gotchas: see `CONTRIBUTING.md`.

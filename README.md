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

Requires Docker + the Compose plugin on the server (the native, Docker-free
path currently supports Linux x86-64 only -- see below).

```text
envoy telemetry start [--runtime auto|compose|native]
envoy telemetry stop
envoy telemetry status
envoy telemetry logs [collector|tempo|grafana|sweep]
envoy telemetry open
envoy telemetry configure --retention-days=N
```

- `start` is explicit and idempotent -- installing/updating this bundle
  never launches anything on its own, and running `start` again while
  already running is a no-op.
- `auto` (the default) prefers Docker Compose when available, otherwise
  falls back to native mode on Linux. Windows and macOS require Docker.
- `stop` always preserves telemetry data (Tempo's storage volume is never
  removed).
- Generated credentials, PID files, and state live under the platform
  Envoy config root (`~/.envoy/telemetry-server/`, honoring
  `ENVOY_CONFIG_ROOT`), never inside the published bundle itself.

### Authentication

- **Grafana**: no anonymous access. If your studio doesn't have SSO/LDAP
  wired up for internal tools (most don't, and that's fine) --
  `telemetry-controller start` provisions a shared **`studio-viewer`**
  account with a generated password, printed once at `start` and
  recoverable via `envoy telemetry status`. Give that one credential to
  every artist; nobody needs their own account. A separate, random
  break-glass **admin** account is also generated for operator use only.
  SSO/LDAP remains a supported upgrade path -- configure it the normal
  Grafana way and it supersedes the shared-credential default.
- **Collector**: every OTLP submission must present the shared bearer
  token generated at `start` (this is what workstations pick up via
  `OTEL_EXPORTER_OTLP_HEADERS`, alongside `ENVOY_TELEMETRY_ENDPOINT`).
- **Tempo**: never exposed to the network directly -- reachable only from
  the Collector and Grafana over the internal Compose network.

### Retention

30 days by default. Tempo's own config file
(`compose/tempo/tempo.yaml`'s `overrides.block_retention`) is
the durable source of truth; `envoy telemetry configure
--retention-days=N` is a convenience wrapper that edits it (and prompts
you to restart the server so Tempo picks up the change) rather than
requiring you to hand-edit YAML.

### Runtimes

| Runtime | Platforms | Notes |
|---|---|---|
| `compose` (recommended) | Windows, Linux, macOS | Requires Docker + the Compose plugin. |
| `native` | Linux x86-64 only | No Docker; runs Grafana/Tempo/Collector/the sweep as plain background processes. For operators who'd rather not run Docker on the server. |

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
ones.

## Repository layout

- `rust/telemetry-controller/` -- the lifecycle controller (`envoy
  telemetry ...`), an independent Rust workspace/binary from `envoy`
  itself (mirrors how `engit` is independent from `envoy-cli`).
- `compose/` -- Docker Compose definitions, Collector/Tempo config,
  Grafana provisioning, and the dashboard JSON.
- `.envoy/` -- `commands.json`/`global_env.json` (registers `envoy
  telemetry ...`) and `publish-manifest.yaml` (native/controller-binary
  artifact staging for `engit publish bundle`).
- `VERSIONS.lock`, `THIRD_PARTY_LICENSES/` -- pinned versions and license
  inventory for every bundled third-party service.

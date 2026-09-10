# Pilot Rollout Checklist

A short, practical checklist for running this bundle as a **first, small
studio pilot** -- a handful of real workstations pointed at one real server,
before deciding on a wider rollout. This is deliberately lighter-weight than
a full public/tagged release: see "Known limitations acceptable for a
pilot" below for exactly what's still deferred, and why that's fine for
this stage.

For full architecture and command reference, see `README.md`; this
checklist only sequences and cross-references it.

## Before you start

- [ ] **Policy sign-off.** Confirm with whoever handles license compliance
      (Grafana is `AGPL-3.0-only` -- see `README.md`'s "Licensing" section
      and `THIRD_PARTY_LICENSES/README.md`'s AGPL note) and whoever owns
      employee-monitoring/privacy policy at your studio (this dashboard
      aggregates every participating workstation's command usage). Both are
      policy decisions for your studio to make, not something this repo can
      resolve for you -- do this before real usage data from real
      workstations starts flowing anywhere, even for a small pilot.
- [ ] **Pick the pilot server.** One machine (or a small number), reachable
      over the studio network by every pilot workstation, with Docker + the
      Compose plugin available (or a Linux x86-64 host if you'd rather use
      the Docker-free native runtime -- see `README.md`'s Runtimes table).

### Known limitations acceptable for a pilot (not a full release)

- `VERSIONS.lock`'s image/artifact **digests** are still
  `TODO: resolve at publish time` placeholders. This is fine for a pilot --
  every image is still pinned to a specific real version tag (never
  `:latest`), just not yet hardened to an immutable digest. Resolve these
  before a wider studio-wide rollout.
- There's **no tagged release yet**. A pilot runs from a locally-built
  controller binary (or an `engit publish bundle --dry-run`-validated
  layout) rather than a real `engit publish bundle` artifact. Fine for a
  pilot operated directly by the team building it; revisit before
  distributing this more broadly so operators aren't expected to build it
  themselves.

## Setting up the pilot server

1. Follow `README.md`'s "Server setup" section to install/register the
   bundle, then run:
   ```text
   envoy telemetry start [--runtime auto|compose|native]
   ```
2. Confirm the studio network can actually reach the server on the ports
   that matter: **4318** (Collector, direct OTLP/HTTP) and **3000**
   (Grafana). Tempo is deliberately never exposed -- don't open it up.
3. Record the generated `studio-viewer` Grafana password somewhere your
   pilot participants can find it (it's also always recoverable later via
   `envoy telemetry status`).
4. Check the default 30-day retention is acceptable for the pilot, or
   adjust with `envoy telemetry configure --retention-days=N`.

## Pointing pilot workstations at the server

- Set `ENVOY_TELEMETRY_ENDPOINT` for each pilot workstation -- an `http(s)://`
  URL for direct OTLP submission, or a filesystem/UNC path for the
  file-drop transport. Typically resolved automatically via your studio's
  own `gt/envoy` bundle's `global_env.json` rather than set per-workstation
  by hand.
- For the direct-URL transport, workstations also need the shared Collector
  bearer token (see `README.md`'s Collector auth note) -- this normally
  travels alongside `ENVOY_TELEMETRY_ENDPOINT` via
  `OTEL_EXPORTER_OTLP_HEADERS`.
- Start with a small, known set of pilot workstations before opening it up
  further.

## Monitoring during the pilot

- Periodically check `envoy telemetry status` and `envoy telemetry logs
  [collector|tempo|grafana|sweep]`.
- Watch the "Envoy Command Telemetry" Grafana dashboard for data actually
  arriving from pilot workstations.
- If any pilot workstations use the file-drop transport, periodically check
  that path's `dead-letter/` subdirectory. A growing dead-letter backlog
  usually means a bearer-token or connectivity mismatch between those
  workstations and the server, not a problem with the workstations'
  `envoy` installs themselves.

## After the pilot

- Gather feedback from pilot participants.
- Decide go/no-go for a wider studio rollout.
- Before a wider rollout (or any tagged release): resolve `VERSIONS.lock`'s
  outstanding digest placeholders and cut a real semver tag -- both
  deliberately out of scope for this pilot.

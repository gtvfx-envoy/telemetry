# Third-Party Notices

This bundle runs unmodified upstream binaries as **shared studio
infrastructure** (see `../README.md`). This file lists each one, its
license, and where to get the authoritative license text and source. Full
license texts are intentionally not retyped here from memory (risk of
transcription error in a legal document) -- see each `source` link, or the
copy embedded directly in the upstream release archive/image itself.

| Component | Version | License (SPDX) | Source |
|---|---|---|---|
| Grafana | 13.0.6 | `AGPL-3.0-only` | https://github.com/grafana/grafana |
| Grafana Tempo | 3.0.3 | `Apache-2.0` | https://github.com/grafana/tempo |
| OpenTelemetry Collector Contrib | 0.159.0 | `Apache-2.0` | https://github.com/open-telemetry/opentelemetry-collector-contrib |
| Debian (`bookworm-slim`, sweep service base image only) | bookworm | multiple (see Debian) | https://github.com/debuerreotype/debuerreotype |

See `../VERSIONS.lock` for exact pinned tags/versions and (once resolved
at publish time) immutable digests/checksums.

## AGPL note (Grafana)

Grafana is the only `AGPL-3.0-only`-licensed component here. Self-hosting
an **unmodified** upstream Grafana binary for an internal dashboard,
without redistributing Grafana itself further, does not on its own
trigger AGPL's network-copyleft clause (that clause is about *modified*
network-accessible versions of an AGPL program). This bundle does not
modify Grafana's source in any way -- only its configuration/provisioning,
which are ordinary runtime configuration, not a derivative work of
Grafana's own source.

That said, this is a **studio-specific compliance/policy question, not
just a technical one**. Confirm with whoever handles license compliance
at your studio before the first internal rollout, per the plan's
acceptance criteria -- this note is informational, not a substitute for
that sign-off.

## Updating this file

Whenever `VERSIONS.lock` changes a pinned version, update the table above
in the same change.

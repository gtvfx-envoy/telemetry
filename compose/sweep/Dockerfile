# Minimal runtime image for the file-drop ingestion sweep.
#
# The `telemetry-controller` binary is pre-built for linux/amd64 by the
# publish pipeline (see ../../.envoy/publish-manifest.yaml) and copied in
# at build time from the `native/` artifact directory -- this Dockerfile
# does not compile Rust itself, matching how native artifacts are staged
# rather than vendored (see ext/python's own publish-manifest.yaml
# pattern in the main envoy repo).

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY telemetry-controller /usr/local/bin/telemetry-controller
RUN chmod +x /usr/local/bin/telemetry-controller

ENTRYPOINT ["/usr/local/bin/telemetry-controller"]
CMD ["sweep", "--daemon", "--drop-dir=/drop", "--collector-endpoint=http://collector:4318/v1/traces"]

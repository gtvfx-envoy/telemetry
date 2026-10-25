# Minimal runtime image for the file-drop ingestion sweep.
#
# The `telemetry-controller` binary is pre-built for linux/amd64 by the
# publish pipeline (see ../../.envoy/publish-manifest.yaml) and copied in
# at build time from the bundle's `bin/linux-x86_64/` directory -- this
# Dockerfile does not compile Rust itself, matching how controller binaries
# are staged rather than vendored (see `.envoy/publish-manifest.yaml`).

FROM debian:bookworm-slim@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY telemetry-controller /usr/local/bin/telemetry-controller
RUN chmod +x /usr/local/bin/telemetry-controller

ENTRYPOINT ["/usr/local/bin/telemetry-controller"]
CMD ["sweep", "--daemon", "--drop-dir=/drop", "--collector-endpoint=http://collector:4318/v1/traces"]

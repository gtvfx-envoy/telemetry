#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd -- "$SCRIPT_DIR/.." && pwd)
CONTROLLER=${1:-"$REPO_ROOT/bin/linux-x86_64/telemetry-controller"}
STATE_ROOT=${2:-"$REPO_ROOT/bin/smoke-state"}
DROP_PATH=${3:-"$STATE_ROOT/drop"}
CONTROLLER=$(python3 -c 'import os, sys; print(os.path.abspath(sys.argv[1]))' "$CONTROLLER")
STATE_ROOT=$(python3 -c 'import os, sys; print(os.path.abspath(sys.argv[1]))' "$STATE_ROOT")
DROP_PATH=$(python3 -c 'import os, sys; print(os.path.abspath(sys.argv[1]))' "$DROP_PATH")
COMPOSE_FILE="$REPO_ROOT/compose/docker-compose.yml"
STATE_FILE="$STATE_ROOT/telemetry-server/state.json"
START_LOG="$STATE_ROOT/start.log"

export COMPOSE_PROJECT_NAME=${COMPOSE_PROJECT_NAME:-"telemetry-smoke-$$"}
export ENVOY_CONFIG_ROOT=$STATE_ROOT
export ENVOY_TELEMETRY_DROP_PATH=$DROP_PATH

if [[ ! -x "$CONTROLLER" ]]; then
    echo "Controller is missing or not executable: $CONTROLLER" >&2
    exit 1
fi
if [[ -e "$STATE_ROOT" ]]; then
    echo "Smoke-test state path already exists; choose a fresh path: $STATE_ROOT" >&2
    exit 1
fi

compose_label="label=com.docker.compose.project=$COMPOSE_PROJECT_NAME"
existing_containers=$(docker container ls --all --quiet --filter "$compose_label")
existing_volumes=$(docker volume ls --quiet --filter "$compose_label")
existing_networks=$(docker network ls --quiet --filter "$compose_label")
if [[ -n "$existing_containers" || -n "$existing_volumes" || -n "$existing_networks" ]]; then
    echo "Smoke-test Compose project already owns Docker resources: $COMPOSE_PROJECT_NAME" >&2
    echo "Choose a fresh COMPOSE_PROJECT_NAME; existing resources were left untouched." >&2
    exit 1
fi

mkdir -p "$STATE_ROOT" "$DROP_PATH"

readStateField() {
    python3 -c \
        'import json, sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' \
        "$STATE_FILE" "$1"
}

cleanup() {
    local exit_code=$?
    set +e

    if [[ $exit_code -ne 0 && -f "$STATE_FILE" ]]; then
        echo "Smoke test failed; recent Compose logs follow:" >&2
        COLLECTOR_BEARER_TOKEN=redacted \
        GRAFANA_ADMIN_PASSWORD=redacted \
        GRAFANA_VIEWER_PASSWORD=redacted \
        TEMPO_CONFIG_DIR="$STATE_ROOT/telemetry-server/runtime" \
            docker compose -f "$COMPOSE_FILE" logs --no-color --tail=100 >&2
    fi

    "$CONTROLLER" stop >/dev/null 2>&1
    COLLECTOR_BEARER_TOKEN=cleanup \
    GRAFANA_ADMIN_PASSWORD=cleanup \
    GRAFANA_VIEWER_PASSWORD=cleanup \
    TEMPO_CONFIG_DIR="$STATE_ROOT/telemetry-server/runtime" \
        docker compose -f "$COMPOSE_FILE" down --volumes --remove-orphans \
        >/dev/null 2>&1

    exit "$exit_code"
}
trap cleanup EXIT

if ! "$CONTROLLER" start --runtime compose >"$START_LOG" 2>&1; then
    sed -E 's/((password|token):).*/\1 REDACTED/I' "$START_LOG" >&2
    exit 1
fi

"$CONTROLLER" status | grep -F "Runtime:   compose (running)" >/dev/null

collector_token=$(readStateField collector_bearer_token)
viewer_password=$(readStateField grafana_viewer_password)
trace_id=$(python3 -c 'import secrets; print(secrets.token_hex(16))')
span_id=$(python3 -c 'import secrets; print(secrets.token_hex(8))')
now_ns=$(date +%s%N)
end_ns=$((now_ns + 1000000))
direct_payload="$STATE_ROOT/direct-otlp.json"

python3 - "$direct_payload" "$trace_id" "$span_id" "$now_ns" "$end_ns" <<'PY'
import json
import sys

path, trace_id, span_id, start_ns, end_ns = sys.argv[1:]
payload = {
    "resourceSpans": [{
        "resource": {"attributes": [{
            "key": "service.name",
            "value": {"stringValue": "envoy-release-smoke"},
        }]},
        "scopeSpans": [{
            "scope": {"name": "release-smoke"},
            "spans": [{
                "traceId": trace_id,
                "spanId": span_id,
                "name": "release-direct-smoke",
                "kind": 1,
                "startTimeUnixNano": start_ns,
                "endTimeUnixNano": end_ns,
            }],
        }],
    }],
}
with open(path, "w", encoding="utf-8") as handle:
    json.dump(payload, handle)
PY

unauthorized_code=$(curl -sS -o /dev/null -w '%{http_code}' \
    -X POST http://localhost:4318/v1/traces \
    -H 'Content-Type: application/json' \
    --data-binary "@$direct_payload")
[[ $unauthorized_code == 401 ]]

authorized_code=$(curl -sS -o /dev/null -w '%{http_code}' \
    -X POST http://localhost:4318/v1/traces \
    -H 'Content-Type: application/json' \
    -H "Authorization: Bearer $collector_token" \
    --data-binary "@$direct_payload")
[[ $authorized_code == 200 ]]

viewer_code=$(curl -sS -o /dev/null -w '%{http_code}' \
    -u "studio-viewer:$viewer_password" \
    http://localhost:3000/api/user)
[[ $viewer_code == 200 ]]

datasource_response="$STATE_ROOT/datasource.json"
curl -fsS -u "studio-viewer:$viewer_password" \
    http://localhost:3000/api/datasources/name/Tempo \
    -o "$datasource_response"
datasource_uid=$(python3 -c \
    'import json, sys; print(json.load(open(sys.argv[1]))["uid"])' \
    "$datasource_response")

trace_code=000
for _attempt in $(seq 1 30); do
    trace_code=$(curl -sS -o /dev/null -w '%{http_code}' \
        -u "studio-viewer:$viewer_password" \
        "http://localhost:3000/api/datasources/proxy/uid/$datasource_uid/api/traces/$trace_id")
    [[ $trace_code == 200 ]] && break
    sleep 2
done
[[ $trace_code == 200 ]]

drop_file="$DROP_PATH/release-smoke.json"
python3 - "$drop_file" <<'PY'
import json
import sys
import time

payload = {
    "name": "envoy.command.run",
    "attributes": {
        "envoy.command.name": {"Str": "release-file-drop-smoke"},
        "envoy.success": {"Bool": True},
    },
    "timestamp_unix_millis": int(time.time() * 1000),
}
with open(sys.argv[1], "w", encoding="utf-8") as handle:
    json.dump(payload, handle)
PY

for _attempt in $(seq 1 20); do
    [[ ! -e "$drop_file" ]] && break
    sleep 1
done
[[ ! -e "$drop_file" ]]

echo "Compose smoke test passed: auth, OTLP, Tempo query, and file-drop ingestion."

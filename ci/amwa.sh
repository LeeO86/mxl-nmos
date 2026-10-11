#!/usr/bin/env bash
# AMWA nmos-testing against the example node: IS-04-01, IS-05-01, IS-05-02, BCP-007-03-01.
#
#   ci/amwa.sh off <node-binary>   # DNS-SD off (default): node registers with an nmos-cpp registry
#   ci/amwa.sh on  <node-binary>   # DNS-SD on (feature dns-sd): the tool's mock registries over mDNS
#
# Needs: docker (mode off), python3, an nmos-testing checkout with its requirements in
# $NMOS_TESTING_DIR (default /tmp/nmos-testing) and $NMOS_TESTING_PYTHON (default
# /tmp/nmos-venv/bin/python), and the kernel TAI offset set (ci/set-tai-offset.py).
# A fresh node runs for each suite. Results go to $AMWA_OUT (default ci-artifacts/amwa-<mode>);
# ci/check_amwa.py then fails on any result not listed in ci/amwa-expected-<mode>.json.
set -euo pipefail

MODE="${1:?usage: amwa.sh off|on <node-binary>}"
NODE_BIN="${2:?usage: amwa.sh off|on <node-binary>}"
HERE="$(cd "$(dirname "$0")" && pwd)"
TOOL="${NMOS_TESTING_DIR:-/tmp/nmos-testing}"
PY="${NMOS_TESTING_PYTHON:-/tmp/nmos-venv/bin/python}"
OUT="${AMWA_OUT:-$HERE/../ci-artifacts/amwa-$MODE}"
REGISTRY_IMAGE="${NMOS_REGISTRY_IMAGE:-mirror.gcr.io/rhastie/nmos-cpp@sha256:c54c8ded3b61740dbd579c03892b0229842250417aa397d87a92e2b91ba645b8}"
NODE_PORT=3312
REG_PORT=8010
QUERY_PORT=8011
HOST_IP="${HOST_IP:-$(ip -4 route get 1.1.1.1 | sed -n 's/.* src \([0-9.]*\).*/\1/p')}"
mkdir -p "$OUT"
NODE_PID=""

log() { echo "[amwa-$MODE] $*"; }

wait_http() { # <url> <seconds>
    for _ in $(seq 1 $(($2 * 5))); do
        curl -fsS -o /dev/null "$1" 2>/dev/null && return 0
        sleep 0.2
    done
    log "timeout waiting for $1"
    return 1
}

stop_node() {
    if [[ -n "$NODE_PID" ]] && kill -0 "$NODE_PID" 2>/dev/null; then
        kill -TERM "$NODE_PID"
        wait "$NODE_PID" || true
    fi
    NODE_PID=""
}

cleanup() {
    stop_node
    if [[ "$MODE" == off ]]; then
        docker logs mxl-nmos-amwa-registry >"$OUT/registry.log" 2>&1 || true
        docker rm -f mxl-nmos-amwa-registry >/dev/null 2>&1 || true
    fi
}
trap cleanup EXIT

start_node() { # <suite>
    local env=(NMOS_SEED=amwa-ci NMOS_HOST_ADDRESS="$HOST_IP" NMOS_PORT="$NODE_PORT" RUST_LOG=info)
    if [[ "$MODE" == off ]]; then
        env+=(NMOS_REGISTRY_ADDRESS="$HOST_IP" NMOS_REGISTRY_PORT="$REG_PORT")
    else
        env+=(NMOS_DNS_SD=true)
    fi
    env "${env[@]}" "$NODE_BIN" >"$OUT/node-$1.log" 2>&1 &
    NODE_PID=$!
    wait_http "http://$HOST_IP:$NODE_PORT/x-nmos/node/v1.3/self" 30
}

if [[ "$MODE" == off ]]; then
    docker rm -f mxl-nmos-amwa-registry >/dev/null 2>&1 || true
    docker run -d --name mxl-nmos-amwa-registry --network host \
        -v "$HERE/registry.json:/home/registry.json:ro" "$REGISTRY_IMAGE" >/dev/null
    wait_http "http://$HOST_IP:$QUERY_PORT/x-nmos/query/v1.3/nodes" 60
    cat >"$TOOL/nmostesting/UserConfig.py" <<EOF
from . import Config as CONFIG
CONFIG.ENABLE_DNS_SD = False
CONFIG.QUERY_API_HOST = "$HOST_IP"
CONFIG.QUERY_API_PORT = $QUERY_PORT
EOF
else
    cat >"$TOOL/nmostesting/UserConfig.py" <<EOF
from . import Config as CONFIG
CONFIG.ENABLE_DNS_SD = True
CONFIG.DNS_SD_MODE = "multicast"
CONFIG.DNS_SD_ADVERT_TIMEOUT = 60
EOF
fi

declare -A ARGS=(
    [IS-04-01]="--host $HOST_IP --port $NODE_PORT --version v1.3"
    [IS-05-01]="--host $HOST_IP --port $NODE_PORT --version v1.2"
    [IS-05-02]="--host $HOST_IP $HOST_IP --port $NODE_PORT $NODE_PORT --version v1.3 v1.2"
    [BCP-007-03-01]="--host $HOST_IP $HOST_IP --port $NODE_PORT $NODE_PORT --version v1.3 v1.2"
)
for suite in IS-04-01 IS-05-01 IS-05-02 BCP-007-03-01; do
    start_node "$suite"
    log "running $suite"
    set +e
    # shellcheck disable=SC2086
    (cd "$TOOL" && "$PY" nmos-test.py suite "$suite" --selection all ${ARGS[$suite]} \
        --output "$OUT/$suite.json") >"$OUT/$suite.log" 2>&1
    rc=$?
    set -e
    log "$suite exited $rc"
    stop_node
done

if [[ "$MODE" == off ]]; then
    # The last node stopped with SIGTERM: it and its devices must be gone from the registry.
    node_id=$(python3 -c "import uuid; print(uuid.uuid5(uuid.UUID('8c76aff5-a8f9-53ce-97d0-766f1074b699'), 'amwa-ci'))")
    code=$(curl -s -o /dev/null -w '%{http_code}' "http://$HOST_IP:$QUERY_PORT/x-nmos/query/v1.3/nodes/$node_id")
    devices=$(curl -fsS "http://$HOST_IP:$QUERY_PORT/x-nmos/query/v1.3/devices" |
        python3 -c "import json,sys; print(sum(d['node_id'] == sys.argv[1] for d in json.load(sys.stdin)))" "$node_id")
    log "after SIGTERM: Query API node $node_id -> HTTP $code, devices left: $devices"
    [[ "$code" == 404 && "$devices" == 0 ]] || { log "FAIL: the node did not deregister"; exit 1; }
fi

python3 "$HERE/check_amwa.py" "$OUT" "$HERE/amwa-expected-$MODE.json"

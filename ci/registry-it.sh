#!/usr/bin/env bash
# Integration test against an nmos-cpp registry container (tests/nmos_cpp_registry.rs).
#
#   ci/registry-it.sh
#
# Starts the registry (Registration API 8010, Query API 8011) on the host network,
# runs the test, removes the container.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
IMAGE="${NMOS_REGISTRY_IMAGE:-mirror.gcr.io/rhastie/nmos-cpp@sha256:c54c8ded3b61740dbd579c03892b0229842250417aa397d87a92e2b91ba645b8}"
NAME=mxl-nmos-it-registry
HOST_IP="${HOST_IP:-$(ip -4 route get 1.1.1.1 | sed -n 's/.* src \([0-9.]*\).*/\1/p')}"

docker rm -f "$NAME" >/dev/null 2>&1 || true
docker run -d --name "$NAME" --network host -v "$HERE/registry.json:/home/registry.json:ro" "$IMAGE" >/dev/null
trap 'docker logs "$NAME" > "${IT_ARTIFACTS:-/tmp}/registry-it.log" 2>&1 || true; docker rm -f "$NAME" >/dev/null 2>&1 || true' EXIT
for _ in $(seq 1 300); do
    curl -fsS -o /dev/null "http://$HOST_IP:8011/x-nmos/query/v1.3/nodes" 2>/dev/null && break
    sleep 0.2
done
MXL_NMOS_IT_REGISTRY="$HOST_IP:8010" MXL_NMOS_IT_CONTAINER="$NAME" MXL_NMOS_IT_HOST="$HOST_IP" \
    cargo test --test nmos_cpp_registry -- --nocapture

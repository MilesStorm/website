#!/usr/bin/env bash
# Runs the built image for the end-to-end tests: Redis, SurrealDB (in memory, with the
# database-level `dice` login the website has in production), the stand-ins for auth and Ark
# (fake-upstreams.mjs), the server with FRONTEND_UI=astro, and Caddy in front of it with HTTPS, as in production (a release
# build takes a browser's changes only from an https:// page of its own host), all on the host's
# network. The tests use https://localhost:$TLS_PORT. `e2e/serve.sh stop` removes them.
# IMAGE (default frontend:e2e), E2E_PORT (8090), TLS_PORT (8443), FAKE_PORT (4010),
# REDIS_PORT (6390), SURREAL_PORT (8091).
set -euo pipefail
cd "$(dirname "$0")"
IMAGE=${IMAGE:-frontend:e2e}
E2E_PORT=${E2E_PORT:-8090}
TLS_PORT=${TLS_PORT:-8443}
FAKE_PORT=${FAKE_PORT:-4010}
REDIS_PORT=${REDIS_PORT:-6390}
SURREAL_PORT=${SURREAL_PORT:-8091}

stop() {
  docker rm -f frontend-e2e frontend-e2e-redis frontend-e2e-tls frontend-e2e-surreal >/dev/null 2>&1 || true
  if [[ -f .fake.pid ]]; then kill "$(cat .fake.pid)" 2>/dev/null || true; rm -f .fake.pid; fi
}
if [[ ${1:-} == stop ]]; then stop; exit 0; fi
stop

docker run -d --rm --name frontend-e2e-redis --network host redis:7-alpine --port "$REDIS_PORT" >/dev/null
# The version the cluster runs (surreal/single-node-values.yaml in the homelab repo).
docker run -d --rm --name frontend-e2e-surreal --network host surrealdb/surrealdb:v3.2.4 \
  start --bind "127.0.0.1:$SURREAL_PORT" --user root --pass e2e-root memory >/dev/null
for _ in $(seq 30); do
  curl -fsS -o /dev/null "http://127.0.0.1:$SURREAL_PORT/health" 2>/dev/null && break
  sleep 1
done
curl -fsS -u root:e2e-root -H 'Accept: application/json' "http://127.0.0.1:$SURREAL_PORT/sql" \
  -d "DEFINE NAMESPACE milesstorm; USE NS milesstorm; DEFINE DATABASE arcane; USE DB arcane;
      DEFINE USER dice ON DATABASE PASSWORD 'e2e-only' ROLES EDITOR;" >/dev/null
FAKE_PORT=$FAKE_PORT SITE_URL="https://localhost:$TLS_PORT" node fake-upstreams.mjs > fake.log 2>&1 &
echo $! > .fake.pid
docker run -d --rm --name frontend-e2e --network host \
  -e IP=127.0.0.1 -e PORT="$E2E_PORT" -e FRONTEND_UI=astro -e GIT_SHA=e2e0000 \
  -e REDIS_HOST=127.0.0.1 -e REDIS_PORT="$REDIS_PORT" \
  -e AUTH_SERVICE_URL="http://127.0.0.1:$FAKE_PORT" -e PUBLIC_AUTH_URL="http://127.0.0.1:$FAKE_PORT" \
  -e BFF_SERVICE_SECRET=e2e-only \
  -e SURREAL_URL="http://127.0.0.1:$SURREAL_PORT" -e SURREAL_USER=dice -e SURREAL_PASS=e2e-only \
  "$IMAGE" >/dev/null
# Caddy's own CA signs the certificate; the browser is told to accept it (playwright.config.ts).
docker run -d --rm --name frontend-e2e-tls --network host caddy:2.10.2-alpine \
  caddy reverse-proxy --from "https://localhost:$TLS_PORT" --to "127.0.0.1:$E2E_PORT" --internal-certs --disable-redirects \
  >/dev/null

for _ in $(seq 60); do
  # Up, and the roll-sharing schema applied (pictures and sharing are off until then).
  if curl -fsSk -o /dev/null "https://localhost:$TLS_PORT/api/session" 2>/dev/null &&
    docker logs frontend-e2e 2>&1 | grep -q 'roll-sharing schema applied'; then
    echo "serving on https://localhost:$TLS_PORT"
    exit 0
  fi
  sleep 1
done
docker logs frontend-e2e
docker logs frontend-e2e-tls
docker logs frontend-e2e-surreal
exit 1

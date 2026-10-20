#!/usr/bin/env bash
# Runs the built image for the end-to-end tests: Redis, the stand-ins (fake-upstreams.mjs), the
# server with FRONTEND_UI=astro, and Caddy in front of it with HTTPS, as in production (a release
# build takes a browser's changes only from an https:// page of its own host), all on the host's
# network. The tests use https://localhost:$TLS_PORT. `e2e/serve.sh stop` removes them.
# IMAGE (default frontend:e2e), E2E_PORT (8090), TLS_PORT (8443), FAKE_PORT (4010),
# REDIS_PORT (6390).
set -euo pipefail
cd "$(dirname "$0")"
IMAGE=${IMAGE:-frontend:e2e}
E2E_PORT=${E2E_PORT:-8090}
TLS_PORT=${TLS_PORT:-8443}
FAKE_PORT=${FAKE_PORT:-4010}
REDIS_PORT=${REDIS_PORT:-6390}

stop() {
  docker rm -f frontend-e2e frontend-e2e-redis frontend-e2e-tls >/dev/null 2>&1 || true
  if [[ -f .fake.pid ]]; then kill "$(cat .fake.pid)" 2>/dev/null || true; rm -f .fake.pid; fi
  rm -rf .status
}
if [[ ${1:-} == stop ]]; then stop; exit 0; fi
stop

docker run -d --rm --name frontend-e2e-redis --network host redis:7-alpine --port "$REDIS_PORT" >/dev/null
FAKE_PORT=$FAKE_PORT SITE_URL="https://localhost:$TLS_PORT" node fake-upstreams.mjs > fake.log 2>&1 &
echo $! > .fake.pid
docker run -d --rm --name frontend-e2e --network host \
  -e IP=127.0.0.1 -e PORT="$E2E_PORT" -e FRONTEND_UI=astro -e GIT_SHA=e2e0000 \
  -e REDIS_HOST=127.0.0.1 -e REDIS_PORT="$REDIS_PORT" \
  -e AUTH_SERVICE_URL="http://127.0.0.1:$FAKE_PORT" -e PUBLIC_AUTH_URL="http://127.0.0.1:$FAKE_PORT" \
  -e BFF_SERVICE_SECRET=e2e-only \
  -e SURREAL_URL="http://127.0.0.1:$FAKE_PORT" -e SURREAL_USER=e2e -e SURREAL_PASS=e2e-only \
  "$IMAGE" >/dev/null
# Caddy's own CA signs the certificate; the browser is told to accept it (playwright.config.ts).
docker run -d --rm --name frontend-e2e-tls --network host caddy:2.10.2-alpine \
  caddy reverse-proxy --from "https://localhost:$TLS_PORT" --to "127.0.0.1:$E2E_PORT" --internal-certs --disable-redirects \
  >/dev/null

for _ in $(seq 60); do
  if curl -fsSk -o /dev/null "https://localhost:$TLS_PORT/api/session" 2>/dev/null; then
    # The status pages as built, for pages.spec.ts (no page in PAGES is guarded yet).
    rm -rf .status && mkdir .status
    docker cp frontend-e2e:/app/web/403.html .status/403.html >/dev/null
    docker cp frontend-e2e:/app/web/503.html .status/503.html >/dev/null
    echo "serving on https://localhost:$TLS_PORT"
    exit 0
  fi
  sleep 1
done
docker logs frontend-e2e
exit 1

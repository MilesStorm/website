#!/usr/bin/env bash
# Runs the built image for the end-to-end tests: Redis, the stand-ins (fake-upstreams.mjs) and
# the server with FRONTEND_UI=astro, all on the host's network. `e2e/serve.sh stop` removes them.
# IMAGE (default frontend:e2e), E2E_PORT (8090), FAKE_PORT (4010), REDIS_PORT (6390).
set -euo pipefail
cd "$(dirname "$0")"
IMAGE=${IMAGE:-frontend:e2e}
E2E_PORT=${E2E_PORT:-8090}
FAKE_PORT=${FAKE_PORT:-4010}
REDIS_PORT=${REDIS_PORT:-6390}

stop() {
  docker rm -f frontend-e2e frontend-e2e-redis >/dev/null 2>&1 || true
  if [[ -f .fake.pid ]]; then kill "$(cat .fake.pid)" 2>/dev/null || true; rm -f .fake.pid; fi
  rm -rf .status
}
if [[ ${1:-} == stop ]]; then stop; exit 0; fi
stop

docker run -d --rm --name frontend-e2e-redis --network host redis:7-alpine --port "$REDIS_PORT" >/dev/null
FAKE_PORT=$FAKE_PORT SITE_URL="http://localhost:$E2E_PORT" node fake-upstreams.mjs > fake.log 2>&1 &
echo $! > .fake.pid
docker run -d --rm --name frontend-e2e --network host \
  -e IP=127.0.0.1 -e PORT="$E2E_PORT" -e FRONTEND_UI=astro -e GIT_SHA=e2e0000 \
  -e REDIS_HOST=127.0.0.1 -e REDIS_PORT="$REDIS_PORT" \
  -e AUTH_SERVICE_URL="http://127.0.0.1:$FAKE_PORT" -e PUBLIC_AUTH_URL="http://127.0.0.1:$FAKE_PORT" \
  -e BFF_SERVICE_SECRET=e2e-only \
  -e SURREAL_URL="http://127.0.0.1:$FAKE_PORT" -e SURREAL_USER=e2e -e SURREAL_PASS=e2e-only \
  "$IMAGE" >/dev/null

for _ in $(seq 60); do
  if curl -fsS -o /dev/null "http://localhost:$E2E_PORT/api/session"; then
    # The status pages as built, for pages.spec.ts (no page in PAGES is guarded yet).
    rm -rf .status && mkdir .status
    docker cp frontend-e2e:/app/web/403.html .status/403.html >/dev/null
    docker cp frontend-e2e:/app/web/503.html .status/503.html >/dev/null
    echo "serving on http://localhost:$E2E_PORT"
    exit 0
  fi
  sleep 1
done
docker logs frontend-e2e
exit 1

# Environment Variables

## Auth service (`services/auth`)

### Required — service panics on startup if missing

| Variable | Description |
|---|---|
| `DATABASE_URL` | PostgreSQL connection string, e.g. `postgres://user:pass@host/db` |
| `CLIENT_ID` | GitHub OAuth app client ID |
| `CLIENT_SECRET` | GitHub OAuth app client secret |
| `G_CLIENT_ID` | Google OAuth app client ID |
| `G_CLIENT_SECRET` | Google OAuth app client secret |
| `JWT_SECRET` | Secret used to sign JWTs returned from `/internal/token/introspect`. Must be kept private and stable — rotating it invalidates all live sessions. |
| `BFF_SERVICE_SECRET` | Shared secret between auth and frontend. Auth uses it to gate all `/internal/*` endpoints. **Must match `BFF_SERVICE_SECRET` in the frontend.** |

### Optional — have sane defaults

| Variable | Default | Description |
|---|---|---|
| `BFF_CALLBACK_URL` | `http://localhost:8080` | Public URL of the frontend. After a successful OAuth login, auth redirects the browser here (`/oauth/callback?code=...`). In production set this to `https://milesstorm.com`. |
| `SERVER_IP` | `localhost` | Bind address. Set to `0.0.0.0` in the K8s deployment. |
| `SERVER_PORT` | `7070` | Bind port. |
| `RUST_LOG` | `info,sqlx=warn,tower_sessions=warn` | Log filter string passed to `tracing-subscriber`. |

---

## Frontend service (`services/frontend`)

### Required — service panics on startup if missing

| Variable | Description |
|---|---|
| `BFF_SERVICE_SECRET` | Shared secret sent as `x-service-token` on every call to `/internal/*` on the auth service. **Must match `BFF_SERVICE_SECRET` in auth.** |

### Optional — have sane defaults

| Variable | Default | Description |
|---|---|---|
| `AUTH_SERVICE_URL` | `http://localhost:7070` | Internal URL of the auth service used for server-to-server calls. In the cluster set to `http://auth-service.auth.svc.cluster.local`. |
| `PUBLIC_AUTH_URL` | `http://localhost:7070` | Public base URL of the auth service used for browser-facing OAuth redirects. In production set to `https://milesstorm.com` (Istio routes `/api/*` to auth). |
| `IP` | `127.0.0.1` | Bind address for the Dioxus fullstack server. Set to `0.0.0.0` in the Dockerfile so the istio sidecar (and any in-cluster traffic) can reach the pod. |
| `PORT` | `8080` | Port the Dioxus fullstack server binds to. Set to `80` in the Dockerfile so K8s manifests don't need changing. |
| `RUST_LOG` | `info,dioxus=warn,tower_sessions=warn` | Log filter string. |

---

## Notes

- `BFF_SERVICE_SECRET` appears in **both** services and must be the **same value**. It is the shared secret for the internal service-to-service channel between frontend and auth. Generate with e.g. `openssl rand -hex 32`.
- `JWT_SECRET` is auth-only. The frontend never sees JWTs directly — it holds opaque BFF tokens and the auth service issues JWTs only for internal introspection.

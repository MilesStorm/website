# Frontend

The website: Astro pages with React islands, served by a Rust (axum) server that is also the
backend for frontend (BFF). One image, `ghcr.io/milesstorm/frontend`.

```
services/frontend/
├─ Cargo.toml        # workspace: crates/*
├─ Dockerfile        # web ∥ bff → verify → runtime (no Node at run time)
├─ crates/
│  ├─ bff/           # package `bff`, binary `server`: the pages (spliced with the viewer and the
│  │                 #   trace), the JSON API under /api/*, OAuth, the camera WebSocket, rolls
│  └─ api-types/     # the API's types and the PAGES table, the source of the OpenAPI document
├─ web/              # the Astro app: src/pages, src/islands, src/components/ui (shadcn),
│                    #   assets/trace.js (browser tracing), src/api (generated client types)
└─ surreal/          # the SurrealDB schema the server applies at startup
```

- `server` serves the site. `server openapi` prints the API's OpenAPI document (committed as
  `web/src/api/openapi.json`; `npm run api` in `web/` regenerates it and the TypeScript types).
  `server check-pages <dir>` checks an Astro build has exactly the pages in `PAGES`.
- Running it locally: the root [README](../../README.MD#run-the-web-application-locally).
  Configuration: [ENV.md](../../ENV.md). Tracing: [TRACING.md](../../TRACING.md).

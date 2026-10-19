# Arcane message contract

What ai_pipeline sends over the camera WebSocket and the website passes on unchanged to the
Arcane page (`/ws/arcane`) and the browser extension (`/api/arcane/rolls`), one file per kind
of message. Two tests read these files:

- `services/ai_pipeline/src/serve.rs` (`messages_are_the_contract_fixtures`): ai_pipeline
  still writes exactly these messages.
- `services/frontend/crates/api-types` (`arcane_fixtures_are_the_documented_messages`): each
  one reads into `ArcaneServerMsg`, whose fields deny anything unknown, so the OpenAPI
  document (and the TypeScript types made from it) describes them.

A change to a message changes a fixture, and both tests then say whether the other side
agrees.

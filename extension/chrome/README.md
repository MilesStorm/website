# Arcane Dice (Chrome extension)

A side panel that shows your latest dice roll from the Arcane dice tray, live.

The camera runs on the website's Arcane page (`/arcane`) on any device. This extension
only displays results, for whoever is logged in to milesstorm.com in this Chrome.

```
camera page ──ws──▶ website (/ws/arcane) ──▶ ai_pipeline (finds dice, reads them, settles a roll)
                        │  roll event
                        ▼
                     Redis  arcane:rolls:<user>  +  arcane:last_roll:<user> (1 h)
                        │
side panel ◀──SSE── website (/api/arcane/rolls)
```

## Install (unpacked)
1. Open `chrome://extensions`, turn on **Developer mode**.
2. **Load unpacked** → pick this `extension/chrome/` folder.
3. Click the extension's toolbar button to open the side panel.
4. Log in on milesstorm.com in a normal tab if the panel asks. The account needs the
   `arcane` permission.

## What it shows
- Each die, left to right, and the total.
- A die the model isn't sure about shows as **?** and there's no total. The server
  never guesses. Nudge the die or fix the camera, and the roll updates.
- Opening the panel shows the last roll from the past hour straight away.

## Settings
Right-click the toolbar button → **Options** to switch between milesstorm.com and a local
dev server (`http://localhost:8080`). Chrome asks for permission the first time the dev
server is chosen; it's an optional permission, so normal installs never talk to
localhost. Only the sites in `lib/config.js` can be chosen, and a test keeps that list in
sync with `manifest.json`.

## How login works
There is no separate login. The panel calls the website with `credentials: "include"`,
so Chrome sends the site's normal session cookie. That works because the site is in
`host_permissions`. The extension never sees a password or token.

## Website endpoints used
| Endpoint | Reply |
|---|---|
| `GET /api/arcane/me` | Always 200: `{"logged_in":bool,"username":..,"has_arcane":bool}` |
| `GET /api/arcane/rolls` | SSE stream of `event: roll` messages; 401 when logged out, 403 without the permission |

The roll JSON is ai_pipeline's `RollEvent` (`services/ai_pipeline/src/roll.rs`).

## Later: typing rolls into another website
The latest roll is mirrored into `chrome.storage.session` under `lastRoll`. Only the
extension's own pages can read that storage; content scripts can't. A future content script
that fills the target site's text box should ask the service worker with
`chrome.runtime.sendMessage`. The worker answers from storage after checking
`sender.tab.url`, so a compromised web page can't read or plant rolls. The target site will
also need adding to the manifest.

## Tests
```
node --test test/
```
Covers the stream parser (chunks split mid-line, CRLF, multi-line data, keep-alive
comments), roll parsing and display, and the settings list.

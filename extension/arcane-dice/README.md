# Arcane Dice (Firefox and Chrome extension)

A toolbar button that shows your latest dice roll from the Arcane dice tray. It updates
live while the popup is open.

The camera runs on the website's Arcane page (`/arcane`), on any device. This extension
only shows results, for whoever is logged in to milesstorm.com in this browser.

```
camera page ──ws──▶ website (/ws/arcane) ──▶ ai_pipeline (finds dice, reads them, settles a roll)
                        │  roll event
                        ▼
                     Redis  arcane:rolls:<user>  +  arcane:last_roll:<user> (1 h)
                        │
popup ◀──SSE──── website (/api/arcane/rolls)
```

The same folder loads in both browsers; nothing needs building.

## Install
**Firefox (140 or newer):**
1. Open `about:debugging#/runtime/this-firefox`.
2. Click **Load Temporary Add-on…** and pick this folder's `manifest.json`.
3. Temporary add-ons are removed when Firefox restarts. To keep it installed, sign it
   on addons.mozilla.org as a self-distributed (unlisted) add-on, e.g. with
   `npx web-ext sign --channel unlisted`.

**Chrome:**
1. Open `chrome://extensions` and turn on **Developer mode**.
2. Click **Load unpacked** and pick this folder.

Then click the toolbar button (pin it if it's hidden in the extensions menu). If the
popup asks, log in on milesstorm.com in a normal tab. The account needs the `arcane`
permission. In Firefox, if the popup says it needs permission to reach the site, click
**Allow access**. Firefox lets users switch site access off per extension.

## What it shows
- Each die, left to right, and the total.
- A die the model isn't sure about shows as **?**, with no total. The server never
  guesses. Nudge the die or adjust the camera, and the roll updates.
- Opening the popup shows the last roll from the past hour straight away.

## Site
The extension only talks to `https://milesstorm.com` (`lib/config.js`). It never uses
plain http, and a test checks that no plain-http URL is in the shipped files. Testing
against a local server is done with a throwaway copy of the extension, never by
changing this one.

## How login works
There is no separate login. The popup calls the website with `credentials: "include"`,
so the browser sends the site's normal session cookie, because the extension has host
permission for the site. The extension never sees a password or token.

## Website endpoints used
| Endpoint | Reply |
|---|---|
| `GET /api/arcane/me` | Always 200: `{"logged_in":bool,"username":..,"has_arcane":bool}` |
| `GET /api/arcane/rolls` | SSE stream of `event: roll` messages; 401 when logged out, 403 without the permission, 429 with more than 8 open |

The roll JSON is ai_pipeline's `RollEvent` (`services/ai_pipeline/src/roll.rs`).

## Later: typing rolls into another website
The popup only runs while it's open, so auto-typing will need a background script
holding the stream. The popup already mirrors the latest roll into
`chrome.storage.session` (key `lastRoll`), which only the extension's own pages can read.
A content script for the target site should get the roll by messaging the background
script (`chrome.runtime.sendMessage`), which checks `sender.tab.url`. That way a
compromised web page can't read or plant rolls.

## Tests
```
node --test test/
```
Covers the stream parser (chunks split mid-line, CRLF, multi-line data, keep-alive
comments), roll parsing and display, and the settings list.

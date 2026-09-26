# Arcane Dice (Firefox and Chrome extension)

A toolbar button that shows your latest dice roll from the Arcane dice tray, and types
each roll into the chat of an open Roll20 game, as `[[13 + 3]]`. Roll20 turns that into
an inline roll showing the total (16), like a roll made in the game.

The camera runs on the website's Arcane page (`/arcane`), on any device. This extension
only shows and passes on results, for whoever is logged in to milesstorm.com in this
browser.

```
camera page ──ws──▶ website (/ws/arcane) ──▶ ai_pipeline (finds dice, reads them, settles a roll)
                        │  roll event
                        ▼
                     Redis  arcane:rolls:<user>  +  arcane:last_roll:<user> (1 h)
                        │
popup ◀──SSE──── website (/api/arcane/rolls) ──SSE──▶ background.js ──▶ roll20.js ──▶ Roll20 chat
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

Reload any Roll20 game tab that was already open when the extension was installed or
updated.

## What it shows
- Each die, left to right, and the total.
- A die the model isn't sure about shows as **?**, with no total. The server never
  guesses. Nudge the die or adjust the camera, and the roll updates.
- Opening the popup shows the last roll from the past hour straight away.
- **Send rolls to Roll20 chat** (on by default). If Firefox hasn't given the extension
  access to Roll20, an **Allow access to Roll20** button appears under it.

## Roll20 chat
- While a Roll20 game tab (`app.roll20.net/editor`) is open and the switch is on, the
  background script holds the roll stream. With no game tab open it holds nothing.
- A roll is sent once every die is read and the reading has held for 1 second, because
  the dice reader sometimes corrects itself a moment after a roll. Each roll is sent
  once; a correction after that is ignored (flag the roll instead).
- A roll with an unreadable die isn't sent. A note in the game page says to nudge the
  die, and the roll is sent once it can be read.
- A d10's `0` counts as 10, like the website's total.
- The roll the website replays on connect (your last roll, sent as a `replay` event) is
  never posted, so opening a game doesn't post an old roll.
- With several game tabs open, the one used most recently gets the rolls.
- The page script (`roll20.js`) types into `#textchat-input textarea` and clicks
  `#chatSendBtn`, then puts back anything you had half-typed. It only ever types text of
  the form `[[n + n + …]]`: a test keeps its pattern equal to `lib/chat.js`.
- Short notes (connected, logged out, can't reach the site, unreadable die) appear in the
  top-right corner of the game page for a few seconds.

## Sites
The extension only talks to `https://milesstorm.com` and runs its page script only on
`https://app.roll20.net` (`lib/config.js`). It never uses
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
| `GET /api/arcane/rolls` | SSE stream: first `event: replay` with the last roll from the past hour (if any), then `event: roll` for each new roll; 401 when logged out, 403 without the permission, 429 with more than 8 open |

The roll JSON is ai_pipeline's `RollEvent` (`services/ai_pipeline/src/roll.rs`).

## Tests
```
node --test test/
```
Covers the stream parser (chunks split mid-line, CRLF, multi-line data, keep-alive
comments), roll parsing and display, the replay flag, the chat text, when rolls are sent
(delay, corrections, replays, unreadable dice), and the site list.

The Roll20 part has also been run end to end in Firefox with a throwaway copy pointed at
a fake site and a fake game page. It has not yet been tried in Chrome.

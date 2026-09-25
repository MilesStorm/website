// Side panel: shows the logged-in user's latest dice roll, live.
//
// Login is the website's own session cookie (milesstorm.bff). Chrome sends it on
// these fetches because the site is in host_permissions; the extension never sees
// or stores a password or token. The fetches live here, not in the service worker,
// because service-worker fetches may be sent without the cookie.

import { SseParser } from "./lib/sse.js";
import { parseRoll, describeRoll, timeAgo } from "./lib/roll.js";
import { DEFAULT_BASE, normalizeBase } from "./lib/config.js";

const $ = (id) => document.getElementById(id);

let base = DEFAULT_BASE;
let controller = new AbortController();
let shown = null;

/** Resolves after `ms`, or as soon as `signal` aborts. */
function sleep(ms, signal) {
  return new Promise((resolve) => {
    const t = setTimeout(resolve, ms);
    signal.addEventListener("abort", () => (clearTimeout(t), resolve()), { once: true });
  });
}

function setStatus(kind, text, { login = false } = {}) {
  const el = $("status");
  el.dataset.kind = kind;
  el.textContent = text;
  $("login").hidden = !login;
}

function render(roll) {
  shown = roll;
  $("empty").hidden = roll !== null;
  $("roll").hidden = roll === null;
  if (!roll) return;

  const { dice, total, unreadable } = describeRoll(roll);
  const list = $("dice");
  list.replaceChildren(
    ...dice.map((d) => {
      const li = document.createElement("li");
      li.textContent = d.label;
      if (!d.readable) li.className = "unreadable";
      return li;
    }),
  );
  // A single readable die is its own total; don't repeat it.
  $("total").textContent = total !== null && dice.length > 1 ? `= ${total}` : "";
  const warn = $("unreadable");
  warn.hidden = unreadable === 0;
  warn.textContent =
    unreadable === 1
      ? "1 die couldn't be read. Nudge it or adjust the camera."
      : `${unreadable} dice couldn't be read. Nudge them or adjust the camera.`;
  $("when").textContent = `(${timeAgo(roll.ts)})`;
}

async function getMe(signal) {
  const resp = await fetch(`${base}/api/arcane/me`, { credentials: "include", cache: "no-store", signal });
  if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
  return resp.json();
}

/** Streams rolls until the connection ends. Returns normally on 401/403 so the caller re-checks login. */
async function streamRolls(signal) {
  const resp = await fetch(`${base}/api/arcane/rolls`, {
    credentials: "include",
    cache: "no-store",
    headers: { Accept: "text/event-stream" },
    signal,
  });
  if (resp.status === 401 || resp.status === 403) return;
  if (!resp.ok || !resp.body) throw new Error(`HTTP ${resp.status}`);

  const reader = resp.body.pipeThrough(new TextDecoderStream()).getReader();
  const parser = new SseParser();
  for (;;) {
    const { value, done } = await reader.read();
    if (done) return;
    for (const ev of parser.push(value)) {
      if (ev.event !== "roll") continue;
      const roll = parseRoll(ev.data);
      if (!roll) continue;
      render(roll);
      chrome.storage.session.set({ lastRoll: roll }).catch(console.error);
    }
  }
}

async function run() {
  let backoff = 1000;
  for (;;) {
    const signal = controller.signal;
    try {
      const me = await getMe(signal);
      if (!me.logged_in) {
        setStatus("out", "You're not logged in.", { login: true });
        await sleep(5000, signal); // notice a login in another tab
        continue;
      }
      if (!me.has_arcane) {
        setStatus("denied", `Logged in as ${me.username}, but this account doesn't have dice access.`);
        await sleep(30000, signal);
        continue;
      }
      setStatus("live", `Live, as ${me.username}`);
      if (!shown) render(null);
      await streamRolls(signal);
      backoff = 1000;
    } catch (e) {
      if (signal.aborted) continue; // the site setting changed; start over at once
      console.warn("arcane dice:", e);
    }
    if (signal.aborted) continue;
    setStatus("retry", `Can't reach ${new URL(base).host}. Retrying…`);
    await sleep(backoff, signal);
    backoff = Math.min(backoff * 2, 30000);
  }
}

async function loadBase() {
  const { base: saved } = await chrome.storage.sync.get("base");
  return normalizeBase(saved);
}

chrome.storage.onChanged.addListener((changes, area) => {
  if (area !== "sync" || !("base" in changes)) return;
  base = normalizeBase(changes.base.newValue);
  shown = null;
  $("roll").hidden = true;
  setStatus("connecting", "Connecting…");
  controller.abort();
  controller = new AbortController();
});

$("login").addEventListener("click", () => chrome.tabs.create({ url: `${base}/login` }));

// Keep "12 s ago" current.
setInterval(() => {
  if (shown) $("when").textContent = `(${timeAgo(shown.ts)})`;
}, 1000);

(async () => {
  base = await loadBase();
  const { lastRoll } = await chrome.storage.session.get("lastRoll");
  if (lastRoll) render(lastRoll);
  run();
})();

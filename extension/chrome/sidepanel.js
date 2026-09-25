// Side panel: shows the logged-in user's latest dice roll, live.
//
// Login is the website's own session cookie (milesstorm.bff). Chrome sends it on
// these fetches because the site is in host_permissions; the extension never sees
// or stores a password or token. The fetches live here, not in the service worker,
// because service-worker fetches may be sent without the cookie.

import { SseParser } from "./lib/sse.js";
import { parseRoll, describeRoll, timeAgo } from "./lib/roll.js";
import { DEFAULT_BASE, isOptional, normalizeBase, originPattern } from "./lib/config.js";

const $ = (id) => document.getElementById(id);

let base = DEFAULT_BASE;
let controller = new AbortController();
let shown = null;

/** The server sends a keep-alive every 20 s; this long without a byte means the link is dead. */
const STALL_MS = 50_000;

/** Resolves after `ms`, or as soon as `signal` aborts. */
function sleep(ms, signal) {
  return new Promise((resolve) => {
    const onAbort = () => (clearTimeout(t), resolve());
    const t = setTimeout(() => (signal.removeEventListener("abort", onAbort), resolve()), ms);
    signal.addEventListener("abort", onAbort, { once: true });
  });
}

/** Forget the shown roll (logged out, no access, or another site). */
function clearRoll() {
  render(null);
  $("empty").hidden = true;
  chrome.storage.session.remove("lastRoll").catch(console.error);
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

/**
 * Streams rolls until the connection ends. Resolves to "denied" on 401/403 (the
 * caller re-checks login), "busy" on 429 (too many panels open), or "ended" after a
 * stream that delivered data. Throws when the site can't be reached or goes silent.
 */
async function streamRolls(outer) {
  // Aborted by the caller (site changed) or by the stall watchdog.
  const ctl = new AbortController();
  const stop = () => ctl.abort();
  outer.addEventListener("abort", stop, { once: true });
  let stall = setTimeout(stop, STALL_MS);
  try {
    const resp = await fetch(`${base}/api/arcane/rolls`, {
      credentials: "include",
      cache: "no-store",
      headers: { Accept: "text/event-stream" },
      signal: ctl.signal,
    });
    if (resp.status === 401 || resp.status === 403) return "denied";
    if (resp.status === 429) return "busy";
    if (!resp.ok || !resp.body) throw new Error(`HTTP ${resp.status}`);

    const reader = resp.body.pipeThrough(new TextDecoderStream()).getReader();
    const parser = new SseParser();
    for (;;) {
      const { value, done } = await reader.read();
      if (done) return "ended";
      clearTimeout(stall);
      stall = setTimeout(stop, STALL_MS);
      for (const ev of parser.push(value)) {
        if (ev.event !== "roll") continue;
        const roll = parseRoll(ev.data);
        if (!roll) continue;
        render(roll);
        chrome.storage.session.set({ lastRoll: roll }).catch(console.error);
      }
    }
  } finally {
    clearTimeout(stall);
    outer.removeEventListener("abort", stop);
  }
}

async function run() {
  let backoff = 1000;
  for (;;) {
    const signal = controller.signal;
    try {
      const me = await getMe(signal);
      if (!me.logged_in) {
        if (shown) clearRoll();
        setStatus("out", "You're not logged in.", { login: true });
        await sleep(5000, signal); // notice a login in another tab
        continue;
      }
      if (!me.has_arcane) {
        if (shown) clearRoll();
        setStatus("denied", `Logged in as ${me.username}, but this account doesn't have dice access.`);
        await sleep(30000, signal);
        continue;
      }
      setStatus("live", `Live, as ${me.username}`);
      if (!shown) render(null);
      const outcome = await streamRolls(signal);
      if (outcome === "ended") {
        // A stream that worked: reconnect promptly. (The server also ends streams
        // after a logout, and the next /me check then shows that.)
        backoff = 1000;
        setStatus("connecting", "Reconnecting…");
      } else if (outcome === "denied") {
        setStatus("connecting", "Checking login…");
      } else if (outcome === "busy") {
        setStatus("retry", "Too many dice panels open for this account. Close one to continue.");
      }
    } catch (e) {
      if (signal.aborted) continue; // the site setting changed; start over at once
      console.warn("arcane dice:", e);
      setStatus("retry", `Can't reach ${new URL(base).host}. Retrying…`);
    }
    if (signal.aborted) continue;
    // Every path waits, so a server that keeps disagreeing with itself can't spin.
    await sleep(backoff, signal);
    backoff = Math.min(backoff * 2, 30000);
  }
}

/** The chosen site, or the default if its optional permission was since removed. */
async function usableBase(value) {
  const b = normalizeBase(value);
  if (isOptional(b) && !(await chrome.permissions.contains({ origins: [originPattern(b)] }))) return DEFAULT_BASE;
  return b;
}

async function loadBase() {
  const { base: saved } = await chrome.storage.sync.get("base");
  return usableBase(saved);
}

chrome.storage.onChanged.addListener(async (changes, area) => {
  if (area !== "sync" || !("base" in changes)) return;
  base = await usableBase(changes.base.newValue);
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

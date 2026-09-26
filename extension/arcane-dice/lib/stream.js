// The website's roll stream, shared by the popup and the background script.
//
// Login is the website's own session cookie (milesstorm.bff). The browser sends it
// on these fetches because the extension has host permission for the site; the
// extension never sees or stores a password or token.

import { SseParser } from "./sse.js";
import { parseRoll } from "./roll.js";
import { BASE } from "./config.js";

/** The server sends a keep-alive every 20 s; this long without a byte means the link is dead. */
const STALL_MS = 50_000;

/** Resolves after `ms`, or as soon as `signal` aborts. */
export function sleep(ms, signal) {
  return new Promise((resolve) => {
    const onAbort = () => (clearTimeout(t), resolve());
    const t = setTimeout(() => (signal.removeEventListener("abort", onAbort), resolve()), ms);
    signal.addEventListener("abort", onAbort, { once: true });
  });
}

/** `GET /api/arcane/me`: `{logged_in, username, has_arcane}`. */
export async function getMe(signal) {
  const resp = await fetch(`${BASE}/api/arcane/me`, { credentials: "include", cache: "no-store", signal });
  if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
  return resp.json();
}

/**
 * Streams rolls until the connection ends, calling `onRoll(roll, {replay})` for each.
 * `replay` is true for the stored last roll the server sends first: shown, but not a
 * new roll. Resolves to "denied" on 401/403 (the caller re-checks login), "busy" on
 * 429 (too many streams open), or "ended" after a stream that delivered data.
 * Throws when the site can't be reached or goes silent.
 */
export async function streamRolls(outer, onRoll) {
  // Aborted by the caller or by the stall watchdog.
  const ctl = new AbortController();
  const stop = () => ctl.abort();
  outer.addEventListener("abort", stop, { once: true });
  let stall = setTimeout(stop, STALL_MS);
  try {
    const resp = await fetch(`${BASE}/api/arcane/rolls`, {
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
        if (ev.event !== "roll" && ev.event !== "replay") continue;
        const roll = parseRoll(ev.data);
        if (!roll) continue;
        roll.receivedAt = Date.now();
        onRoll(roll, { replay: ev.event === "replay" });
      }
    }
  } finally {
    clearTimeout(stall);
    outer.removeEventListener("abort", stop);
  }
}

// Toolbar popup: the logged-in user's latest dice roll, live while it's open.
//
// Login is the website's own session cookie (milesstorm.bff). The browser sends it
// on these fetches because the extension has host permission for the site; the
// extension never sees or stores a password or token. `chrome.*` is used
// throughout: Firefox provides it too, with promises.

import { canFlag, flagPayload, flagResult } from "./lib/flag.js";
import { describeRoll, timeAgo } from "./lib/roll.js";
import { AUTO_SEND_KEY, BASE, ROLL20, autoSendOn, originPattern } from "./lib/config.js";
import { getMe, sleep, streamRolls } from "./lib/stream.js";

const $ = (id) => document.getElementById(id);

const base = BASE;
let controller = new AbortController();
let shown = null;

/** Forget the shown roll (logged out, no access, or another site). */
function clearRoll() {
  render(null);
  $("empty").hidden = true;
  chrome.storage.session.remove("lastRoll").catch(console.error);
}

function setStatus(kind, text, { login = false, grant = false } = {}) {
  const el = $("status");
  el.dataset.kind = kind;
  el.textContent = text;
  $("login").hidden = !login;
  $("grant").hidden = !grant;
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
  renderFlag(roll);
}

// ── Flag as wrong roll ────────────────────────────────────────────────────────

/** The roll the flag form belongs to; the form is rebuilt only when this changes,
 * so a re-sent update of the same roll doesn't wipe what the user typed. */
let flagFor = null;
const flagged = new Set();

function renderFlag(roll) {
  const key = `${roll.rollId}/${roll.dice.length}`;
  if (key !== flagFor) {
    flagFor = key;
    $("flag-form").hidden = true;
    $("flag-invalid").hidden = true;
    $("flag-status").hidden = true;
    $("flag-inputs").replaceChildren(
      ...roll.dice.map((d, i) => {
        const input = document.createElement("input");
        input.type = "text";
        input.inputMode = "numeric";
        input.maxLength = 2;
        input.placeholder = d.value ?? "?";
        input.setAttribute("aria-label", `Die ${i + 1}, read as ${d.value ?? "unreadable"}`);
        return input;
      }),
    );
  }
  const done = flagged.has(roll.rollId);
  $("flag").hidden = !canFlag(roll) && !done;
  $("flag-open").hidden = done || !$("flag-form").hidden;
}

$("flag-open").addEventListener("click", () => {
  $("flag-form").hidden = false;
  $("flag-open").hidden = true;
  $("flag-status").hidden = true;
  $("flag-inputs").querySelector("input")?.focus();
});

$("flag-cancel").addEventListener("click", () => {
  $("flag-form").hidden = true;
  $("flag-invalid").hidden = true;
  if (shown) renderFlag(shown);
});

$("flag-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  if (!shown) return;
  const roll = shown;
  const typed = [...$("flag-inputs").querySelectorAll("input")].map((i) => i.value);
  const body = flagPayload(roll, typed);
  const formFor = flagFor;
  $("flag-invalid").hidden = body !== null;
  if (!body) return;

  $("flag-send").disabled = true;
  let status = 0;
  try {
    const resp = await fetch(`${base}/api/arcane/flag`, {
      method: "POST",
      credentials: "include",
      cache: "no-store",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    status = resp.status;
  } catch (err) {
    console.warn("arcane dice flag:", err);
  }
  $("flag-send").disabled = false;

  const result = flagResult(status);
  if (result.ok) flagged.add(roll.rollId);
  // A newer roll arrived while sending: its form is fresh, don't mark it.
  if (flagFor !== formFor) return;
  if (result.ok) $("flag-form").hidden = true;
  $("flag-status").textContent = result.text;
  $("flag-status").hidden = false;
  if (shown) renderFlag(shown);
});

async function run() {
  let backoff = 1000;
  for (;;) {
    const signal = controller.signal;
    try {
      // Firefox lets users switch site access off; without it the login cookie
      // isn't sent and requests fail.
      if (!(await chrome.permissions.contains({ origins: [originPattern(base)] }))) {
        if (shown) clearRoll();
        setStatus("out", `The extension needs permission to reach ${new URL(base).host}.`, { grant: true });
        await sleep(5000, signal);
        continue;
      }
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
      const outcome = await streamRolls(signal, (roll) => {
        render(roll);
        chrome.storage.session.set({ lastRoll: roll }).catch(console.error);
      });
      if (outcome === "ended") {
        // A stream that worked: reconnect promptly. (The server also ends streams
        // after a logout, and the next /me check then shows that.)
        backoff = 1000;
        setStatus("connecting", "Reconnecting…");
      } else if (outcome === "denied") {
        setStatus("connecting", "Checking login…");
      } else if (outcome === "busy") {
        setStatus("retry", "Too many dice popups open for this account. Close one to continue.");
      }
    } catch (e) {
      if (signal.aborted) continue; // access was just granted; start over at once
      console.warn("arcane dice:", e);
      setStatus("retry", `Can't reach ${new URL(base).host}. Retrying…`);
    }
    if (signal.aborted) continue;
    // Every path waits, so a server that keeps disagreeing with itself can't spin.
    await sleep(backoff, signal);
    backoff = Math.min(backoff * 2, 30000);
  }
}

/** Drop the current connection and start over at once. */
function restart() {
  controller.abort();
  controller = new AbortController();
}

$("login").addEventListener("click", () => chrome.tabs.create({ url: `${base}/login` }));
// permissions.request needs a user click, so it can only happen here.
$("grant").addEventListener("click", async () => {
  if (await chrome.permissions.request({ origins: [originPattern(base)] })) restart();
});

// ── Roll20 ────────────────────────────────────────────────────────────────────

const roll20 = { origins: [originPattern(ROLL20)] };

async function showRoll20() {
  $("auto-send").checked = await autoSendOn();
  // Without site access the Roll20 part of the extension doesn't run there.
  $("grant-roll20").hidden = !$("auto-send").checked || (await chrome.permissions.contains(roll20));
}

$("auto-send").addEventListener("change", async (e) => {
  await chrome.storage.local.set({ [AUTO_SEND_KEY]: e.target.checked });
  showRoll20();
});
// permissions.request needs a user click, so it can only happen here.
$("grant-roll20").addEventListener("click", async () => {
  await chrome.permissions.request(roll20);
  showRoll20();
});
showRoll20();

// Keep "12 s ago" current, and hide the flag link once the roll is too old.
setInterval(() => {
  if (!shown) return;
  $("when").textContent = `(${timeAgo(shown.ts)})`;
  renderFlag(shown);
}, 1000);

(async () => {
  const { lastRoll } = await chrome.storage.session.get("lastRoll");
  if (lastRoll) render(lastRoll);
  run();
})();

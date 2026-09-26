// Sends your rolls to Roll20's chat.
//
// Each open Roll20 game tab runs roll20.js, which connects a port here. While at
// least one is connected and auto-send is on, this holds the website's roll stream
// and hands each finished roll, as `[[13 + 3]]`, to the tab used most recently.
// The tab pings every 20 s; that activity keeps this script from being suspended
// while it waits on the stream.

import { AUTO_SEND_KEY, BASE, ROLL20, autoSendOn, originPattern } from "./lib/config.js";
import { AutoSender } from "./lib/autosend.js";
import { getMe, sleep, streamRolls } from "./lib/stream.js";

const host = new URL(BASE).host;

/** Connected Roll20 tabs: port → when that tab was last in use. */
const tabs = new Map();
let autoSend = true;
let controller = null;
/** Latest note for the tabs (problems, or "live"), repeated to tabs that connect later. */
let status = null;

const sender = new AutoSender({
  send: (text) => target()?.postMessage({ type: "send", text }),
  unreadable: (n) =>
    target()?.postMessage({
      type: "note",
      kind: "warn",
      text: n === 1 ? "1 die couldn't be read. Nudge it or adjust the camera." : `${n} dice couldn't be read. Nudge them or adjust the camera.`,
    }),
});

/** The Roll20 tab used most recently. */
function target() {
  let best = null;
  let when = -1;
  for (const [port, t] of tabs) if (t > when) [best, when] = [port, t];
  return best;
}

function setStatus(kind, text) {
  if (status?.kind === kind && status?.text === text) return;
  status = { type: "note", kind, text };
  for (const port of tabs.keys()) port.postMessage(status);
}

/** Start or stop the stream to match: needed while a Roll20 tab is open and auto-send is on. */
function update() {
  const wanted = autoSend && tabs.size > 0;
  if (wanted && !controller) {
    controller = new AbortController();
    run(controller.signal);
  } else if (!wanted && controller) {
    controller.abort();
    controller = null;
    status = null;
    sender.cancel();
  }
}

async function run(signal) {
  let backoff = 1000;
  while (!signal.aborted) {
    try {
      if (!(await chrome.permissions.contains({ origins: [originPattern(BASE)] }))) {
        setStatus("bad", `Arcane Dice needs permission to reach ${host}. Open the extension's popup to allow it.`);
        await sleep(5000, signal);
        continue;
      }
      const me = await getMe(signal);
      if (!me.logged_in) {
        setStatus("bad", `Arcane Dice: log in on ${host} to send your rolls to this chat.`);
        await sleep(5000, signal);
        continue;
      }
      if (!me.has_arcane) {
        setStatus("bad", `Arcane Dice: ${me.username} doesn't have dice access.`);
        await sleep(30000, signal);
        continue;
      }
      setStatus("live", `Arcane Dice: your rolls go to this chat (as ${me.username}).`);
      const outcome = await streamRolls(signal, (roll, how) => sender.roll(roll, how));
      if (outcome === "ended") backoff = 1000;
      else if (outcome === "busy") setStatus("bad", "Arcane Dice: too many dice windows are open for this account.");
    } catch (e) {
      if (signal.aborted) break;
      console.warn("arcane dice:", e);
      setStatus("bad", `Arcane Dice can't reach ${host}. Retrying…`);
    }
    await sleep(backoff, signal);
    backoff = Math.min(backoff * 2, 30000);
  }
}

chrome.runtime.onConnect.addListener((port) => {
  // Only our own content script, on a Roll20 page.
  if (port.name !== "roll20" || port.sender?.id !== chrome.runtime.id || !port.sender?.url?.startsWith(`${ROLL20}/`)) {
    port.disconnect();
    return;
  }
  tabs.set(port, Date.now());
  port.onMessage.addListener((msg) => {
    if (msg?.type === "active") tabs.set(port, Date.now());
    // "ping" needs no answer: receiving it is what keeps this script awake.
  });
  port.onDisconnect.addListener(() => {
    tabs.delete(port);
    update();
  });
  if (status && autoSend) port.postMessage(status);
  update();
});

chrome.storage.onChanged.addListener((changes, area) => {
  if (area !== "local" || !(AUTO_SEND_KEY in changes)) return;
  autoSend = changes[AUTO_SEND_KEY].newValue !== false;
  for (const port of tabs.keys()) port.postMessage({ type: "note", kind: autoSend ? "live" : "off", text: autoSend ? "Arcane Dice: sending your rolls to this chat again." : "Arcane Dice: stopped sending rolls to this chat." });
  update();
});

autoSendOn().then((on) => {
  autoSend = on;
  update();
});

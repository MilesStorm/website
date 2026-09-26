// Runs on Roll20 game pages: types rolls from background.js into the chat.
//
// A content script can't import modules, so this file is self-contained. It only
// ever types text matching CHAT_TEXT (lib/chat.js; a test keeps the two equal), and
// puts back anything the player had typed in the chat box.

(() => {
  const CHAT_TEXT = /^\[\[\d{1,2}( \+ \d{1,2}){0,19}\]\]$/;
  let port = null;
  /** Left for another page but kept in the back/forward cache: not a game tab now. */
  let hidden = false;

  function connect() {
    if (!chrome.runtime?.id) return; // the extension was removed or reloaded; this copy is dead
    try {
      port = chrome.runtime.connect({ name: "roll20" });
    } catch {
      port = null; // the extension was removed or reloaded; this copy is dead
      return;
    }
    port.onMessage.addListener(onMessage);
    port.onDisconnect.addListener(() => {
      port = null;
      // The background script was suspended or restarted: reconnect (which wakes it).
      setTimeout(() => port || hidden || connect(), 1000);
    });
    if (document.visibilityState === "visible") port.postMessage({ type: "active" });
  }

  function onMessage(msg) {
    if (msg?.type === "send" && typeof msg.text === "string" && CHAT_TEXT.test(msg.text)) {
      if (!sendToChat(msg.text)) note("bad", "Arcane Dice couldn't find the Roll20 chat box. Is the chat tab open?");
    } else if (msg?.type === "note" && typeof msg.text === "string") {
      note(msg.kind, msg.text);
    }
  }

  /** Types `text` into Roll20's chat and sends it, keeping the player's draft. */
  function sendToChat(text) {
    const box = document.querySelector("#textchat-input textarea");
    const button = document.querySelector("#chatSendBtn") ?? document.querySelector("#textchat-input button");
    if (!box || !button) return false;
    const draft = box.value;
    box.value = text;
    box.dispatchEvent(new Event("input", { bubbles: true }));
    button.click();
    if (draft) {
      // Put the draft back once Roll20 has taken the roll out of the box.
      let tries = 0;
      const restore = () => {
        if (box.value === "") box.value = draft;
        else if (box.value === text && ++tries < 20) setTimeout(restore, 50);
      };
      restore();
    }
    return true;
  }

  // ── Notes: a small box in the top-right corner, gone after a few seconds ─────

  let noteBox = null;
  let noteTimer = 0;

  function note(kind, text) {
    if (!noteBox) {
      const holder = document.createElement("div");
      const shadow = holder.attachShadow({ mode: "closed" });
      const style = document.createElement("style");
      style.textContent = `
        div { position: fixed; top: 12px; right: 12px; z-index: 2147483647; max-width: 320px;
              padding: 10px 14px; border-radius: 8px; font: 14px/1.4 system-ui, sans-serif;
              background: #17171b; color: #ececf1; border: 1px solid #3a3a44;
              box-shadow: 0 4px 16px rgb(0 0 0 / 0.3); }
        div[data-kind="live"] { border-left: 4px solid #34d399; }
        div[data-kind="warn"] { border-left: 4px solid #fbbf24; }
        div[data-kind="bad"] { border-left: 4px solid #fb923c; }
        div[hidden] { display: none; }`;
      noteBox = document.createElement("div");
      noteBox.setAttribute("role", "status");
      shadow.append(style, noteBox);
      document.documentElement.append(holder);
    }
    noteBox.dataset.kind = kind;
    noteBox.textContent = text;
    noteBox.hidden = false;
    clearTimeout(noteTimer);
    noteTimer = setTimeout(() => (noteBox.hidden = true), kind === "bad" ? 10000 : 5000);
  }

  // The most recently used Roll20 tab gets the rolls.
  const active = () => document.visibilityState === "visible" && port?.postMessage({ type: "active" });
  document.addEventListener("visibilitychange", active);
  window.addEventListener("focus", active);
  // Keeps the background script awake while this tab is open.
  setInterval(() => port?.postMessage({ type: "ping" }), 20000);

  // Firefox keeps a page left by navigation alive in its back/forward cache, port
  // included; let go of it so rolls don't go to a page nobody sees.
  window.addEventListener("pagehide", () => {
    hidden = true;
    port?.disconnect();
    port = null;
  });
  window.addEventListener("pageshow", (e) => {
    hidden = false;
    if (e.persisted && !port) connect();
  });

  connect();
})();

// The one site the extension talks to. HTTPS only: the login cookie is Secure and
// nothing should travel in plain text.
export const BASE = "https://milesstorm.com";

/**
 * Host-permission pattern for a site. No port: Firefox ignores patterns that
 * include one, so the permission would silently never apply.
 */
export function originPattern(base) {
  const u = new URL(base);
  return `${u.protocol}//${u.hostname}/*`;
}

/** The Roll20 game page, where rolls are typed into the chat (content script). */
export const ROLL20 = "https://app.roll20.net";

/** chrome.storage.local key: whether rolls go to Roll20's chat (default on). */
export const AUTO_SEND_KEY = "autoSend";

/** Whether auto-send is on; unset counts as on. */
export async function autoSendOn() {
  const { [AUTO_SEND_KEY]: on } = await chrome.storage.local.get(AUTO_SEND_KEY);
  return on !== false;
}

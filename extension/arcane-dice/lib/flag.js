// "Flag as wrong roll": the website keeps each user's latest roll picture for 10
// minutes (services/frontend capture.rs); flagging within that time saves it for
// training, with the user's optional corrections per die.

export const FLAG_WINDOW_MS = 10 * 60 * 1000;

/**
 * Whether the roll can still be flagged (the server's copy lasts 10 minutes).
 * Timed from when this browser received it (`receivedAt`), not the server's `ts`,
 * so a wrong computer clock doesn't shift the window.
 */
export function canFlag(roll, now = Date.now()) {
  return roll !== null && now - (roll.receivedAt ?? roll.ts) < FLAG_WINDOW_MS;
}

/** A correction as typed: "" means "left empty"; otherwise must be a die face 0–20. */
export function isFace(text) {
  return /^(0|[1-9]|1[0-9]|20)$/.test(text);
}

/**
 * Request body for POST /api/arcane/flag, or null when a box holds something that
 * isn't a die face. One value per die, left to right; empty boxes become null.
 */
export function flagPayload(roll, typed) {
  if (typed.length !== roll.dice.length) return null;
  const values = [];
  for (const raw of typed) {
    const t = raw.trim();
    if (t === "") values.push(null);
    else if (isFace(t)) values.push(t);
    else return null;
  }
  return { roll_id: roll.rollId, values };
}

/** Plain-language result for the server's reply. */
export function flagResult(status) {
  if (status === 200) return { ok: true, text: "Thanks! Sent for review." };
  if (status === 410) {
    return { ok: false, text: "That roll can't be flagged any more. Only your latest roll can be, for 10 minutes." };
  }
  if (status === 429) return { ok: false, text: "You've flagged a lot of rolls this hour. Try again later." };
  if (status === 401 || status === 403) return { ok: false, text: "Log in on the website to flag rolls." };
  if (status === 503) return { ok: false, text: "Flagging isn't available right now." };
  return { ok: false, text: "Couldn't send. Try again." };
}

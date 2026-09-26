// Rolls as Roll20 chat text. `[[13 + 3]]` is an inline roll: Roll20 works out the sum
// and shows it highlighted (16), like a roll made in the game.

import { isFace } from "./flag.js";

/** Most dice in one message; more than this is surely a misread. */
export const MAX_DICE = 20;

/** Chat text for a fully read roll, or null (a die unreadable, or no dice). */
export function chatText(roll) {
  if (!roll.complete || roll.dice.length === 0 || roll.dice.length > MAX_DICE) return null;
  const faces = [];
  for (const d of roll.dice) {
    if (typeof d.value !== "string" || !isFace(d.value)) return null;
    faces.push(d.value === "0" ? "10" : d.value); // a d10's 0 counts as 10, like the server's total
  }
  return `[[${faces.join(" + ")}]]`;
}

/** The only text the content script will ever type into the chat. */
export const CHAT_TEXT = /^\[\[\d{1,2}( \+ \d{1,2}){0,19}\]\]$/;

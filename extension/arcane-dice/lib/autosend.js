// Which rolls go to the chat, and when.
//
// The dice reader may correct itself a moment after reading a roll (same `rollId`,
// new values), so a roll is sent only once it has been fully read and unchanged for
// `SEND_DELAY_MS`, and each `rollId` at most once. A later correction of a sent roll
// is ignored. The roll the server replays on connect was made earlier: it is never
// sent. A new roll replaces one still waiting (the dice moved again).

import { chatText } from "./chat.js";

export const SEND_DELAY_MS = 1000;
/** Roll ids remembered as done; old ones are forgotten first. */
const REMEMBER = 100;

export class AutoSender {
  #done = new Set();
  #waiting = null; // { rollId, timer }
  #send;
  #unreadable;
  #delay;
  #timers;

  /**
   * `send(text)` posts chat text; `unreadable(count)` reports a roll that stayed
   * incomplete for the delay (once per roll).
   */
  constructor({ send, unreadable, delayMs = SEND_DELAY_MS, timers = globalThis }) {
    this.#send = send;
    this.#unreadable = unreadable;
    this.#delay = delayMs;
    this.#timers = timers;
  }

  roll(roll, { replay = false } = {}) {
    if (replay) {
      if (this.#waiting?.rollId === roll.rollId) this.#cancel();
      this.#finish(roll.rollId);
      return;
    }
    if (this.#done.has(roll.rollId)) return;
    this.#cancel();
    const text = chatText(roll);
    const timer = this.#timers.setTimeout(() => {
      this.#waiting = null;
      if (text !== null) {
        this.#finish(roll.rollId);
        this.#send(text);
      } else {
        // Keep waiting for a readable update of this roll, but say so only once.
        const key = `unreadable:${roll.rollId}`;
        if (this.#done.has(key)) return;
        this.#finish(key);
        this.#unreadable(roll.dice.filter((d) => d.value === null).length);
      }
    }, this.#delay);
    this.#waiting = { rollId: roll.rollId, timer };
  }

  /** Drop anything waiting (auto-send switched off, or the stream restarts). */
  cancel() {
    this.#cancel();
  }

  #cancel() {
    if (this.#waiting) this.#timers.clearTimeout(this.#waiting.timer);
    this.#waiting = null;
  }

  #finish(key) {
    this.#done.add(key);
    if (this.#done.size > REMEMBER) this.#done.delete(this.#done.values().next().value);
  }
}

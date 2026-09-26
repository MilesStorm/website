import { test } from "node:test";
import assert from "node:assert/strict";
import { AutoSender, SEND_DELAY_MS } from "../lib/autosend.js";

/** Fake clock: timers only fire when `tick` moves time past them. */
function setup() {
  let now = 0;
  let next = 1;
  const pending = new Map();
  const timers = {
    setTimeout: (fn, ms) => (pending.set(next, { fn, at: now + ms }), next++),
    clearTimeout: (id) => pending.delete(id),
  };
  const sent = [];
  const notes = [];
  const sender = new AutoSender({ send: (t) => sent.push(t), unreadable: (n) => notes.push(n), timers });
  const tick = (ms) => {
    now += ms;
    for (const [id, t] of [...pending]) if (t.at <= now) (pending.delete(id), t.fn());
  };
  return { sender, sent, notes, tick };
}

const roll = (rollId, ...values) => ({
  rollId,
  dice: values.map((value) => ({ value })),
  complete: values.every((v) => v !== null),
});

test("a read roll is sent after the delay, once", () => {
  const { sender, sent, tick } = setup();
  sender.roll(roll("a", "13", "3"));
  tick(SEND_DELAY_MS - 1);
  assert.deepEqual(sent, []);
  tick(1);
  assert.deepEqual(sent, ["[[13 + 3]]"]);
  sender.roll(roll("a", "13", "3"));
  sender.roll(roll("a", "13", "9")); // a correction after sending is ignored
  tick(SEND_DELAY_MS * 5);
  assert.deepEqual(sent, ["[[13 + 3]]"]);
});

test("a correction within the delay is what gets sent", () => {
  const { sender, sent, tick } = setup();
  sender.roll(roll("a", "13", "6"));
  tick(SEND_DELAY_MS / 2);
  sender.roll(roll("a", "13", "9"));
  tick(SEND_DELAY_MS / 2);
  assert.deepEqual(sent, []);
  tick(SEND_DELAY_MS / 2);
  assert.deepEqual(sent, ["[[13 + 9]]"]);
});

test("the replayed last roll is never sent, nor its later updates", () => {
  const { sender, sent, tick } = setup();
  sender.roll(roll("old", "20"), { replay: true });
  sender.roll(roll("old", "19"));
  tick(SEND_DELAY_MS * 2);
  assert.deepEqual(sent, []);
  sender.roll(roll("new", "4"));
  tick(SEND_DELAY_MS);
  assert.deepEqual(sent, ["[[4]]"]);
});

test("an unreadable roll waits, says so once, then sends when read", () => {
  const { sender, sent, notes, tick } = setup();
  sender.roll(roll("a", "13", null));
  tick(SEND_DELAY_MS);
  sender.roll(roll("a", null, null));
  tick(SEND_DELAY_MS);
  assert.deepEqual(sent, []);
  assert.deepEqual(notes, [1]);
  sender.roll(roll("a", "13", "3"));
  tick(SEND_DELAY_MS);
  assert.deepEqual(sent, ["[[13 + 3]]"]);
});

test("a new roll replaces one still waiting", () => {
  const { sender, sent, tick } = setup();
  sender.roll(roll("a", "1"));
  tick(SEND_DELAY_MS / 2);
  sender.roll(roll("b", "2"));
  tick(SEND_DELAY_MS * 2);
  assert.deepEqual(sent, ["[[2]]"]);
});

test("cancel drops a waiting roll", () => {
  const { sender, sent, tick } = setup();
  sender.roll(roll("a", "5"));
  sender.cancel();
  tick(SEND_DELAY_MS * 2);
  assert.deepEqual(sent, []);
});

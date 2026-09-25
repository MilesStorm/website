import { test } from "node:test";
import assert from "node:assert/strict";
import { parseRoll, describeRoll, timeAgo } from "../lib/roll.js";

// Shape produced by ai_pipeline's RollEvent (services/ai_pipeline/src/roll.rs).
const twoDice = JSON.stringify({
  type: "roll",
  roll_id: "18f3a-0badf00d",
  dice: [
    { value: "5", conf: 0.97, box: [0.1, 0.1, 0.2, 0.2] },
    { value: "12", conf: 0.91, box: [0.5, 0.1, 0.6, 0.2] },
  ],
  total: 17,
  complete: true,
  ts: 1000,
});

test("parses a complete roll", () => {
  const r = parseRoll(twoDice);
  assert.equal(r.rollId, "18f3a-0badf00d");
  assert.deepEqual(r.dice.map((d) => d.value), ["5", "12"]);
  assert.equal(r.total, 17);
  assert.equal(r.complete, true);
  assert.deepEqual(describeRoll(r), {
    dice: [
      { label: "5", readable: true },
      { label: "12", readable: true },
    ],
    total: "17",
    unreadable: 0,
  });
});

test("an unreadable die shows ? and no total", () => {
  const r = parseRoll(
    JSON.stringify({ type: "roll", roll_id: "x", dice: [{ value: "4" }, { value: null }], total: null, complete: false, ts: 1 }),
  );
  assert.equal(r.complete, false);
  const d = describeRoll(r);
  assert.deepEqual(d.dice.map((x) => x.label), ["4", "?"]);
  assert.equal(d.total, null);
  assert.equal(d.unreadable, 1);
});

test("rejects non-roll messages and junk", () => {
  assert.equal(parseRoll('{"type":"frame","detections":[]}'), null);
  assert.equal(parseRoll('{"type":"roll"}'), null);
  assert.equal(parseRoll("not json"), null);
  assert.equal(parseRoll("null"), null);
});

test("a non-string value is treated as unreadable, never shown as text", () => {
  const r = parseRoll(JSON.stringify({ type: "roll", roll_id: "x", dice: [{ value: 7 }], total: 7, complete: true, ts: 1 }));
  assert.equal(r.dice[0].value, null);
  assert.equal(r.complete, false);
  assert.equal(describeRoll(r).total, null);
});

test("timeAgo buckets", () => {
  assert.equal(timeAgo(0, 2000), "just now");
  assert.equal(timeAgo(0, 42_000), "42 s ago");
  assert.equal(timeAgo(0, 180_000), "3 min ago");
  assert.equal(timeAgo(0, 7_200_000), "2 h ago");
  assert.equal(timeAgo(10_000, 0), "just now");
});

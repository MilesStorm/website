import { test } from "node:test";
import assert from "node:assert/strict";
import { canFlag, flagPayload, flagResult, isFace } from "../lib/flag.js";

const roll = { rollId: "18f3a-0badf00d", dice: [{ value: "5" }, { value: null }], ts: 1_000_000 };

test("rolls can be flagged for 10 minutes", () => {
  assert.equal(canFlag(roll, 1_000_000 + 9 * 60_000), true);
  assert.equal(canFlag(roll, 1_000_000 + 10 * 60_000), false);
  assert.equal(canFlag(null), false);
});

test("die faces 0-20 only", () => {
  for (const ok of ["0", "1", "9", "10", "20"]) assert.ok(isFace(ok), ok);
  for (const bad of ["21", "05", "-1", "a", " ", "1.5", "+3"]) assert.ok(!isFace(bad), bad);
});

test("payload: one value per die, empty boxes are null", () => {
  assert.deepEqual(flagPayload(roll, [" 12 ", ""]), { roll_id: "18f3a-0badf00d", values: ["12", null] });
  assert.deepEqual(flagPayload(roll, ["", ""]), { roll_id: "18f3a-0badf00d", values: [null, null] });
});

test("payload refused for bad input", () => {
  assert.equal(flagPayload(roll, ["99", ""]), null);
  assert.equal(flagPayload(roll, ["5"]), null);
});

test("server replies become plain messages", () => {
  assert.equal(flagResult(200).ok, true);
  assert.match(flagResult(410).text, /too old/);
  assert.match(flagResult(429).text, /this hour/);
  assert.match(flagResult(500).text, /Try again/);
});

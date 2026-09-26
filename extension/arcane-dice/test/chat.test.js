import { test } from "node:test";
import assert from "node:assert/strict";
import { CHAT_TEXT, MAX_DICE, chatText } from "../lib/chat.js";

const roll = (...values) => ({
  rollId: "r",
  dice: values.map((value) => ({ value, conf: 0.9 })),
  complete: values.every((v) => v !== null),
});

test("a read roll becomes an inline roll of its faces", () => {
  assert.equal(chatText(roll("13", "3")), "[[13 + 3]]");
  assert.equal(chatText(roll("20")), "[[20]]");
});

test("a d10's 0 counts as 10", () => {
  assert.equal(chatText(roll("0", "4")), "[[10 + 4]]");
});

test("nothing is sent with an unreadable die, or no dice", () => {
  assert.equal(chatText(roll("13", null)), null);
  assert.equal(chatText(roll()), null);
});

test("anything that isn't a die face is refused, never typed", () => {
  assert.equal(chatText({ rollId: "r", complete: true, dice: [{ value: "21" }] }), null);
  assert.equal(chatText({ rollId: "r", complete: true, dice: [{ value: "1]] /roll 1d100 [[1" }] }), null);
  assert.equal(chatText({ rollId: "r", complete: true, dice: [{ value: 5 }] }), null);
});

test("too many dice is a misread", () => {
  assert.equal(chatText(roll(...Array(MAX_DICE).fill("1"))), `[[${Array(MAX_DICE).fill("1").join(" + ")}]]`);
  assert.equal(chatText(roll(...Array(MAX_DICE + 1).fill("1"))), null);
});

test("CHAT_TEXT matches what chatText makes and nothing looser", () => {
  for (const r of [roll("13", "3"), roll("20"), roll(...Array(MAX_DICE).fill("20"))]) assert.ok(CHAT_TEXT.test(chatText(r)));
  for (const bad of ["[[13 + 3]] hi", "/roll 1d20", "[[13+3]]", "[[100]]", "[[]]", "[[1 + 2]]\n[[3]]"]) {
    assert.ok(!CHAT_TEXT.test(bad), bad);
  }
});

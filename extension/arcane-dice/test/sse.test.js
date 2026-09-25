import { test } from "node:test";
import assert from "node:assert/strict";
import { SseParser } from "../lib/sse.js";

function feed(chunks) {
  const p = new SseParser();
  return chunks.flatMap((c) => p.push(c));
}

test("a complete event in one chunk", () => {
  assert.deepEqual(feed(["event: roll\ndata: {\"a\":1}\n\n"]), [{ event: "roll", data: '{"a":1}' }]);
});

test("event split mid-line across chunks", () => {
  assert.deepEqual(feed(["event: ro", "ll\nda", "ta: hel", "lo\n", "\n"]), [{ event: "roll", data: "hello" }]);
});

test("CRLF line endings, including a CRLF split between chunks", () => {
  assert.deepEqual(feed(["data: a\r", "\n\r\n", "data: b\r\n\r\n"]), [
    { event: "message", data: "a" },
    { event: "message", data: "b" },
  ]);
});

test("bare CR line endings", () => {
  assert.deepEqual(feed(["data: x\r\rdata: y\r\r"]), [
    { event: "message", data: "x" },
    { event: "message", data: "y" },
  ]);
});

test("multi-line data is joined with newlines", () => {
  assert.deepEqual(feed(["data: one\ndata: two\ndata:three\n\n"]), [{ event: "message", data: "one\ntwo\nthree" }]);
});

test("comments (keep-alives) are ignored and don't dispatch", () => {
  assert.deepEqual(feed([":\n\n", ": keep-alive\n\n", "data: z\n: mid\n\n"]), [{ event: "message", data: "z" }]);
});

test("event name resets after each event", () => {
  assert.deepEqual(feed(["event: roll\ndata: 1\n\ndata: 2\n\n"]), [
    { event: "roll", data: "1" },
    { event: "message", data: "2" },
  ]);
});

test("an event without a closing blank line is not emitted yet", () => {
  const p = new SseParser();
  assert.deepEqual(p.push("data: waiting\n"), []);
  assert.deepEqual(p.push("\n"), [{ event: "message", data: "waiting" }]);
});

test("an empty chunk between CR and LF doesn't split the CRLF", () => {
  assert.deepEqual(feed(["data: a\r", "", "\n\r\n"]), [{ event: "message", data: "a" }]);
});

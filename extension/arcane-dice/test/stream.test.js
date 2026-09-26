import { test } from "node:test";
import assert from "node:assert/strict";
import { streamRolls } from "../lib/stream.js";

const event = (name, id, value) =>
  `event: ${name}\ndata: ${JSON.stringify({ type: "roll", roll_id: id, dice: [{ value, conf: 1 }], total: Number(value), complete: true, ts: 1 })}\n\n`;

function serve(status, chunks = []) {
  globalThis.fetch = async () =>
    new Response(
      new ReadableStream({
        start(c) {
          for (const ch of chunks) c.enqueue(new TextEncoder().encode(ch));
          c.close();
        },
      }),
      { status },
    );
}

test("the connect-time replay is flagged; live rolls are not", async () => {
  serve(200, [event("replay", "old", "7"), ": keep-alive\n\n", event("roll", "new", "12"), event("other", "x", "1")]);
  const got = [];
  const outcome = await streamRolls(new AbortController().signal, (roll, how) => got.push([roll.rollId, how.replay]));
  assert.equal(outcome, "ended");
  assert.deepEqual(got, [["old", true], ["new", false]]);
});

test("login problems and a full house are reported, not thrown", async () => {
  for (const [status, outcome] of [[401, "denied"], [403, "denied"], [429, "busy"]]) {
    serve(status);
    assert.equal(await streamRolls(new AbortController().signal, () => {}), outcome);
  }
});

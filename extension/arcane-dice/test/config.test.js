import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { BASE, ROLL20, originPattern } from "../lib/config.js";
import { CHAT_TEXT } from "../lib/chat.js";

const manifest = JSON.parse(readFileSync(new URL("../manifest.json", import.meta.url)));

test("the extension only reaches its site and Roll20, over HTTPS", () => {
  assert.ok(BASE.startsWith("https://"));
  assert.ok(ROLL20.startsWith("https://"));
  assert.deepEqual(manifest.host_permissions, [originPattern(BASE), originPattern(ROLL20)]);
  for (const cs of manifest.content_scripts) {
    for (const m of cs.matches) assert.ok(m.startsWith(`${ROLL20}/`), m);
  }
  assert.equal(manifest.optional_host_permissions, undefined);
});

test("host patterns never carry a port (Firefox ignores those)", () => {
  assert.equal(originPattern("https://milesstorm.com"), "https://milesstorm.com/*");
  assert.equal(originPattern("https://example.com:8443"), "https://example.com/*");
});

test("no plain-http URL anywhere in the shipped files", () => {
  const files = ["manifest.json", "popup.html", "popup.js", "background.js", "roll20.js"];
  for (const f of ["config", "roll", "sse", "stream", "chat", "autosend", "flag"]) files.push(`lib/${f}.js`);
  for (const f of files) {
    const text = readFileSync(new URL(`../${f}`, import.meta.url), "utf8");
    assert.ok(!/http:\/\//.test(text), `${f} contains http://`);
  }
});

test("the Roll20 page script accepts exactly the chat text the extension makes", () => {
  const text = readFileSync(new URL("../roll20.js", import.meta.url), "utf8");
  assert.ok(text.includes(`const CHAT_TEXT = ${CHAT_TEXT};`), "roll20.js CHAT_TEXT differs from lib/chat.js");
});

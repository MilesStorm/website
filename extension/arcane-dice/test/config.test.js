import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { BASE, originPattern } from "../lib/config.js";

const manifest = JSON.parse(readFileSync(new URL("../manifest.json", import.meta.url)));

test("the extension only talks to its site, over HTTPS", () => {
  assert.ok(BASE.startsWith("https://"));
  assert.deepEqual(manifest.host_permissions, [originPattern(BASE)]);
  assert.equal(manifest.optional_host_permissions, undefined);
});

test("host patterns never carry a port (Firefox ignores those)", () => {
  assert.equal(originPattern("https://milesstorm.com"), "https://milesstorm.com/*");
  assert.equal(originPattern("https://example.com:8443"), "https://example.com/*");
});

test("no plain-http URL anywhere in the shipped files", () => {
  for (const f of ["manifest.json", "popup.html", "popup.js", "lib/config.js", "lib/roll.js", "lib/sse.js"]) {
    const text = readFileSync(new URL(`../${f}`, import.meta.url), "utf8");
    assert.ok(!/http:\/\//.test(text), `${f} contains http://`);
  }
});

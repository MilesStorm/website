import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { ALLOWED_BASES, DEFAULT_BASE, normalizeBase } from "../lib/config.js";

const manifest = JSON.parse(readFileSync(new URL("../manifest.json", import.meta.url)));

test("every allowed site has a host permission, and vice versa", () => {
  assert.deepEqual(
    ALLOWED_BASES.map((b) => `${b}/*`).sort(),
    [...manifest.host_permissions].sort(),
  );
});

test("unknown or missing settings fall back to the default site", () => {
  assert.equal(normalizeBase(undefined), DEFAULT_BASE);
  assert.equal(normalizeBase("https://evil.example"), DEFAULT_BASE);
  assert.equal(normalizeBase("http://localhost:8080"), "http://localhost:8080");
});

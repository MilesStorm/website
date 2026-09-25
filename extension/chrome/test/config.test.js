import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { ALLOWED_BASES, DEFAULT_BASE, isOptional, normalizeBase, originPattern } from "../lib/config.js";

const manifest = JSON.parse(readFileSync(new URL("../manifest.json", import.meta.url)));

test("only the default site is a granted host permission", () => {
  assert.deepEqual(manifest.host_permissions, [originPattern(DEFAULT_BASE)]);
});

test("every other allowed site is an optional permission, and vice versa", () => {
  assert.deepEqual(
    ALLOWED_BASES.filter(isOptional).map(originPattern).sort(),
    [...manifest.optional_host_permissions].sort(),
  );
});

test("unknown or missing settings fall back to the default site", () => {
  assert.equal(normalizeBase(undefined), DEFAULT_BASE);
  assert.equal(normalizeBase("https://evil.example"), DEFAULT_BASE);
  assert.equal(normalizeBase("http://localhost:8080"), "http://localhost:8080");
});

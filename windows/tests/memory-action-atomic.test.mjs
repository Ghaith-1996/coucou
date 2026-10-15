// Native behavioral schedules live in memory.rs. This runnable source guard
// checks that production wiring keeps cancellation outside the whole save.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
const source = readFileSync(new URL("../src-tauri/src/assistant/tools.rs", import.meta.url), "utf8")
  .split("#[cfg(test)]")[0].replace(/\/\/.*$/gm, "");

test("confirmed memory mutation and persistence share the native cancellation boundary", () => {
  assert.match(source, /let saved = scope\.run_if_active\(\|\| \{\s*crate::memory::with\(\|m\| m\.add\(&a\.text, crate::memory::Source::Chat, crate::memory::now\(\)\)\)\s*\}\)\.unwrap_or_else\(\|\| Err\("The chat was stopped\."\.into\(\)\)\)/);
  assert.doesNotMatch(source, /crate::memory::with\(\|m\| \{\s*if !scope\./);
});
